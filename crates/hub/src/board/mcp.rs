//! The board's MCP face: `board_*` tools on the hub's `/mcp`, one per protocol verb,
//! for agents on any machine. The command line (`cli.rs`) and these tools open the
//! same store, so an agent on the hub's host and one across the world see one board.
//!
//! Identity is the hub's: the actor is the authenticated subject, a person only when
//! the config says so (`grants[].human`, or the owner). A fence may be omitted on a
//! held ticket; the hub then supplies the one it handed that subject at claim or
//! heartbeat, the way the command line's claim cache does. The store still checks it,
//! so a lost lease is refused either way. Every call lands on the audit chain under
//! the project's name.
//!
//! Two kinds of board answer to a project name. A configured board (`boards` in the hub
//! config) is a directory on this host that grants open to several accounts. An account
//! board (D23, packet 15) is the hosted tier's: `board_bind` makes one for the calling
//! account under `account_boards.dir`, reached by that account alone, counted against the
//! free limit. On either, a caller may name its seat (`X-Devagotchi-Seat: claude`): the
//! account's own agent on that machine. On the account's own board the seat is the
//! actor outright; on a shared board it is `<account>/<seat>`, so no account can pose
//! as another's seat. A seat is never a person.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use rmcp::{
    Json, RoleServer, handler::server::wrapper::Parameters, service::RequestContext, tool,
    tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    Actor, Board, ChainStatus, Dep, DepKind, Event, Filter, Lease, NewTicket, NextFilter, Note,
    Patch, Prime, Ticket, TicketView, approval::ApprovalStatus, face::ME, open_project,
};
use crate::{
    audit::AuditEntry,
    auth::Subject,
    config::{DEFAULT_FREE_LIMIT, HubConfig, valid_name},
    hub::Hub,
};

/// The header a caller names its seat in: the account's own agent on that machine.
pub const SEAT_HEADER: &str = "x-devagotchi-seat";

/// The boards a hub serves, opened on first use and kept open; plus the last fence
/// handed to each subject per ticket, so a caller may omit it.
pub struct Boards {
    dirs: BTreeMap<String, PathBuf>,
    accounts: Option<Accounts>,
    /// The hub's address, written into a new board's getting-started steps.
    public_url: String,
    open: Mutex<BTreeMap<String, Arc<Board>>>,
    fences: Mutex<HashMap<(String, String, String), i64>>,
    /// Told of every change any open board commits; the lease reaper waits on it.
    activity: Arc<tokio::sync::Notify>,
}

/// Where account boards live and who may have how many.
struct Accounts {
    dir: PathBuf,
    free_limit: usize,
    unlimited: HashSet<String>,
    /// The hub's owner, who has no limit; in hub mode it can be learned after start.
    owner: std::sync::Arc<std::sync::OnceLock<String>>,
}

/// What `board_bind` says.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Bound {
    pub project: String,
    /// False when the board already existed: binding is idempotent.
    pub created: bool,
    /// Every board this account may work on now, account boards first.
    pub boards: Vec<String>,
    /// How many account boards this account may hold here; absent for no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Which kind of board answered a project name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The calling account's own board.
    Account,
    /// A board the caller's org owns: every member of the org works it (B13).
    Org,
    /// A board from the hub config, shared by grant.
    Shared,
}

impl Boards {
    pub fn from_config(config: &HubConfig, state_dir: &Path) -> Self {
        let accounts = config.account_boards.as_ref().map(|a| {
            let unlimited: HashSet<String> = a.unlimited.iter().cloned().collect();
            Accounts {
                owner: config.owner_handle(),
                dir: a
                    .dir
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| state_dir.join("account-boards")),
                free_limit: a.free_limit.unwrap_or(DEFAULT_FREE_LIMIT) as usize,
                unlimited,
            }
        });
        Self {
            dirs: config
                .boards
                .iter()
                .map(|b| (b.project.clone(), PathBuf::from(&b.dir)))
                .collect(),
            accounts,
            public_url: config.public_url.clone(),
            open: Mutex::new(BTreeMap::new()),
            fences: Mutex::new(HashMap::new()),
            activity: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.accounts.is_none()
    }

    /// Whether this hub hosts account boards at all.
    pub fn hosts_accounts(&self) -> bool {
        self.accounts.is_some()
    }

    fn account_file(accounts: &Accounts, subject: &str, project: &str) -> PathBuf {
        accounts
            .dir
            .join(subject)
            .join(format!("{project}.sqlite3"))
    }

    /// The account boards `subject` holds here, by name, sorted.
    pub fn account_boards(&self, subject: &str) -> Vec<String> {
        let Some(accounts) = &self.accounts else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(accounts.dir.join(subject)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let project = name.strip_suffix(".sqlite3")?;
                (valid_name(project) && e.path().is_file()).then(|| project.to_string())
            })
            .collect();
        names.sort();
        names
    }

    /// Who owns the boards `subject` makes and finds: its org when the identity provider
    /// named one (B13), otherwise the account itself. Org ids and account ids never
    /// collide on disk: an org's directory is `org-<id>`.
    pub fn owner_of(subject: &Subject) -> String {
        match &subject.org {
            Some(org) => format!("org-{org}"),
            None => subject.id.clone(),
        }
    }

    /// The account's own board named `project`, if it has one.
    pub fn open_account(&self, subject: &str, project: &str) -> BridgeResult<Option<Arc<Board>>> {
        let Some(accounts) = &self.accounts else {
            return Ok(None);
        };
        if !valid_name(project) {
            return Ok(None);
        }
        let key = format!("{subject}/{project}");
        let mut open = self.open.lock().expect("boards lock");
        if let Some(board) = open.get(&key) {
            return Ok(Some(board.clone()));
        }
        let file = Self::account_file(accounts, subject, project);
        if !file.is_file() {
            return Ok(None);
        }
        let board = Arc::new(Board::open(&file, project)?.reporting_to(self.activity.clone()));
        open.insert(key, board.clone());
        Ok(Some(board))
    }

    /// Make (or find) the account's board for `project`. `shared` is every configured
    /// board the account already reaches: the same name cannot mean two boards.
    pub fn bind(&self, subject: &str, project: &str, shared: &[String]) -> BridgeResult<Bound> {
        self.bind_for(subject, &[subject], project, shared)
    }

    /// Make (or find) `owner`'s board for `project` (an account, or `org-<id>`); the free
    /// limit counts `owner`'s boards and is lifted when any of `who` (the caller, its org)
    /// is unlimited.
    pub fn bind_for(
        &self,
        subject: &str,
        who: &[&str],
        project: &str,
        shared: &[String],
    ) -> BridgeResult<Bound> {
        let Some(accounts) = &self.accounts else {
            return Err(BridgeError::new(
                "not_offered",
                "this hub does not host account boards; its boards are the ones its operator configured",
            ));
        };
        if !valid_name(project) {
            return Err(BridgeError::new(
                "invalid",
                format!("project {project:?} is not [a-z0-9][a-z0-9-]{{0,62}}"),
            ));
        }
        if shared.iter().any(|name| name == project) {
            return Err(BridgeError::new(
                "name_taken",
                format!(
                    "{project:?} is a board this hub already lets you work on; bind another name"
                ),
            ));
        }
        let held = self.account_boards(subject);
        let limit = (!who.iter().any(|id| {
            accounts.unlimited.contains(*id) || accounts.owner.get().is_some_and(|o| o == id)
        }))
        .then_some(accounts.free_limit);
        let existed = held.iter().any(|name| name == project);
        if !existed && limit.is_some_and(|limit| held.len() >= limit) {
            return Err(BridgeError::new(
                "plan_limit",
                format!(
                    "a free {} holds {} project{} on this hub and this one already has {}; unbind one, or ask the operator",
                    if subject.starts_with("org-") {
                        "org"
                    } else {
                        "account"
                    },
                    accounts.free_limit,
                    if accounts.free_limit == 1 { "" } else { "s" },
                    held.join(", ")
                ),
            ));
        }
        if !existed {
            let dir = accounts.dir.join(subject);
            std::fs::create_dir_all(&dir)
                .map_err(|e| BridgeError::new("board_io", format!("{}: {e}", dir.display())))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&accounts.dir, std::fs::Permissions::from_mode(0o700));
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            let file = Self::account_file(accounts, subject, project);
            let board = Board::open(&file, project)?.reporting_to(self.activity.clone());
            // A new account board starts with the getting-started steps (B12).
            if let Err(error) = board.welcome_if_new(&Actor::agent("devagotchi"), &self.public_url)
            {
                eprintln!(
                    "devagotchi hub: board {project}: getting-started ticket: {}",
                    error.message
                );
            }
            let board = Arc::new(board);
            self.open
                .lock()
                .expect("boards lock")
                .insert(format!("{subject}/{project}"), board);
        }
        let mut boards = self.account_boards(subject);
        boards.extend(shared.iter().cloned());
        Ok(Bound {
            project: project.to_string(),
            created: !existed,
            boards,
            limit,
        })
    }

    /// The board named `project`, opened if this is its first use. A directory that was
    /// initialised under another project name is refused: ids embed the prefix.
    pub fn open(&self, project: &str) -> BridgeResult<Arc<Board>> {
        let mut open = self.open.lock().expect("boards lock");
        if let Some(board) = open.get(project) {
            return Ok(board.clone());
        }
        let dir = self.dirs.get(project).ok_or_else(|| {
            BridgeError::new(
                "unknown_board",
                format!("no board for project {project:?} is configured on this hub"),
            )
        })?;
        let (config, board) = open_project(dir)?;
        let board = board.reporting_to(self.activity.clone());
        if config.project != project {
            return Err(BridgeError::new(
                "board_mismatch",
                format!(
                    "{} was initialised as project {:?}; the hub config names it {project:?}",
                    dir.display(),
                    config.project
                ),
            ));
        }
        // A board that has never had a ticket starts with the steps to check that
        // Devagotchi works, so the first session never finds it empty.
        if let Err(error) = board.welcome_if_new(&Actor::agent("devagotchi"), &self.public_url) {
            eprintln!(
                "devagotchi hub: board {project}: getting-started ticket: {}",
                error.message
            );
        }
        let board = Arc::new(board);
        open.insert(project.to_string(), board.clone());
        Ok(board)
    }

    /// Keep leases honest without a sweep timer: sleep until the first lease on any open
    /// board lapses, or until a board changes (a claim, heartbeat or release moves that
    /// moment), then expire what has lapsed. Runs for the life of the hub.
    pub async fn reap_on_time(self: Arc<Self>) {
        loop {
            let boards = self.clone();
            let next = tokio::task::spawn_blocking(move || boards.next_lease_expiry())
                .await
                .unwrap_or(None);
            let now = crate::board::system_clock()();
            match next {
                Some(at) if at <= now => {
                    let boards = self.clone();
                    let failed = tokio::task::spawn_blocking(move || {
                        let mut failed = false;
                        for (project, result) in boards.reap_all() {
                            if let Err(error) = result {
                                eprintln!("devagotchi hub: board {project}: reap failed: {error}");
                                failed = true;
                            }
                        }
                        failed
                    })
                    .await
                    .unwrap_or(true);
                    if failed {
                        // A store that cannot be written now is not retried in a tight loop.
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
                Some(at) => {
                    let lapse = Duration::from_millis((at - now) as u64);
                    tokio::select! {
                        _ = self.activity.notified() => {}
                        _ = tokio::time::sleep(lapse) => {}
                    }
                }
                None => self.activity.notified().await,
            }
        }
    }

    /// The first moment a lease on any open board lapses.
    fn next_lease_expiry(&self) -> Option<i64> {
        let open: Vec<_> = self
            .open
            .lock()
            .expect("boards lock")
            .values()
            .cloned()
            .collect();
        open.iter()
            .filter_map(|board| match board.next_lease_expiry() {
                Ok(at) => at,
                Err(error) => {
                    eprintln!("devagotchi hub: lease check failed: {error}");
                    None
                }
            })
            .min()
    }

    /// Expire lapsed leases on every open board.
    pub fn reap_all(&self) -> Vec<(String, BridgeResult<usize>)> {
        let open: Vec<_> = self
            .open
            .lock()
            .expect("boards lock")
            .iter()
            .map(|(name, board)| (name.clone(), board.clone()))
            .collect();
        open.into_iter()
            .map(|(name, board)| {
                let reaped = board.reap();
                (name, reaped)
            })
            .collect()
    }

    fn remember_fence(&self, subject: &str, project: &str, ticket: &str, fence: i64) {
        self.fences.lock().expect("fence lock").insert(
            (subject.to_string(), project.to_string(), ticket.to_string()),
            fence,
        );
    }

    fn forget_fence(&self, subject: &str, project: &str, ticket: &str) {
        self.fences.lock().expect("fence lock").remove(&(
            subject.to_string(),
            project.to_string(),
            ticket.to_string(),
        ));
    }

    fn fence_for(
        &self,
        subject: &str,
        project: &str,
        ticket: &str,
        given: Option<i64>,
    ) -> BridgeResult<i64> {
        if let Some(fence) = given {
            return Ok(fence);
        }
        self.fences
            .lock()
            .expect("fence lock")
            .get(&(subject.to_string(), project.to_string(), ticket.to_string()))
            .copied()
            .ok_or_else(|| {
                BridgeError::new(
                    "stale_fence",
                    format!(
                        "no fence known for {ticket}: pass the fence from your claim, or claim it again"
                    ),
                )
            })
    }
}

/// A refused board call: the store's stable code, its message, and what to call next.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BoardError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    pub next: String,
}

fn next_for(code: &str, id: Option<&str>) -> String {
    let id = id.unwrap_or("<id>");
    match code {
        "not_offered" => "whoami lists the boards this hub serves you; ask its operator for a grant".to_string(),
        "name_taken" => "whoami lists that shared board; use it, or board_bind another name".to_string(),
        "plan_limit" => "whoami lists the project you hold; work in it, or ask the operator for more".to_string(),
        "stale_fence" | "lease_expired" | "not_holder" => {
            format!("board_get {id} to see who holds it; board_claim {id} to take it if it is free")
        }
        "held_by_other" => format!("board_get {id}; wait, or board_claim with no id for another ticket"),
        "version_conflict" => format!("board_get {id} and retry board_update with the current version"),
        "wip_limit" => "board_complete, board_handoff or board_release one of the tickets you hold first".into(),
        "blocked" => format!("board_get {id}: finish or hand off the blockers it lists"),
        "distinct_required" => "someone else must take this lane; leave it for them".into(),
        "not_claimable" => format!("board_get {id}: only a lane queue is claimable; board_update moves it along the workflow"),
        "use_lease_ops" => "use board_claim / board_complete / board_handoff / board_release; held tickets do not move by board_update".into(),
        "transition_not_allowed" => "board_prime names the states; the workflow allows only its transitions".into(),
        "humans_only" => "ask the person who owns this board".into(),
        "not_found" => "board_list with open_only to see the ids on this board".into(),
        "not_granted" | "unknown_board" => "whoami lists the boards your identity may work on".into(),
        "no_board" | "board_mismatch" | "board_version" => "the hub's operator must initialise or fix this board; nothing an agent can do".into(),
        "nothing_ready" => "board_create what you found, or wait with board_wait".into(),
        _ => "board_prime to re-orient".into(),
    }
}

fn refuse(error: BridgeError, ticket: Option<&str>) -> Json<BoardError> {
    Json(BoardError {
        next: next_for(&error.code, ticket),
        code: error.code,
        message: error.message,
        ticket: ticket.map(str::to_string),
    })
}

// ----- inputs ------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectIn {
    /// The project whose board to use, as the hub names it (whoami lists yours).
    pub project: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PrimeIn {
    pub project: String,
    /// How many ready tickets to list (1..=10, default 10).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadyIn {
    pub project: String,
    /// At most this many (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListIn {
    pub project: String,
    #[serde(default)]
    pub state: Option<String>,
    /// A holder's name, or `@me` for yourself as this board knows you.
    #[serde(default)]
    pub holder: Option<String>,
    /// An assignee's name, or `@me`.
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Children of this ticket only.
    #[serde(default)]
    pub parent: Option<String>,
    /// task, epic or message.
    #[serde(default)]
    pub kind: Option<String>,
    /// Leave out done and cancelled tickets.
    #[serde(default)]
    pub open_only: bool,
    /// Substring of the title or description.
    #[serde(default)]
    pub text: Option<String>,
    /// At most this many (default 50).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TicketIn {
    pub project: String,
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateIn {
    pub project: String,
    /// 1 to 300 characters.
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// task (default), epic or message.
    #[serde(default)]
    pub kind: Option<String>,
    /// 0 is most urgent; the board default is 2.
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Makes this a child (`parent.n` id) and records a `parent` dependency.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    /// The ticket you were working when you found this one; recorded as `discovered-from`.
    #[serde(default)]
    pub discovered_from: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClaimIn {
    pub project: String,
    /// A ticket to claim. Omit it to claim the best ready ticket for you.
    #[serde(default)]
    pub id: Option<String>,
    /// Lease length; the board clamps it to its min and max.
    #[serde(default)]
    pub ttl_ms: Option<i64>,
    /// Without an id: only tickets carrying this label.
    #[serde(default)]
    pub label: Option<String>,
    /// Without an id: only tickets in these queue states.
    #[serde(default)]
    pub states: Vec<String>,
    /// Without an id: only children of this ticket.
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ClaimOut {
    /// False when nothing was ready for you; `lease` is then absent.
    pub claimed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease: Option<Lease>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HeartbeatIn {
    pub project: String,
    pub id: String,
    /// The fence from your claim. Omit it to use the one this hub last gave you for this ticket.
    #[serde(default)]
    pub fence: Option<i64>,
    /// Anything watchers should see about where you are; replaces the previous progress.
    #[serde(default)]
    pub progress: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateIn {
    pub project: String,
    pub id: String,
    /// The ticket's current version (optimistic concurrency); from board_get.
    pub version: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    /// Replaces the label set.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub clear_assignee: bool,
    /// A declared workflow transition for an unheld ticket (blocked, cancelled, back to
    /// ready). Active states and held tickets move only through claim / complete /
    /// handoff / release.
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteIn {
    pub project: String,
    pub id: String,
    /// 1 to 20000 characters; notes are append-only and travel with the ticket.
    pub body: String,
    /// note (default), handoff, review, decision, question …
    #[serde(default)]
    pub kind: Option<String>,
    /// Address the note to a subject; it shows in their board_prime.
    #[serde(default)]
    pub to: Option<String>,
    /// Required while you hold the ticket; omit to use the one this hub last gave you.
    #[serde(default)]
    pub fence: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HandoffIn {
    pub project: String,
    pub id: String,
    #[serde(default)]
    pub fence: Option<i64>,
    /// What the next worker must know. Required.
    pub note: String,
    /// Who should pick it up; they become the assignee.
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReleaseIn {
    pub project: String,
    pub id: String,
    #[serde(default)]
    pub fence: Option<i64>,
    /// Why you are letting it go without finishing.
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompleteIn {
    pub project: String,
    pub id: String,
    #[serde(default)]
    pub fence: Option<i64>,
    /// What was done, for the reviewer. Required.
    pub summary: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DepIn {
    pub project: String,
    pub id: String,
    pub depends_on: String,
    /// blocks (default; the only kind readiness uses), parent, related, discovered-from,
    /// duplicates or supersedes.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub remove: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForceIn {
    pub project: String,
    pub id: String,
    /// At least three characters; recorded on the ticket.
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EventsIn {
    pub project: String,
    /// Events with a higher sequence number than this (0 for all).
    #[serde(default)]
    pub after_seq: i64,
    /// At most this many (default 100, max 1000).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitIn {
    pub project: String,
    /// Return as soon as an event newer than this exists.
    pub after_seq: i64,
    /// How long to wait before returning an empty list (default 20000, max 25000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApprovalRequestIn {
    pub project: String,
    /// One line a person reads on their phone, e.g. "Restart the hub on Texas".
    pub title: String,
    /// Exactly what will run or change.
    pub what: String,
    /// Why it is needed now.
    #[serde(default)]
    pub why: Option<String>,
    /// How to undo it if it goes wrong.
    #[serde(default)]
    pub undo: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApprovalWaitIn {
    pub project: String,
    pub id: String,
    /// How long to wait for the decision (default 25000, max 50000). 0 reads the status now.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApprovalOut {
    #[serde(flatten)]
    pub approval: ApprovalStatus,
    /// Where the person approves or denies it.
    pub url: String,
    /// On a request: how many phones were sent a push. 0 means nobody who decides on this
    /// board has turned on notifications: give the user the url yourself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phones: Option<usize>,
}

// ----- the tools ---------------------------------------------------------------

type BoardOut<T> = Result<Json<T>, Json<BoardError>>;

impl Hub {
    /// Push "approval needed" to the phones of the people who decide on `project`. The
    /// payload names the approval and links to it; it never carries the command text.
    /// Returns how many phones the push goes to.
    pub fn announce_approval(&self, project: &str, approval: &ApprovalStatus) -> usize {
        self.announce_approval_to(self.config.people_for_board(project), project, approval)
    }

    /// Push "approval needed" to these people's phones only (a test goes to the person
    /// who asked for it). Returns how many phones the push goes to.
    pub fn announce_approval_to(
        &self,
        people: Vec<String>,
        project: &str,
        approval: &ApprovalStatus,
    ) -> usize {
        let Some(push) = self.push.clone() else {
            return 0;
        };
        let phones = push.phones_for(&people);
        let payload = serde_json::json!({
            "title": "Approval needed",
            "body": approval.title,
            "url": format!("/approvals#{project}-{}", approval.id),
            "tag": approval.id,
        });
        tokio::spawn(async move { push.notify(&people, &payload).await });
        phones
    }

    /// An approval with the page where a person decides it.
    fn approval_out(&self, project: &str, approval: ApprovalStatus) -> ApprovalOut {
        let url = format!(
            "{}/approvals#{project}-{}",
            self.config.public_url.trim_end_matches('/'),
            approval.id
        );
        ApprovalOut {
            approval,
            url,
            phones: None,
        }
    }

    /// The board `project` for this request's subject, after the grant check.
    /// The board `project` names for this account: its own first, then a shared one by
    /// grant. An account board and a shared board never share a name (`bind` refuses).
    fn board_for(&self, subject: &Subject, project: &str) -> BridgeResult<(Arc<Board>, Kind)> {
        if let Some(board) = self
            .boards
            .open_account(&Boards::owner_of(subject), project)?
        {
            let kind = if subject.org.is_some() {
                Kind::Org
            } else {
                Kind::Account
            };
            return Ok((board, kind));
        }
        let granted = self
            .config
            .boards_for(&subject.id)
            .iter()
            .any(|b| b.project == project);
        if !granted {
            return Err(if self.config.board(project).is_none() {
                BridgeError::new(
                    "unknown_board",
                    if self.boards.hosts_accounts() {
                        format!(
                            "no board for project {project:?} here: board_bind makes yours, or ask for a grant on a shared one"
                        )
                    } else {
                        format!("no board for project {project:?} is configured on this hub")
                    },
                )
            } else {
                BridgeError::new(
                    "not_granted",
                    format!("{} holds no grant for board {project:?}", subject.id),
                )
            });
        }
        Ok((self.boards.open(project)?, Kind::Shared))
    }

    /// The seat the caller named (`X-Devagotchi-Seat`), checked like any board name.
    fn seat(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Option<String>, Json<BoardError>> {
        let Some(value) = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get(SEAT_HEADER))
        else {
            return Ok(None);
        };
        match value.to_str() {
            Ok(seat) if valid_name(seat) => Ok(Some(seat.to_string())),
            _ => Err(refuse(
                BridgeError::new(
                    "invalid",
                    "X-Devagotchi-Seat must be a name like a board's: [a-z0-9][a-z0-9-]{0,62}",
                ),
                None,
            )),
        }
    }

    /// Who the store sees: the seat on the account's own board, `<account>/<seat>` on an
    /// org's or a shared one (two members' Claudes are two actors), the account itself
    /// without a seat. A seat is never a person; the account is the person on its own
    /// board and on its org's, and on a shared one when its grant says so.
    fn actor_for(&self, subject: &Subject, seat: Option<&str>, kind: Kind) -> (String, bool) {
        // An AI the hub connected itself (`via: hub`) is never the person: without a seat
        // of its own it works as the account's "ai" seat.
        let seat = match (seat, subject.via) {
            (None, "hub") => Some("ai"),
            (seat, _) => seat,
        };
        match (seat, kind) {
            (Some(seat), Kind::Account) => (seat.to_string(), false),
            (Some(seat), Kind::Org | Kind::Shared) => (format!("{}/{seat}", subject.id), false),
            (None, Kind::Account | Kind::Org) => (subject.id.clone(), true),
            (None, Kind::Shared) => (subject.id.clone(), self.config.is_human(&subject.id)),
        }
    }

    /// Run one store call for the authenticated subject, on a blocking thread, and
    /// audit it: who, which tool, which project, which ticket, how it ended.
    async fn board_call<T, F>(
        &self,
        tool: &'static str,
        project: &str,
        ticket: Option<&str>,
        context: &RequestContext<RoleServer>,
        f: F,
    ) -> BoardOut<T>
    where
        T: Send + 'static,
        F: FnOnce(&Board, &Actor<'_>) -> BridgeResult<T> + Send + 'static,
    {
        let subject = self.board_subject(context)?;
        let seat = self.seat(context)?;
        let result = match self.board_for(&subject, project) {
            Ok((board, kind)) => {
                let (id, human) = self.actor_for(&subject, seat.as_deref(), kind);
                tokio::task::spawn_blocking(move || {
                    let actor = Actor {
                        id: &id,
                        human,
                        wip_limit: None,
                    };
                    f(&board, &actor)
                })
                .await
                .unwrap_or_else(|join| {
                    Err(BridgeError::new(
                        "board_store",
                        format!("board call did not complete: {join}"),
                    ))
                })
            }
            Err(error) => Err(error),
        };
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(error) => error.code.clone(),
        };
        if let Err(error) = self.audit.record(&AuditEntry {
            subject: &subject.id,
            via: subject.via,
            tool,
            broker: project,
            operation_id: ticket.unwrap_or_default(),
            outcome: &outcome,
        }) {
            // A call that cannot be audited is not a call this bastion made.
            return Err(refuse(error, ticket));
        }
        result.map(Json).map_err(|error| refuse(error, ticket))
    }

    fn board_subject(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Subject, Json<BoardError>> {
        self.subject(context)
            .map_err(|Json(error)| refuse(error, None))
    }

    /// The key the fence memory uses: the account, and the seat when one is named, so two
    /// seats of one account never pick up each other's fences.
    fn subject_id(&self, context: &RequestContext<RoleServer>) -> Result<String, Json<BoardError>> {
        let subject = self.board_subject(context)?;
        Ok(match self.seat(context)? {
            Some(seat) => format!("{}#{seat}", subject.id),
            None => subject.id,
        })
    }
}

#[tool_router(router = board_router, vis = "pub(crate)")]
impl Hub {
    #[tool(
        name = "board_prime",
        description = "Start here, every session: the tickets you hold (with their fences), what is ready for you to claim, what needs a person, notes addressed to you, and the heartbeat interval. Your identity is the bearer token; `project` is the board.",
        title = "Start a board session",
        annotations(
            title = "Start a board session",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_prime(
        &self,
        Parameters(input): Parameters<PrimeIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Prime> {
        let limit = input.limit.unwrap_or(10) as usize;
        self.board_call(
            "board_prime",
            &input.project,
            None,
            &context,
            move |board, actor| board.prime(actor, limit),
        )
        .await
    }

    #[tool(
        name = "board_ready",
        description = "Tickets you could claim right now, best first: assigned to you, then priority, then age. Only lane queues with no open blocker and no live lease.",
        title = "Tickets ready to claim",
        annotations(
            title = "Tickets ready to claim",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_ready(
        &self,
        Parameters(input): Parameters<ReadyIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Vec<Ticket>> {
        let limit = input.limit.unwrap_or(20).clamp(1, 200) as usize;
        self.board_call(
            "board_ready",
            &input.project,
            None,
            &context,
            move |board, actor| board.ready(actor, limit),
        )
        .await
    }

    #[tool(
        name = "board_list",
        description = "Tickets on the board, filtered by state, holder, assignee, label, parent, kind or text. open_only leaves out done and cancelled.",
        title = "List tickets",
        annotations(title = "List tickets", read_only_hint = true, open_world_hint = false)
    )]
    async fn board_list(
        &self,
        Parameters(input): Parameters<ListIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Vec<Ticket>> {
        let filter = Filter {
            state: input.state,
            holder: input.holder,
            assignee: input.assignee,
            label: input.label,
            parent: input.parent,
            kind: input.kind,
            ready: false,
            open_only: input.open_only,
            text: input.text,
            limit: Some(input.limit.unwrap_or(50).clamp(1, 500) as usize),
        };
        self.board_call(
            "board_list",
            &input.project,
            None,
            &context,
            move |board, actor| {
                // `@me` is whoever the caller is on this board (a seat is spelled by the
                // hub, so the command line cannot name itself).
                let me = |who: Option<String>| {
                    who.map(|w| if w == ME { actor.id.to_string() } else { w })
                };
                let filter = Filter {
                    holder: me(filter.holder),
                    assignee: me(filter.assignee),
                    ..filter
                };
                board.list(&filter)
            },
        )
        .await
    }

    #[tool(
        name = "board_get",
        description = "One ticket in full: fields, version, holder and lease, dependencies both ways, children, and every note.",
        title = "Get a ticket",
        annotations(title = "Get a ticket", read_only_hint = true, open_world_hint = false)
    )]
    async fn board_get(
        &self,
        Parameters(input): Parameters<TicketIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<TicketView> {
        let id = input.id.clone();
        self.board_call(
            "board_get",
            &input.project,
            Some(&input.id),
            &context,
            move |board, _| board.get(&id),
        )
        .await
    }

    #[tool(
        name = "board_create",
        description = "Create a ticket. Work you discover while holding another ticket goes on the board with discovered_from set, never silently done. parent makes a child with a `parent.n` id.",
        title = "Create a ticket",
        annotations(
            title = "Create a ticket",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_create(
        &self,
        Parameters(input): Parameters<CreateIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Ticket> {
        let new = NewTicket {
            title: input.title,
            description: input.description,
            kind: input.kind,
            priority: input.priority,
            labels: input.labels,
            parent: input.parent,
            assignee: input.assignee,
            discovered_from: input.discovered_from,
        };
        self.board_call(
            "board_create",
            &input.project,
            None,
            &context,
            move |board, actor| board.create(actor, new),
        )
        .await
    }

    #[tool(
        name = "board_claim",
        description = "Take a lease on a ticket (by id, or the best ready one for you when id is omitted). Returns the fence every later call on it needs and the heartbeat interval. Re-claiming what you hold returns the current lease. A ticket held by someone else, blocked, or over your WIP limit is refused.",
        title = "Claim a ticket",
        annotations(
            title = "Claim a ticket",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_claim(
        &self,
        Parameters(input): Parameters<ClaimIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<ClaimOut> {
        let subject = self.subject_id(&context)?;
        let project = input.project.clone();
        let id = input.id.clone();
        let wanted = id.clone();
        let result = self
            .board_call(
                "board_claim",
                &input.project,
                id.as_deref(),
                &context,
                move |board, actor| match &wanted {
                    Some(id) => board.claim(actor, id, input.ttl_ms).map(Some),
                    None => board.claim_next(
                        actor,
                        &NextFilter {
                            label: input.label,
                            states: input.states,
                            parent: input.parent,
                            ttl_ms: input.ttl_ms,
                        },
                    ),
                },
            )
            .await?;
        let Json(lease) = result;
        if let Some(lease) = &lease {
            self.boards
                .remember_fence(&subject, &project, &lease.ticket.id, lease.fence);
        }
        Ok(Json(ClaimOut {
            claimed: lease.is_some(),
            lease,
        }))
    }

    #[tool(
        name = "board_heartbeat",
        description = "Extend your lease on a ticket you hold, at least as often as board_prime's heartbeat_every_ms says; optionally record progress for watchers. A lapsed lease is refused: claim again.",
        title = "Extend a lease",
        annotations(
            title = "Extend a lease",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_heartbeat(
        &self,
        Parameters(input): Parameters<HeartbeatIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Lease> {
        let subject = self.subject_id(&context)?;
        let fence = self
            .boards
            .fence_for(&subject, &input.project, &input.id, input.fence)
            .map_err(|e| refuse(e, Some(&input.id)))?;
        let id = input.id.clone();
        let result = self
            .board_call(
                "board_heartbeat",
                &input.project,
                Some(&input.id),
                &context,
                move |board, actor| board.heartbeat(actor, &id, fence, input.progress),
            )
            .await?;
        self.boards
            .remember_fence(&subject, &input.project, &input.id, result.0.fence);
        Ok(result)
    }

    #[tool(
        name = "board_update",
        description = "Edit a ticket's title, description, priority, labels or assignee, or move an unheld ticket along a declared workflow transition (blocked, cancelled, back to ready). version must be current; active states and held tickets change only through claim / complete / handoff / release.",
        title = "Update a ticket",
        annotations(
            title = "Update a ticket",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_update(
        &self,
        Parameters(input): Parameters<UpdateIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Ticket> {
        let patch = Patch {
            title: input.title,
            description: input.description,
            priority: input.priority,
            labels: input.labels,
            assignee: if input.clear_assignee {
                Some(None)
            } else {
                input.assignee.map(Some)
            },
            state: input.state,
        };
        let id = input.id.clone();
        let version = input.version;
        self.board_call(
            "board_update",
            &input.project,
            Some(&input.id),
            &context,
            move |board, actor| board.update(actor, &id, version, patch),
        )
        .await
    }

    #[tool(
        name = "board_note",
        description = "Append a note to a ticket: what you learned, a decision, a question, a review. Notes are append-only and travel with the ticket; `to` addresses one to a subject, who sees it in board_prime.",
        title = "Add a note to a ticket",
        annotations(
            title = "Add a note to a ticket",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_note(
        &self,
        Parameters(input): Parameters<NoteIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Note> {
        // A fence is only needed while the caller holds the ticket; use the cached one
        // when the caller gave none and the hub knows one, and let the store decide.
        let subject = self.subject_id(&context)?;
        let fence = input.fence.or_else(|| {
            self.boards
                .fence_for(&subject, &input.project, &input.id, None)
                .ok()
        });
        let id = input.id.clone();
        self.board_call(
            "board_note",
            &input.project,
            Some(&input.id),
            &context,
            move |board, actor| {
                board.add_note(
                    actor,
                    &id,
                    input.kind.as_deref().unwrap_or("note"),
                    &input.body,
                    input.to.as_deref(),
                    fence,
                )
            },
        )
        .await
    }

    #[tool(
        name = "board_handoff",
        description = "Stop working a ticket you hold and leave it for someone else, with the note they need; `to` makes them the assignee. The ticket goes back to its queue unfinished.",
        title = "Hand off a ticket",
        annotations(
            title = "Hand off a ticket",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_handoff(
        &self,
        Parameters(input): Parameters<HandoffIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Ticket> {
        let subject = self.subject_id(&context)?;
        let fence = self
            .boards
            .fence_for(&subject, &input.project, &input.id, input.fence)
            .map_err(|e| refuse(e, Some(&input.id)))?;
        let id = input.id.clone();
        let result = self
            .board_call(
                "board_handoff",
                &input.project,
                Some(&input.id),
                &context,
                move |board, actor| {
                    board.handoff(actor, &id, fence, &input.note, input.to.as_deref())
                },
            )
            .await?;
        self.boards
            .forget_fence(&subject, &input.project, &input.id);
        Ok(result)
    }

    #[tool(
        name = "board_release",
        description = "Give a ticket you hold back to its queue without finishing it, with a reason. Nothing is recorded as done.",
        title = "Release a ticket",
        annotations(
            title = "Release a ticket",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_release(
        &self,
        Parameters(input): Parameters<ReleaseIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Ticket> {
        let subject = self.subject_id(&context)?;
        let fence = self
            .boards
            .fence_for(&subject, &input.project, &input.id, input.fence)
            .map_err(|e| refuse(e, Some(&input.id)))?;
        let id = input.id.clone();
        let result = self
            .board_call(
                "board_release",
                &input.project,
                Some(&input.id),
                &context,
                move |board, actor| board.release(actor, &id, fence, &input.reason),
            )
            .await?;
        self.boards
            .forget_fence(&subject, &input.project, &input.id);
        Ok(result)
    }

    #[tool(
        name = "board_complete",
        description = "Finish a ticket you hold with a summary for the reviewer. It moves to the next lane (review), where someone other than you must take it; a review completed moves it to done.",
        title = "Complete a ticket",
        annotations(
            title = "Complete a ticket",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_complete(
        &self,
        Parameters(input): Parameters<CompleteIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Ticket> {
        let subject = self.subject_id(&context)?;
        let fence = self
            .boards
            .fence_for(&subject, &input.project, &input.id, input.fence)
            .map_err(|e| refuse(e, Some(&input.id)))?;
        let id = input.id.clone();
        let result = self
            .board_call(
                "board_complete",
                &input.project,
                Some(&input.id),
                &context,
                move |board, actor| board.complete(actor, &id, fence, &input.summary),
            )
            .await?;
        self.boards
            .forget_fence(&subject, &input.project, &input.id);
        Ok(result)
    }

    #[tool(
        name = "board_dep",
        description = "Record (or remove) a dependency: `id` depends on `depends_on`. Only `blocks` gates readiness; parent, related, discovered-from, duplicates and supersedes are memory. Cycles are refused.",
        title = "Set a dependency",
        annotations(
            title = "Set a dependency",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_dep(
        &self,
        Parameters(input): Parameters<DepIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Vec<Dep>> {
        let kind = match input.kind.as_deref() {
            None => DepKind::Blocks,
            Some(name) => DepKind::parse(name).ok_or_else(|| {
                refuse(
                    BridgeError::new(
                        "invalid",
                        format!("kind {name:?} is not blocks, parent, related, discovered-from, duplicates or supersedes"),
                    ),
                    Some(&input.id),
                )
            })?,
        };
        let id = input.id.clone();
        self.board_call(
            "board_dep",
            &input.project,
            Some(&input.id),
            &context,
            move |board, actor| {
                if input.remove {
                    board.remove_dep(actor, &id, &input.depends_on, kind)
                } else {
                    board.add_dep(actor, &id, &input.depends_on, kind)
                }
            },
        )
        .await
    }

    #[tool(
        name = "board_force_release",
        description = "A person's act only: take a ticket away from whoever holds it, with a reason, and put it back in its queue. Agents are refused.",
        title = "Force-release a ticket",
        annotations(
            title = "Force-release a ticket",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn board_force_release(
        &self,
        Parameters(input): Parameters<ForceIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Ticket> {
        let id = input.id.clone();
        self.board_call(
            "board_force_release",
            &input.project,
            Some(&input.id),
            &context,
            move |board, actor| board.force_release(actor, &id, &input.reason),
        )
        .await
    }

    #[tool(
        name = "board_events",
        description = "The board's append-only, hash-chained event log after a sequence number: every create, claim, note, handoff and completion, by whom and when.",
        title = "Board event log",
        annotations(
            title = "Board event log",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_events(
        &self,
        Parameters(input): Parameters<EventsIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Vec<Event>> {
        let limit = input.limit.unwrap_or(100).clamp(1, 1000) as usize;
        let after = input.after_seq;
        self.board_call(
            "board_events",
            &input.project,
            None,
            &context,
            move |board, _| board.events(after, limit),
        )
        .await
    }

    #[tool(
        name = "board_wait",
        description = "Wait for board events: returns the events newer than after_seq the moment the board records one, or an empty list when timeout_ms passes (call again). Use it instead of calling board_events in a loop.",
        title = "Wait for board events",
        annotations(
            title = "Wait for board events",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_wait(
        &self,
        Parameters(input): Parameters<WaitIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Vec<Event>> {
        let subject = self.board_subject(&context)?;
        let (board, _) = self
            .board_for(&subject, &input.project)
            .map_err(|e| refuse(e, None))?;
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(input.timeout_ms.unwrap_or(20_000).min(25_000));
        let after = input.after_seq;
        // Subscribed before the first read: a change between the read and the sleep is kept.
        let mut changes = board.changes();
        let events = loop {
            changes.borrow_and_update();
            let board = board.clone();
            let found = tokio::task::spawn_blocking(move || board.events(after, 100))
                .await
                .unwrap_or_else(|join| {
                    Err(BridgeError::new(
                        "board_store",
                        format!("board call did not complete: {join}"),
                    ))
                })
                .map_err(|e| refuse(e, None))?;
            if !found.is_empty() {
                break found;
            }
            // Sleep until the board commits a change; nothing reads it in between.
            tokio::select! {
                biased;
                _ = context.ct.cancelled() => break Vec::new(),
                changed = tokio::time::timeout_at(deadline, changes.changed()) => {
                    if !matches!(changed, Ok(Ok(()))) {
                        break Vec::new();
                    }
                }
            }
        };
        // One audit line per wait, however long it blocked.
        if let Err(error) = self.audit.record(&AuditEntry {
            subject: &subject.id,
            via: subject.via,
            tool: "board_wait",
            broker: &input.project,
            operation_id: "",
            outcome: "ok",
        }) {
            return Err(refuse(error, None));
        }
        Ok(Json(events))
    }

    #[tool(
        name = "approval_request",
        description = "Ask the user to approve one step before you take it: what will run or change, why, and how to undo it. It reaches them where they approve (their phone) and returns at once with the approval's id, status pending and phones (how many phones were notified; if 0, give the user the url yourself); then call approval_wait with that id and act only when it says approved. A denial means do not take the step.",
        title = "Ask the user to approve a step",
        annotations(
            title = "Ask the user to approve a step",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn approval_request(
        &self,
        Parameters(input): Parameters<ApprovalRequestIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<ApprovalOut> {
        let project = input.project.clone();
        let Json(approval) = self
            .board_call(
                "approval_request",
                &input.project,
                None,
                &context,
                move |board, actor| {
                    board.request_approval(
                        actor,
                        &input.title,
                        &input.what,
                        input.why.as_deref().unwrap_or_default(),
                        input.undo.as_deref().unwrap_or_default(),
                    )
                },
            )
            .await?;
        let phones = self.announce_approval(&project, &approval);
        let mut out = self.approval_out(&project, approval);
        out.phones = Some(phones);
        Ok(Json(out))
    }

    #[tool(
        name = "approval_wait",
        description = "Wait for the user's decision on an approval you requested: returns as soon as they approve or deny, or with status pending when timeout_ms passes (call again). status approved: take the step. denied: do not; read note for why.",
        title = "Wait for the user's decision",
        annotations(
            title = "Wait for the user's decision",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn approval_wait(
        &self,
        Parameters(input): Parameters<ApprovalWaitIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<ApprovalOut> {
        let subject = self.board_subject(&context)?;
        let (board, _) = self
            .board_for(&subject, &input.project)
            .map_err(|e| refuse(e, Some(&input.id)))?;
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(input.timeout_ms.unwrap_or(25_000).min(50_000));
        let mut changes = board.changes();
        let approval = loop {
            changes.borrow_and_update();
            let (board, id) = (board.clone(), input.id.clone());
            let status = tokio::task::spawn_blocking(move || board.approval_status(&id))
                .await
                .unwrap_or_else(|join| {
                    Err(BridgeError::new(
                        "board_store",
                        format!("board call did not complete: {join}"),
                    ))
                })
                .map_err(|e| refuse(e, Some(&input.id)))?;
            if status.status != "pending" || tokio::time::Instant::now() >= deadline {
                break status;
            }
            // The decision is a board write: sleep until the board commits a change.
            tokio::select! {
                biased;
                _ = context.ct.cancelled() => break status,
                changed = tokio::time::timeout_at(deadline, changes.changed()) => {
                    if !matches!(changed, Ok(Ok(()))) {
                        break status;
                    }
                }
            }
        };
        if let Err(error) = self.audit.record(&AuditEntry {
            subject: &subject.id,
            via: subject.via,
            tool: "approval_wait",
            broker: &input.project,
            operation_id: &input.id,
            outcome: &approval.status,
        }) {
            return Err(refuse(error, Some(&input.id)));
        }
        Ok(Json(self.approval_out(&input.project, approval)))
    }

    #[tool(
        name = "board_verify",
        description = "Recompute the board's event hash chain and report the first broken entry, if any.",
        title = "Verify the board's event chain",
        annotations(
            title = "Verify the board's event chain",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_verify(
        &self,
        Parameters(input): Parameters<ProjectIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<ChainStatus> {
        self.board_call(
            "board_verify",
            &input.project,
            None,
            &context,
            move |board, _| board.verify_chain(),
        )
        .await
    }

    #[tool(
        name = "board_bind",
        description = "Make this account's own board for a project on this hub (or find it: binding again is harmless), so `project` works in every board_* tool from any of the account's machines. A free account holds one project here. Refused as not_offered on a hub that only serves configured boards, name_taken when the name is a shared board you already reach, plan_limit at the limit.",
        title = "Bind a project to this account's board",
        annotations(
            title = "Bind a project to this account's board",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn board_bind(
        &self,
        Parameters(input): Parameters<ProjectIn>,
        context: RequestContext<RoleServer>,
    ) -> BoardOut<Bound> {
        let subject = self.board_subject(&context)?;
        let shared: Vec<String> = self
            .config
            .boards_for(&subject.id)
            .iter()
            .map(|b| b.project.clone())
            .collect();
        let boards = self.boards.clone();
        let owner = Boards::owner_of(&subject);
        let who: Vec<String> = std::iter::once(subject.id.clone())
            .chain(subject.org.clone())
            .collect();
        let project = input.project.clone();
        let result = tokio::task::spawn_blocking(move || {
            let who: Vec<&str> = who.iter().map(String::as_str).collect();
            boards.bind_for(&owner, &who, &project, &shared)
        })
        .await
        .unwrap_or_else(|join| {
            Err(BridgeError::new(
                "board_store",
                format!("board call did not complete: {join}"),
            ))
        });
        let outcome = match &result {
            Ok(bound) if bound.created => "created".to_string(),
            Ok(_) => "ok".to_string(),
            Err(error) => error.code.clone(),
        };
        if let Err(error) = self.audit.record(&AuditEntry {
            subject: &subject.id,
            via: subject.via,
            tool: "board_bind",
            broker: &input.project,
            operation_id: "",
            outcome: &outcome,
        }) {
            return Err(refuse(error, None));
        }
        result.map(Json).map_err(|error| refuse(error, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BOARD_DIR, BOARD_FILE, ProjectConfig};

    fn hub_config(project: &str, dir: &std::path::Path) -> HubConfig {
        HubConfig::parse(
            &serde_json::json!({
                "listen": "127.0.0.1:1",
                "public_url": "http://127.0.0.1:1",
                "auth": { "mode": "dev", "tokens_file": dir.join("tokens.json") },
                "brokers": [],
                "boards": [{ "project": project, "dir": dir.join("project") }],
                "grants": [{ "subject": "agent", "boards": [project] }],
            })
            .to_string(),
        )
        .expect("config")
    }

    fn init(dir: &std::path::Path, project: &str) {
        let board_dir = dir.join("project").join(BOARD_DIR);
        std::fs::create_dir_all(&board_dir).unwrap();
        let board = Board::open(&board_dir.join(BOARD_FILE), project).unwrap();
        let config = ProjectConfig {
            project: project.into(),
            prefix: board.prefix().into(),
            created_at: 0,
            hub: None,
        };
        std::fs::write(
            board_dir.join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
    }

    /// The reaper sleeps until a lease lapses, then expires it: the ticket is back in its
    /// queue within moments of the lapse, with no sweep timer.
    #[tokio::test]
    async fn a_lapsed_lease_is_reaped_when_it_lapses() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), "site");
        let boards = Arc::new(Boards::from_config(
            &hub_config("site", dir.path()),
            dir.path(),
        ));
        let board = boards.open("site").unwrap();
        let operator = Actor::human("jesse");
        let mut settings = board.settings().unwrap();
        settings.lease_min_ttl_ms = 100;
        board.put_settings(&operator, &settings).unwrap();
        let id = board
            .create(
                &operator,
                crate::board::NewTicket {
                    title: "lapses".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .id;
        tokio::spawn(boards.clone().reap_on_time());
        let began = tokio::time::Instant::now();
        board
            .claim(&Actor::agent("claude"), &id, Some(400))
            .unwrap();
        let mut changes = board.changes();
        changes.borrow_and_update();
        let reaped = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                changes.changed().await.unwrap();
                if board.get(&id).unwrap().ticket.holder.is_none() {
                    return board.get(&id).unwrap().ticket;
                }
            }
        })
        .await
        .expect("the lease was reaped when it lapsed");
        assert_eq!(reaped.state, "ready");
        let elapsed = began.elapsed();
        assert!(elapsed >= Duration::from_millis(350), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    fn hosted_config(dir: &std::path::Path, unlimited: &[&str]) -> HubConfig {
        HubConfig::parse(
            &serde_json::json!({
                "listen": "127.0.0.1:1",
                "public_url": "http://127.0.0.1:1",
                "auth": { "mode": "dev", "tokens_file": dir.join("tokens.json") },
                "owner": "300000000000000001",
                "brokers": [],
                "boards": [{ "project": "shared", "dir": dir.join("project") }],
                "grants": [{ "subject": "300000000000000002", "boards": ["shared"] }],
                "account_boards": { "dir": dir.join("accounts"), "unlimited": unlimited },
            })
            .to_string(),
        )
        .expect("config")
    }

    #[test]
    fn org_boards_belong_to_the_org_and_every_member_works_them() {
        use crate::auth::Subject;
        let member = |id: &str, org: Option<&str>| Subject {
            id: id.into(),
            via: "nomen",
            name: None,
            org: org.map(str::to_string),
            provisional_until: None,
            agent: None,
            claim_code: None,
            provisional_warning: false,
        };
        let (alice, bob) = (
            member("300000000000000011", Some("395263680805142530")),
            member("300000000000000012", Some("395263680805142530")),
        );
        let carol = member("300000000000000013", Some("400000000000000001"));
        let solo = member("300000000000000014", None);
        assert_eq!(Boards::owner_of(&alice), "org-395263680805142530");
        assert_eq!(Boards::owner_of(&bob), Boards::owner_of(&alice));
        assert_eq!(
            Boards::owner_of(&solo),
            solo.id,
            "no org: the account's own"
        );

        let dir = tempfile::tempdir().unwrap();
        // The paid org is unlimited by its org id; Carol's org is free.
        let boards = Boards::from_config(
            &hosted_config(dir.path(), &["395263680805142530"]),
            dir.path(),
        );
        let bind = |who: &Subject, project: &str| {
            let ids: Vec<&str> = std::iter::once(who.id.as_str())
                .chain(who.org.as_deref())
                .collect();
            boards.bind_for(&Boards::owner_of(who), &ids, project, &[])
        };
        assert!(bind(&alice, "site").unwrap().created);
        assert!(
            !bind(&bob, "site").unwrap().created,
            "Bob finds the org's board"
        );
        let a = boards
            .open_account(&Boards::owner_of(&alice), "site")
            .unwrap()
            .unwrap();
        let b = boards
            .open_account(&Boards::owner_of(&bob), "site")
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b), "one board, not two");
        assert!(
            bind(&bob, "api").unwrap().created,
            "an unlimited org binds more"
        );
        assert_eq!(
            boards.account_boards(&Boards::owner_of(&alice)),
            vec!["api", "site"]
        );

        assert!(
            boards
                .open_account(&Boards::owner_of(&carol), "site")
                .unwrap()
                .is_none(),
            "another org never reaches it"
        );
        assert!(
            bind(&carol, "site").unwrap().created,
            "its own board of the same name"
        );
        assert_eq!(
            bind(&carol, "more").unwrap_err().code,
            "plan_limit",
            "a free org holds one"
        );
        assert!(
            boards.open_account(&solo.id, "site").unwrap().is_none(),
            "an account with no org has only its own"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("accounts/org-395263680805142530"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn account_boards_are_bound_per_account_counted_and_never_named_like_a_shared_one() {
        let dir = tempfile::tempdir().unwrap();
        let only_configured =
            Boards::from_config(&hub_config("devagotchi", dir.path()), dir.path());
        assert_eq!(
            only_configured
                .bind("someone", "theirs", &[])
                .expect_err("refused")
                .code,
            "not_offered"
        );
        assert!(!only_configured.hosts_accounts());

        let boards = Boards::from_config(
            &hosted_config(dir.path(), &["300000000000000003"]),
            dir.path(),
        );
        assert!(boards.hosts_accounts());
        let free = "300000000000000002";
        assert!(boards.account_boards(free).is_empty());
        assert!(
            boards.open_account(free, "mine").unwrap().is_none(),
            "nothing until bound"
        );

        let bound = boards.bind(free, "mine", &["shared".into()]).unwrap();
        assert!(bound.created);
        assert_eq!(bound.boards, vec!["mine".to_string(), "shared".to_string()]);
        assert_eq!(bound.limit, Some(1));
        assert!(
            dir.path()
                .join("accounts")
                .join(free)
                .join("mine.sqlite3")
                .is_file()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("accounts").join(free))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{mode:o}");
        }
        let again = boards.bind(free, "mine", &["shared".into()]).unwrap();
        assert!(!again.created, "binding is idempotent");
        assert_eq!(boards.account_boards(free), vec!["mine".to_string()]);
        let opened = boards.open_account(free, "mine").unwrap().expect("bound");
        assert_eq!(
            opened.prefix(),
            Board::open(
                &dir.path().join("accounts").join(free).join("mine.sqlite3"),
                "mine"
            )
            .unwrap()
            .prefix()
        );

        assert_eq!(
            boards.bind(free, "another", &[]).expect_err("limit").code,
            "plan_limit"
        );
        assert_eq!(
            boards
                .bind(free, "shared", &["shared".into()])
                .expect_err("taken")
                .code,
            "name_taken"
        );
        assert_eq!(
            boards
                .bind(free, "Bad Name", &[])
                .expect_err("invalid")
                .code,
            "invalid"
        );
        assert!(
            boards.open_account(free, "../etc").unwrap().is_none(),
            "a bad name opens nothing"
        );

        // The owner and the unlimited list bind as many as they like.
        for subject in ["300000000000000001", "300000000000000003"] {
            for project in ["a", "b", "c"] {
                let bound = boards.bind(subject, project, &[]).unwrap();
                assert!(
                    bound.created && bound.limit.is_none(),
                    "{subject} {project}"
                );
            }
            assert_eq!(boards.account_boards(subject), vec!["a", "b", "c"]);
        }
        // Another account's board is not this account's, whatever the name.
        assert!(boards.open_account(free, "a").unwrap().is_none());
        assert!(!boards.is_empty());
    }

    #[test]
    fn an_uninitialised_or_renamed_project_is_refused_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let boards = Boards::from_config(&hub_config("devagotchi", dir.path()), dir.path());
        assert!(!boards.is_empty());
        let error = boards.open("devagotchi").err().expect("refused");
        assert_eq!(error.code, "no_board");
        assert!(error.message.contains("board init"), "{}", error.message);
        assert!(
            !dir.path().join("project").join(BOARD_DIR).exists(),
            "nothing was created"
        );
        assert_eq!(
            boards.open("other").err().expect("refused").code,
            "unknown_board"
        );

        init(dir.path(), "something-else");
        let error = boards.open("devagotchi").err().expect("refused");
        assert_eq!(error.code, "board_mismatch");
        assert!(
            error.message.contains("something-else"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_board_opens_once_and_the_fence_memory_follows_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), "devagotchi");
        let boards = Boards::from_config(&hub_config("devagotchi", dir.path()), dir.path());
        let first = boards.open("devagotchi").unwrap();
        let again = boards.open("devagotchi").unwrap();
        assert!(Arc::ptr_eq(&first, &again), "one open board per project");
        assert_eq!(boards.reap_all().len(), 1);

        let missing = boards
            .fence_for("agent", "devagotchi", "dvg-1", None)
            .unwrap_err();
        assert_eq!(missing.code, "stale_fence");
        assert_eq!(
            boards
                .fence_for("agent", "devagotchi", "dvg-1", Some(7))
                .unwrap(),
            7,
            "a given fence wins"
        );
        boards.remember_fence("agent", "devagotchi", "dvg-1", 3);
        assert_eq!(
            boards
                .fence_for("agent", "devagotchi", "dvg-1", None)
                .unwrap(),
            3
        );
        assert_eq!(
            boards
                .fence_for("someone-else", "devagotchi", "dvg-1", None)
                .unwrap_err()
                .code,
            "stale_fence",
            "fences are remembered per subject"
        );
        boards.forget_fence("agent", "devagotchi", "dvg-1");
        assert_eq!(
            boards
                .fence_for("agent", "devagotchi", "dvg-1", None)
                .unwrap_err()
                .code,
            "stale_fence"
        );
    }

    #[test]
    fn refusals_name_what_to_call_next() {
        let refused = refuse(BridgeError::new("held_by_other", "leased"), Some("dvg-1"));
        assert_eq!(refused.0.code, "held_by_other");
        assert_eq!(refused.0.ticket.as_deref(), Some("dvg-1"));
        assert!(
            refused.0.next.contains("board_get dvg-1"),
            "{}",
            refused.0.next
        );
        let unknown = refuse(BridgeError::new("something_new", "?"), None);
        assert!(unknown.0.next.contains("board_prime"));
    }
}
