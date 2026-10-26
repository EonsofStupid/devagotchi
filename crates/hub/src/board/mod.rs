//! The agents' board: tickets that any subscription claims under a lease, works,
//! annotates and hands off, stored per project in `<project>/.devagotchi/board.sqlite3`.
//! Contract: `docs/agent-board.md`. The hub never owns jobs; a ticket is a unit of
//! coordination and memory, a broker operation is a unit of execution.
//!
//! Increment 7 is the store and its protocol; increment 8 adds its two faces: the
//! `board` command line (`cli.rs`) for agents on this host and the `board_*` MCP tools
//! (`mcp.rs`) for agents anywhere, through the hub.

pub mod approval;
pub mod cli;
pub mod face;
mod lease;
pub mod mcp;
pub mod remote;
pub mod welcome;
pub mod workflow;

#[cfg(test)]
mod tests;

use std::{
    cell::Cell,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, watch};

#[allow(unused_imports)]
pub use lease::{Lease, NextFilter};
pub use workflow::{StateKind, Workflow};

/// Milliseconds since the Unix epoch. Injected so tests can move time.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default()
    })
}

/// Who is acting. Identity comes from the hub's authentication, never from a tool
/// argument; `human` gates force-release; `wip_limit` overrides the board default.
#[derive(Debug, Clone)]
pub struct Actor<'a> {
    pub id: &'a str,
    pub human: bool,
    pub wip_limit: Option<i64>,
}

impl<'a> Actor<'a> {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn agent(id: &'a str) -> Self {
        Self {
            id,
            human: false,
            wip_limit: None,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn human(id: &'a str) -> Self {
        Self {
            id,
            human: true,
            wip_limit: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DepKind {
    /// `depends_on` must be done before the ticket is ready. The only kind readiness uses.
    Blocks,
    /// `depends_on` is the parent; the ticket is one of its children.
    Parent,
    Related,
    /// The ticket was found while working `depends_on` (agent-discovered work).
    DiscoveredFrom,
    Duplicates,
    Supersedes,
}

impl DepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DepKind::Blocks => "blocks",
            DepKind::Parent => "parent",
            DepKind::Related => "related",
            DepKind::DiscoveredFrom => "discovered-from",
            DepKind::Duplicates => "duplicates",
            DepKind::Supersedes => "supersedes",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "blocks" => DepKind::Blocks,
            "parent" => DepKind::Parent,
            "related" => DepKind::Related,
            "discovered-from" => DepKind::DiscoveredFrom,
            "duplicates" => DepKind::Duplicates,
            "supersedes" => DepKind::Supersedes,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Dep {
    pub ticket: String,
    pub depends_on: String,
    pub kind: DepKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ticket {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub state: String,
    pub priority: i64,
    pub labels: Vec<String>,
    pub parent: Option<String>,
    pub assignee: Option<String>,
    pub holder: Option<String>,
    pub lease_id: Option<String>,
    pub lease_ttl_ms: Option<i64>,
    pub lease_expires_at: Option<i64>,
    pub heartbeat_at: Option<i64>,
    pub fence: i64,
    pub version: i64,
    pub expiry_count: i64,
    pub last_worker: Option<String>,
    pub progress: Option<serde_json::Value>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
    /// Open `blocks` dependencies, computed on read.
    pub blocked_by: Vec<String>,
    /// The lease has lapsed but the reaper has not run yet, computed on read.
    pub lease_expired: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Note {
    pub seq: i64,
    pub ticket: String,
    pub author: String,
    pub kind: String,
    pub body: String,
    pub to_principal: Option<String>,
    pub fence: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub seq: i64,
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub ticket: Option<String>,
    pub data: serde_json::Value,
    pub fence: Option<i64>,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TicketView {
    pub ticket: Ticket,
    pub deps: Vec<Dep>,
    pub dependents: Vec<Dep>,
    pub children: Vec<String>,
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChainStatus {
    pub entries: u64,
    pub ok: bool,
    pub head: String,
    pub first_broken: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct NewTicket {
    pub title: String,
    pub description: String,
    pub kind: Option<String>,
    pub priority: Option<i64>,
    pub labels: Vec<String>,
    /// Makes the id hierarchical (`parent.n`) and records a `parent` dependency.
    pub parent: Option<String>,
    pub assignee: Option<String>,
    /// Records a `discovered-from` dependency.
    pub discovered_from: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i64>,
    pub labels: Option<Vec<String>>,
    /// `Some(None)` clears the assignee.
    pub assignee: Option<Option<String>>,
    /// A workflow transition for an unheld ticket; held tickets move only through lease operations.
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub state: Option<String>,
    pub holder: Option<String>,
    pub assignee: Option<String>,
    pub label: Option<String>,
    pub parent: Option<String>,
    pub kind: Option<String>,
    /// Only tickets that are claimable now (a lane queue, no open blockers, unheld or lapsed).
    pub ready: bool,
    pub open_only: bool,
    pub text: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Settings {
    pub lease_ttl_ms: i64,
    pub lease_min_ttl_ms: i64,
    pub lease_max_ttl_ms: i64,
    pub max_lease_expiries: i64,
    pub wip_limit_default: i64,
    pub default_priority: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lease_ttl_ms: 15 * 60 * 1000,
            lease_min_ttl_ms: 30 * 1000,
            lease_max_ttl_ms: 60 * 60 * 1000,
            max_lease_expiries: 3,
            wip_limit_default: 2,
            default_priority: 2,
        }
    }
}

/// One project's board. One connection, one writer: SQLite in WAL mode serialises
/// writers anyway, and every mutation appends its event in the same transaction.
pub struct Board {
    connection: Mutex<Connection>,
    project: String,
    prefix: String,
    clock: Clock,
    /// Advances each time a write commits an event, so a wait (board_wait, approval_wait)
    /// sleeps until the board changes instead of reading it again and again.
    changes: watch::Sender<u64>,
    /// Also told of every committed change: the hub's lease reaper, which sleeps until the
    /// next lease lapses or the leases change.
    activity: Option<Arc<Notify>>,
}

pub const SCHEMA_VERSION: i64 = 1;
pub const BOARD_DIR: &str = ".devagotchi";
pub const BOARD_FILE: &str = "board.sqlite3";

pub(crate) fn fail(code: &str, message: impl Into<String>) -> BridgeError {
    BridgeError::new(code, message)
}

pub(crate) fn store_error(error: rusqlite::Error) -> BridgeError {
    BridgeError::new("board_store", error.to_string())
}

fn invalid(message: impl Into<String>) -> BridgeError {
    fail("invalid", message)
}

/// `prefix-xxxx` from the project name: lowercase letters and digits, at most four.
pub fn default_prefix(project: &str) -> String {
    let prefix: String = project
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .take(4)
        .collect();
    if prefix.is_empty() {
        "tk".into()
    } else {
        prefix
    }
}

/// What `board init` writes beside the store, so every face of the board (command
/// line here, the hub's tools) opens it under the name its ids were minted with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project: String,
    pub prefix: String,
    pub created_at: i64,
    /// The hub this project is bound to (B11): with an account on file, every board
    /// command runs there as the account; absent, the board is the local store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<String>,
}

pub fn read_project_config(board_dir: &Path) -> BridgeResult<ProjectConfig> {
    let path = board_dir.join("config.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| BridgeError::new("no_board", format!("{}: {e}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|e| BridgeError::new("board_io", format!("config.json: {e}")))
}

/// Open an initialised project's board: `<project_dir>/.devagotchi/board.sqlite3` under
/// the project name recorded there. An uninitialised directory is `no_board`, never
/// silently created: `board init` is the deliberate act that also writes the ignore
/// file and the AGENTS.md block.
pub fn open_project(project_dir: &Path) -> BridgeResult<(ProjectConfig, Board)> {
    let board_dir = project_dir.join(BOARD_DIR);
    if !board_dir.join(BOARD_FILE).is_file() {
        return Err(BridgeError::new(
            "no_board",
            format!(
                "no board under {}; run `devagotchi board init --dir {}` there",
                project_dir.display(),
                project_dir.display()
            ),
        ));
    }
    let config = read_project_config(&board_dir)?;
    let board = Board::open(&board_dir.join(BOARD_FILE), &config.project)?;
    Ok((config, board))
}

/// What an agent needs at the start of a session, in one read: what it holds (with
/// the fences), what it could claim, what needs a person, and notes addressed to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Prime {
    pub project: String,
    pub actor: String,
    pub human: bool,
    pub held: Vec<Held>,
    pub ready: Vec<Ready>,
    pub attention: Vec<Ready>,
    pub notes_to_me: Vec<NoteToMe>,
    /// Heartbeat a held ticket at least this often.
    pub heartbeat_every_ms: i64,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Held {
    pub id: String,
    pub title: String,
    pub state: String,
    pub fence: i64,
    pub version: i64,
    pub lease_expires_in_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ready {
    pub id: String,
    pub title: String,
    pub state: String,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteToMe {
    pub ticket: String,
    pub from: String,
    pub body: String,
}

impl Board {
    /// Open (creating if needed) the board at `path` for `project`. The prefix is
    /// fixed at creation; reopening with another project name is refused, because
    /// ids embed it and a board never changes identity by being moved.
    pub fn open(path: &Path, project: &str) -> BridgeResult<Self> {
        Self::open_with_clock(path, project, system_clock())
    }

    pub fn open_with_clock(path: &Path, project: &str, clock: Clock) -> BridgeResult<Self> {
        if project.trim().is_empty() {
            return Err(invalid("project name is required"));
        }
        let connection = Connection::open(path).map_err(store_error)?;
        connection.execute_batch(SCHEMA).map_err(store_error)?;
        let stored: Option<String> = connection
            .query_row("SELECT value FROM meta WHERE key = 'project'", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(store_error)?;
        let prefix = match stored {
            Some(existing) if existing != project => {
                return Err(fail(
                    "board_mismatch",
                    format!("this board belongs to project {existing:?}, not {project:?}"),
                ));
            }
            Some(_) => connection
                .query_row("SELECT value FROM meta WHERE key = 'prefix'", [], |row| {
                    row.get(0)
                })
                .map_err(store_error)?,
            None => {
                let prefix = default_prefix(project);
                let now = clock();
                let workflow = Workflow::default_workflow();
                connection
                    .execute_batch("BEGIN IMMEDIATE")
                    .map_err(store_error)?;
                for (key, value) in [
                    ("schema_version", SCHEMA_VERSION.to_string()),
                    ("project", project.to_string()),
                    ("prefix", prefix.clone()),
                    ("created_at", now.to_string()),
                ] {
                    connection
                        .execute(
                            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                            params![key, value],
                        )
                        .map_err(store_error)?;
                }
                connection
                    .execute(
                        "INSERT INTO settings (key, value) VALUES ('settings', ?1), ('workflow', ?2)",
                        params![
                            serde_json::to_string(&Settings::default()).expect("settings json"),
                            serde_json::to_string(&workflow).expect("workflow json"),
                        ],
                    )
                    .map_err(store_error)?;
                connection.execute_batch("COMMIT").map_err(store_error)?;
                prefix
            }
        };
        let version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .map_err(store_error)?;
        if version != SCHEMA_VERSION.to_string() {
            return Err(fail(
                "board_version",
                format!("board schema {version} is not {SCHEMA_VERSION}; migrate before use"),
            ));
        }
        Ok(Self {
            connection: Mutex::new(connection),
            project: project.to_string(),
            prefix,
            clock,
            changes: watch::channel(0).0,
            activity: None,
        })
    }

    /// Tell `activity` of every change this board commits from now on.
    pub fn reporting_to(mut self, activity: Arc<Notify>) -> Self {
        self.activity = Some(activity);
        self
    }

    /// A receiver that sees a new value after each committed change. Subscribe before
    /// reading, then await `changed()`: a change in between is never missed.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// When the first lease held on this board lapses, if any is held.
    pub fn next_lease_expiry(&self) -> BridgeResult<Option<i64>> {
        let connection = self.connection.lock().expect("board lock");
        connection
            .query_row(
                "SELECT MIN(lease_expires_at) FROM tickets WHERE holder IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(store_error)
    }

    #[allow(dead_code)]
    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
    }

    pub fn settings(&self) -> BridgeResult<Settings> {
        let connection = self.connection.lock().expect("board lock");
        load_settings(&connection)
    }

    pub fn workflow(&self) -> BridgeResult<Workflow> {
        let connection = self.connection.lock().expect("board lock");
        load_workflow(&connection)
    }

    /// Replace the settings. Validated before it lands; recorded as an admin event.
    /// Admin verbs: the operator's `board settings` / `board workflow` commands and the
    /// web board (increment 10) call these; until then only the tests do.
    #[allow(dead_code)]
    pub fn put_settings(&self, actor: &Actor<'_>, settings: &Settings) -> BridgeResult<()> {
        if settings.lease_min_ttl_ms <= 0
            || settings.lease_max_ttl_ms < settings.lease_min_ttl_ms
            || settings.lease_ttl_ms < settings.lease_min_ttl_ms
            || settings.lease_ttl_ms > settings.lease_max_ttl_ms
        {
            return Err(invalid(
                "lease ttl bounds must satisfy min <= default <= max, min > 0",
            ));
        }
        self.write(actor, |w| {
            w.tx.execute(
                "UPDATE settings SET value = ?1 WHERE key = 'settings'",
                params![serde_json::to_string(settings).expect("settings json")],
            )
            .map_err(store_error)?;
            w.event("admin.settings.put", None, None, serde_json::json!({}))?;
            Ok(())
        })
    }

    /// Replace the workflow. Every existing ticket's state must still exist in it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn put_workflow(&self, actor: &Actor<'_>, workflow: &Workflow) -> BridgeResult<()> {
        workflow.validate().map_err(invalid)?;
        self.write(actor, |w| {
            let mut statement =
                w.tx.prepare("SELECT DISTINCT state FROM tickets")
                    .map_err(store_error)?;
            let states = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(store_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(store_error)?;
            drop(statement);
            if let Some(unknown) = states.iter().find(|s| workflow.state(s).is_none()) {
                return Err(invalid(format!(
                    "tickets are in state {unknown:?}, which the new workflow lacks"
                )));
            }
            w.tx.execute(
                "UPDATE settings SET value = ?1 WHERE key = 'workflow'",
                params![serde_json::to_string(workflow).expect("workflow json")],
            )
            .map_err(store_error)?;
            w.event(
                "admin.workflow.put",
                None,
                None,
                serde_json::json!({ "id": workflow.id }),
            )?;
            Ok(())
        })
    }

    // ----- reads -------------------------------------------------------------

    pub fn get(&self, id: &str) -> BridgeResult<TicketView> {
        let connection = self.connection.lock().expect("board lock");
        let workflow = load_workflow(&connection)?;
        let ticket = read_ticket(&connection, &workflow, id, self.now())?;
        let deps = read_deps(&connection, "ticket", id)?;
        let dependents = read_deps(&connection, "depends_on", id)?;
        let children = {
            let mut statement = connection
                .prepare("SELECT id FROM tickets WHERE parent = ?1 ORDER BY created_at, id")
                .map_err(store_error)?;
            let rows = statement
                .query_map(params![id], |row| row.get::<_, String>(0))
                .map_err(store_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(store_error)?
        };
        let notes = read_notes(&connection, id)?;
        Ok(TicketView {
            ticket,
            deps,
            dependents,
            children,
            notes,
        })
    }

    pub fn list(&self, filter: &Filter) -> BridgeResult<Vec<Ticket>> {
        let connection = self.connection.lock().expect("board lock");
        let workflow = load_workflow(&connection)?;
        list_tickets(&connection, &workflow, filter, self.now(), None)
    }

    /// What `actor` could claim right now, best first: assigned to them, then priority, then age.
    pub fn ready(&self, actor: &Actor<'_>, limit: usize) -> BridgeResult<Vec<Ticket>> {
        let connection = self.connection.lock().expect("board lock");
        let workflow = load_workflow(&connection)?;
        let filter = Filter {
            ready: true,
            limit: Some(limit),
            ..Filter::default()
        };
        list_tickets(&connection, &workflow, &filter, self.now(), Some(actor.id))
    }

    /// The session-start read. `limit` caps the ready list; held tickets are never cut.
    pub fn prime(&self, actor: &Actor<'_>, limit: usize) -> BridgeResult<Prime> {
        let held = self.list(&Filter {
            holder: Some(actor.id.to_string()),
            ..Filter::default()
        })?;
        let ready = self.ready(actor, limit.clamp(1, 10))?;
        let attention = self.list(&Filter {
            state: Some(self.workflow()?.attention_state),
            limit: Some(5),
            ..Filter::default()
        })?;
        let mut notes_to_me = Vec::new();
        for ticket in self.list(&Filter {
            open_only: true,
            limit: Some(50),
            ..Filter::default()
        })? {
            let view = self.get(&ticket.id)?;
            for note in view.notes.iter().rev().take(3) {
                if note.to_principal.as_deref() == Some(actor.id) {
                    notes_to_me.push(NoteToMe {
                        ticket: ticket.id.clone(),
                        from: note.author.clone(),
                        body: note.body.clone(),
                    });
                }
            }
        }
        let now = self.now();
        let settings = self.settings()?;
        let summary = |t: &Ticket| Ready {
            id: t.id.clone(),
            title: t.title.clone(),
            state: t.state.clone(),
            priority: t.priority,
        };
        Ok(Prime {
            project: self.project.clone(),
            actor: actor.id.to_string(),
            human: actor.human,
            held: held
                .iter()
                .map(|t| Held {
                    id: t.id.clone(),
                    title: t.title.clone(),
                    state: t.state.clone(),
                    fence: t.fence,
                    version: t.version,
                    lease_expires_in_ms: t.lease_expires_at.map(|e| e - now).unwrap_or_default(),
                })
                .collect(),
            ready: ready.iter().map(summary).collect(),
            attention: attention.iter().map(summary).collect(),
            notes_to_me,
            heartbeat_every_ms: settings.lease_ttl_ms / 3,
            protocol: cli::PROTOCOL_LINE.to_string(),
        })
    }

    pub fn events(&self, after_seq: i64, limit: usize) -> BridgeResult<Vec<Event>> {
        let connection = self.connection.lock().expect("board lock");
        let mut statement = connection
            .prepare(
                "SELECT seq, at, actor, action, ticket, data, fence, prev_hash, hash
                 FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![after_seq, limit.max(1) as i64], event_from_row)
            .map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    /// Recompute the event chain. The first broken seq, if any, is reported, never hidden.
    pub fn verify_chain(&self) -> BridgeResult<ChainStatus> {
        let connection = self.connection.lock().expect("board lock");
        let mut statement = connection
            .prepare(
                "SELECT seq, at, actor, action, ticket, data, fence, prev_hash, hash FROM events ORDER BY seq",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map([], event_from_row)
            .map_err(store_error)?;
        let mut prev = GENESIS.to_string();
        let mut entries = 0u64;
        let mut first_broken = None;
        let mut head = GENESIS.to_string();
        for event in rows {
            let event = event.map_err(store_error)?;
            entries += 1;
            let expected = event_hash(&prev, &event);
            if first_broken.is_none() && (event.prev_hash != prev || event.hash != expected) {
                first_broken = Some(event.seq);
            }
            prev = event.hash.clone();
            head = event.hash;
        }
        Ok(ChainStatus {
            entries,
            ok: first_broken.is_none(),
            head,
            first_broken,
        })
    }

    // ----- writes ------------------------------------------------------------

    pub fn create(&self, actor: &Actor<'_>, new: NewTicket) -> BridgeResult<Ticket> {
        let title = new.title.trim().to_string();
        if title.is_empty() || title.chars().count() > 300 {
            return Err(invalid("title must be 1 to 300 characters"));
        }
        let kind = new.kind.unwrap_or_else(|| "task".into());
        if !matches!(kind.as_str(), "task" | "epic" | "message") {
            return Err(invalid("kind must be task, epic or message"));
        }
        let labels = clean_labels(&new.labels);
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let settings = load_settings(w.tx)?;
            let now = w.now;
            let id = match &new.parent {
                Some(parent) => {
                    let parent_row = read_ticket(w.tx, &workflow, parent, now)?;
                    if workflow.is_done(&parent_row.state) {
                        return Err(fail("parent_closed", format!("{parent} is closed")));
                    }
                    let next: i64 = w
                        .tx
                        .query_row(
                            "UPDATE tickets SET child_seq = child_seq + 1 WHERE id = ?1 RETURNING child_seq",
                            params![parent],
                            |row| row.get(0),
                        )
                        .map_err(store_error)?;
                    format!("{parent}.{next}")
                }
                None => w.fresh_id(&self.prefix, &self.project, &title, actor.id)?,
            };
            if let Some(from) = &new.discovered_from {
                read_ticket(w.tx, &workflow, from, now)?;
            }
            let priority = new.priority.unwrap_or(settings.default_priority);
            w.tx
                .execute(
                    "INSERT INTO tickets (id, kind, title, description, state, priority, labels, parent,
                       assignee, created_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                    params![
                        id,
                        kind,
                        title,
                        new.description,
                        workflow.initial_state,
                        priority,
                        serde_json::to_string(&labels).expect("labels json"),
                        new.parent,
                        new.assignee,
                        actor.id,
                        now,
                    ],
                )
                .map_err(store_error)?;
            if let Some(parent) = &new.parent {
                insert_dep(w.tx, &id, parent, DepKind::Parent, actor.id, now)?;
            }
            if let Some(from) = &new.discovered_from {
                insert_dep(w.tx, &id, from, DepKind::DiscoveredFrom, actor.id, now)?;
            }
            w.event(
                "ticket.created",
                Some(&id),
                None,
                serde_json::json!({ "title": title, "state": workflow.initial_state, "parent": new.parent }),
            )?;
            read_ticket(w.tx, &workflow, &id, now)
        })
    }

    /// Edit fields and, for an unheld ticket, move it along a workflow transition.
    /// `version` must match (optimistic concurrency); held tickets refuse state changes.
    pub fn update(
        &self,
        actor: &Actor<'_>,
        id: &str,
        version: i64,
        patch: Patch,
    ) -> BridgeResult<Ticket> {
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let now = w.now;
            let current = read_ticket(w.tx, &workflow, id, now)?;
            if current.kind == approval::KIND {
                return Err(fail(
                    "approval_decide_only",
                    "an approval changes only through a person's decision",
                ));
            }
            if current.version != version {
                return Err(fail(
                    "version_conflict",
                    format!(
                        "version {version} is not current ({}); re-read and retry",
                        current.version
                    ),
                ));
            }
            let mut changed = serde_json::Map::new();
            if let Some(title) = &patch.title {
                let title = title.trim();
                if title.is_empty() || title.chars().count() > 300 {
                    return Err(invalid("title must be 1 to 300 characters"));
                }
                w.tx.execute(
                    "UPDATE tickets SET title = ?2 WHERE id = ?1",
                    params![id, title],
                )
                .map_err(store_error)?;
                changed.insert("title".into(), serde_json::json!(title));
            }
            if let Some(description) = &patch.description {
                w.tx.execute(
                    "UPDATE tickets SET description = ?2 WHERE id = ?1",
                    params![id, description],
                )
                .map_err(store_error)?;
                changed.insert("description".into(), serde_json::json!(true));
            }
            if let Some(priority) = patch.priority {
                w.tx.execute(
                    "UPDATE tickets SET priority = ?2 WHERE id = ?1",
                    params![id, priority],
                )
                .map_err(store_error)?;
                changed.insert("priority".into(), serde_json::json!(priority));
            }
            if let Some(labels) = &patch.labels {
                let labels = clean_labels(labels);
                w.tx.execute(
                    "UPDATE tickets SET labels = ?2 WHERE id = ?1",
                    params![id, serde_json::to_string(&labels).expect("labels json")],
                )
                .map_err(store_error)?;
                changed.insert("labels".into(), serde_json::json!(labels));
            }
            if let Some(assignee) = &patch.assignee {
                w.tx.execute(
                    "UPDATE tickets SET assignee = ?2 WHERE id = ?1",
                    params![id, assignee],
                )
                .map_err(store_error)?;
                changed.insert("assignee".into(), serde_json::json!(assignee));
            }
            if let Some(to) = patch.state.as_ref().filter(|to| *to != &current.state) {
                check_transition(&workflow, &current, to, actor.id)?;
                let closed_at: Option<i64> = workflow.is_done(to).then_some(now);
                w.tx.execute(
                    "UPDATE tickets SET state = ?2, closed_at = ?3 WHERE id = ?1",
                    params![id, to, closed_at],
                )
                .map_err(store_error)?;
                changed.insert("from".into(), serde_json::json!(current.state));
                changed.insert("to".into(), serde_json::json!(to));
            }
            if changed.is_empty() {
                return Ok(current);
            }
            w.tx.execute(
                "UPDATE tickets SET version = version + 1, updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )
            .map_err(store_error)?;
            w.event(
                "ticket.updated",
                Some(id),
                None,
                serde_json::Value::Object(changed),
            )?;
            read_ticket(w.tx, &workflow, id, now)
        })
    }

    pub fn add_dep(
        &self,
        actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>> {
        if id == depends_on {
            return Err(invalid("a ticket cannot depend on itself"));
        }
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let now = w.now;
            read_ticket(w.tx, &workflow, id, now)?;
            read_ticket(w.tx, &workflow, depends_on, now)?;
            if kind == DepKind::Blocks && reaches(w.tx, depends_on, id)? {
                return Err(fail(
                    "dependency_cycle",
                    format!("{depends_on} already depends on {id}"),
                ));
            }
            insert_dep(w.tx, id, depends_on, kind, actor.id, now)?;
            w.event(
                "dep.added",
                Some(id),
                None,
                serde_json::json!({ "depends_on": depends_on, "kind": kind.as_str() }),
            )?;
            read_deps(w.tx, "ticket", id)
        })
    }

    pub fn remove_dep(
        &self,
        actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>> {
        self.write(actor, |w| {
            let removed =
                w.tx.execute(
                    "DELETE FROM deps WHERE ticket = ?1 AND depends_on = ?2 AND kind = ?3",
                    params![id, depends_on, kind.as_str()],
                )
                .map_err(store_error)?;
            if removed == 0 {
                return Err(fail("not_found", "no such dependency"));
            }
            w.event(
                "dep.removed",
                Some(id),
                None,
                serde_json::json!({ "depends_on": depends_on, "kind": kind.as_str() }),
            )?;
            read_deps(w.tx, "ticket", id)
        })
    }

    /// Append a note. A note on a ticket the author holds carries the fence, which
    /// is checked; anyone may annotate an unheld ticket without one.
    pub fn add_note(
        &self,
        actor: &Actor<'_>,
        id: &str,
        kind: &str,
        body: &str,
        to_principal: Option<&str>,
        fence: Option<i64>,
    ) -> BridgeResult<Note> {
        let body = body.trim();
        if body.is_empty() || body.chars().count() > 20_000 {
            return Err(invalid("note body must be 1 to 20000 characters"));
        }
        let kind = if kind.trim().is_empty() {
            "note"
        } else {
            kind.trim()
        };
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let now = w.now;
            let ticket = read_ticket(w.tx, &workflow, id, now)?;
            if ticket.holder.as_deref() == Some(actor.id) && !ticket.lease_expired {
                lease::check_held(&ticket, actor.id, fence)?;
            }
            let seq = insert_note(w.tx, id, actor.id, kind, body, to_principal, fence, now)?;
            w.event(
                "note.added",
                Some(id),
                fence,
                serde_json::json!({ "seq": seq, "kind": kind, "to": to_principal }),
            )?;
            read_note(w.tx, seq)
        })
    }

    /// Run one write transaction: IMMEDIATE so the event chain cannot fork.
    pub(crate) fn write<T>(
        &self,
        actor: &Actor<'_>,
        body: impl FnOnce(&mut Writer<'_>) -> BridgeResult<T>,
    ) -> BridgeResult<T> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("board lock");
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let appended = Cell::new(false);
        let result = {
            let mut writer = Writer {
                tx: &tx,
                actor: actor.id,
                now,
                appended: &appended,
            };
            body(&mut writer)
        };
        match result {
            Ok(value) => {
                tx.commit().map_err(store_error)?;
                drop(connection);
                // Only a write that recorded something wakes anyone: an empty reap does not.
                if appended.get() {
                    self.changes.send_modify(|revision| *revision = revision.wrapping_add(1));
                    if let Some(activity) = &self.activity {
                        activity.notify_one();
                    }
                }
                Ok(value)
            }
            Err(error) => {
                // Dropping the transaction rolls it back.
                Err(error)
            }
        }
    }
}

/// One mutation in flight.
pub(crate) struct Writer<'a> {
    pub tx: &'a Transaction<'a>,
    pub actor: &'a str,
    pub now: i64,
    /// Set when this transaction appends an event.
    appended: &'a Cell<bool>,
}

pub(crate) const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

impl Writer<'_> {
    /// Append one event in this transaction, hash-chained to the last one.
    pub fn event(
        &self,
        action: &str,
        ticket: Option<&str>,
        fence: Option<i64>,
        data: serde_json::Value,
    ) -> BridgeResult<i64> {
        let last: Option<(i64, String)> = self
            .tx
            .query_row(
                "SELECT seq, hash FROM events ORDER BY seq DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(store_error)?;
        let (seq, prev) = last
            .map(|(seq, hash)| (seq + 1, hash))
            .unwrap_or((1, GENESIS.to_string()));
        let event = Event {
            seq,
            at: self.now,
            actor: self.actor.to_string(),
            action: action.to_string(),
            ticket: ticket.map(str::to_string),
            data,
            fence,
            prev_hash: prev.clone(),
            hash: String::new(),
        };
        let hash = event_hash(&prev, &event);
        self.tx
            .execute(
                "INSERT INTO events (seq, at, actor, action, ticket, data, fence, prev_hash, hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    seq,
                    event.at,
                    event.actor,
                    event.action,
                    event.ticket,
                    serde_json::to_string(&event.data).expect("event json"),
                    event.fence,
                    event.prev_hash,
                    hash,
                ],
            )
            .map_err(store_error)?;
        self.appended.set(true);
        Ok(seq)
    }

    /// The next fence: global, strictly increasing, never reused.
    pub fn next_fence(&self) -> BridgeResult<i64> {
        self.tx
            .query_row(
                "UPDATE fence SET value = value + 1 WHERE id = 1 RETURNING value",
                [],
                |row| row.get(0),
            )
            .map_err(store_error)
    }

    /// A hash id, `prefix-xxxx`, lengthened on collision so two machines creating
    /// tickets at once never fight over a counter.
    fn fresh_id(
        &self,
        prefix: &str,
        project: &str,
        title: &str,
        actor: &str,
    ) -> BridgeResult<String> {
        let mut salt = [0u8; 8];
        getrandom::fill(&mut salt).map_err(|error| fail("board_random", error.to_string()))?;
        let digest = hex::encode(Sha256::digest(
            format!(
                "{project}\0{title}\0{actor}\0{}\0{}",
                self.now,
                hex::encode(salt)
            )
            .as_bytes(),
        ));
        for length in 4..=12 {
            let candidate = format!("{prefix}-{}", &digest[..length]);
            let taken: bool = self
                .tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tickets WHERE id = ?1)",
                    params![candidate],
                    |row| row.get(0),
                )
                .map_err(store_error)?;
            if !taken {
                return Ok(candidate);
            }
        }
        Err(fail("board_id", "could not allocate a unique id"))
    }
}

pub(crate) fn event_hash(prev: &str, event: &Event) -> String {
    // Canonical: fixed field order, no whitespace, the same bytes any verifier recomputes.
    let canonical = serde_json::json!({
        "seq": event.seq,
        "at": event.at,
        "actor": event.actor,
        "action": event.action,
        "ticket": event.ticket,
        "data": event.data,
        "fence": event.fence,
    });
    let mut hasher = Sha256::new();
    hasher.update(prev.as_bytes());
    hasher.update(
        serde_json::to_string(&canonical)
            .expect("canonical json")
            .as_bytes(),
    );
    hex::encode(hasher.finalize())
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    let data: String = row.get(5)?;
    Ok(Event {
        seq: row.get(0)?,
        at: row.get(1)?,
        actor: row.get(2)?,
        action: row.get(3)?,
        ticket: row.get(4)?,
        data: serde_json::from_str(&data).unwrap_or(serde_json::Value::Null),
        fence: row.get(6)?,
        prev_hash: row.get(7)?,
        hash: row.get(8)?,
    })
}

pub(crate) fn load_settings(connection: &Connection) -> BridgeResult<Settings> {
    let raw: String = connection
        .query_row(
            "SELECT value FROM settings WHERE key = 'settings'",
            [],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    serde_json::from_str(&raw).map_err(|error| fail("board_settings", error.to_string()))
}

pub(crate) fn load_workflow(connection: &Connection) -> BridgeResult<Workflow> {
    let raw: String = connection
        .query_row(
            "SELECT value FROM settings WHERE key = 'workflow'",
            [],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    serde_json::from_str(&raw).map_err(|error| fail("board_workflow", error.to_string()))
}

const TICKET_COLUMNS: &str = "id, kind, title, description, state, priority, labels, parent, assignee, holder,
    lease_id, lease_ttl_ms, lease_expires_at, heartbeat_at, fence, version, expiry_count, last_worker,
    progress, created_by, created_at, updated_at, closed_at";

fn ticket_from_row(row: &Row<'_>, now: i64) -> rusqlite::Result<Ticket> {
    let labels: String = row.get(6)?;
    let progress: Option<String> = row.get(18)?;
    let lease_expires_at: Option<i64> = row.get(12)?;
    Ok(Ticket {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        state: row.get(4)?,
        priority: row.get(5)?,
        labels: serde_json::from_str(&labels).unwrap_or_default(),
        parent: row.get(7)?,
        assignee: row.get(8)?,
        holder: row.get(9)?,
        lease_id: row.get(10)?,
        lease_ttl_ms: row.get(11)?,
        lease_expires_at,
        heartbeat_at: row.get(13)?,
        fence: row.get(14)?,
        version: row.get(15)?,
        expiry_count: row.get(16)?,
        last_worker: row.get(17)?,
        progress: progress.and_then(|p| serde_json::from_str(&p).ok()),
        created_by: row.get(19)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
        closed_at: row.get(22)?,
        blocked_by: Vec::new(),
        lease_expired: lease_expires_at.is_some_and(|at| at <= now),
    })
}

/// A ticket with its open blockers filled in.
pub(crate) fn read_ticket(
    connection: &Connection,
    workflow: &Workflow,
    id: &str,
    now: i64,
) -> BridgeResult<Ticket> {
    let mut ticket = connection
        .query_row(
            &format!("SELECT {TICKET_COLUMNS} FROM tickets WHERE id = ?1"),
            params![id],
            |row| ticket_from_row(row, now),
        )
        .optional()
        .map_err(store_error)?
        .ok_or_else(|| fail("not_found", format!("ticket {id} not found")))?;
    ticket.blocked_by = open_blockers(connection, workflow, id)?;
    Ok(ticket)
}

pub(crate) fn open_blockers(
    connection: &Connection,
    workflow: &Workflow,
    id: &str,
) -> BridgeResult<Vec<String>> {
    let mut statement = connection
        .prepare(
            "SELECT d.depends_on, t.state FROM deps d JOIN tickets t ON t.id = d.depends_on
             WHERE d.ticket = ?1 AND d.kind = 'blocks' ORDER BY d.depends_on",
        )
        .map_err(store_error)?;
    let rows = statement
        .query_map(params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(store_error)?;
    let mut open = Vec::new();
    for row in rows {
        let (depends_on, state) = row.map_err(store_error)?;
        if !workflow.is_done(&state) {
            open.push(depends_on);
        }
    }
    Ok(open)
}

/// Is there a `blocks` path from `from` to `to`? Used to refuse cycles.
fn reaches(connection: &Connection, from: &str, to: &str) -> BridgeResult<bool> {
    let found: bool = connection
        .query_row(
            "WITH RECURSIVE walk(id) AS (
               SELECT ?1
               UNION
               SELECT d.depends_on FROM deps d JOIN walk ON d.ticket = walk.id WHERE d.kind = 'blocks'
             ) SELECT EXISTS(SELECT 1 FROM walk WHERE id = ?2)",
            params![from, to],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    Ok(found)
}

fn insert_dep(
    connection: &Connection,
    ticket: &str,
    depends_on: &str,
    kind: DepKind,
    actor: &str,
    now: i64,
) -> BridgeResult<()> {
    connection
        .execute(
            "INSERT OR IGNORE INTO deps (ticket, depends_on, kind, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ticket, depends_on, kind.as_str(), actor, now],
        )
        .map_err(store_error)?;
    Ok(())
}

fn read_deps(connection: &Connection, column: &str, id: &str) -> BridgeResult<Vec<Dep>> {
    let sql = match column {
        "ticket" => {
            "SELECT ticket, depends_on, kind FROM deps WHERE ticket = ?1 ORDER BY kind, depends_on"
        }
        _ => {
            "SELECT ticket, depends_on, kind FROM deps WHERE depends_on = ?1 ORDER BY kind, ticket"
        }
    };
    let mut statement = connection.prepare(sql).map_err(store_error)?;
    let rows = statement
        .query_map(params![id], |row| {
            let kind: String = row.get(2)?;
            Ok(Dep {
                ticket: row.get(0)?,
                depends_on: row.get(1)?,
                kind: DepKind::parse(&kind).unwrap_or(DepKind::Related),
            })
        })
        .map_err(store_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_note(
    connection: &Connection,
    ticket: &str,
    author: &str,
    kind: &str,
    body: &str,
    to_principal: Option<&str>,
    fence: Option<i64>,
    now: i64,
) -> BridgeResult<i64> {
    connection
        .execute(
            "INSERT INTO notes (ticket, author, kind, body, to_principal, fence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![ticket, author, kind, body, to_principal, fence, now],
        )
        .map_err(store_error)?;
    Ok(connection.last_insert_rowid())
}

fn note_from_row(row: &Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        seq: row.get(0)?,
        ticket: row.get(1)?,
        author: row.get(2)?,
        kind: row.get(3)?,
        body: row.get(4)?,
        to_principal: row.get(5)?,
        fence: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn read_note(connection: &Connection, seq: i64) -> BridgeResult<Note> {
    connection
        .query_row(
            "SELECT seq, ticket, author, kind, body, to_principal, fence, created_at FROM notes WHERE seq = ?1",
            params![seq],
            note_from_row,
        )
        .map_err(store_error)
}

fn read_notes(connection: &Connection, ticket: &str) -> BridgeResult<Vec<Note>> {
    let mut statement = connection
        .prepare(
            "SELECT seq, ticket, author, kind, body, to_principal, fence, created_at
             FROM notes WHERE ticket = ?1 ORDER BY seq",
        )
        .map_err(store_error)?;
    let rows = statement
        .query_map(params![ticket], note_from_row)
        .map_err(store_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
}

/// Candidates in queue order, then the filters SQL cannot express (readiness needs
/// the workflow and the dependency graph), bounded by `limit`.
pub(crate) fn list_tickets(
    connection: &Connection,
    workflow: &Workflow,
    filter: &Filter,
    now: i64,
    ready_for: Option<&str>,
) -> BridgeResult<Vec<Ticket>> {
    let mut query = Query::new(format!("SELECT {TICKET_COLUMNS} FROM tickets WHERE 1 = 1"));
    if let Some(state) = &filter.state {
        query.and("state = ", Box::new(state.clone()));
    }
    if let Some(holder) = &filter.holder {
        query.and("holder = ", Box::new(holder.clone()));
    }
    if let Some(assignee) = &filter.assignee {
        query.and("assignee = ", Box::new(assignee.clone()));
    }
    if let Some(parent) = &filter.parent {
        query.and("parent = ", Box::new(parent.clone()));
    }
    if let Some(kind) = &filter.kind {
        query.and("kind = ", Box::new(kind.clone()));
    }
    if let Some(label) = &filter.label {
        query.and_wrapped(
            "EXISTS (SELECT 1 FROM json_each(tickets.labels) WHERE json_each.value = ",
            Box::new(label.clone()),
            ")",
        );
    }
    if let Some(text) = &filter.text {
        let pattern = format!(
            "%{}%",
            text.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        query.and_wrapped("(title LIKE ", Box::new(pattern.clone()), " ESCAPE '\\'");
        // Same pattern, second column: the clause is "... OR description LIKE ?n ESCAPE '\')".
        query.or_wrapped("description LIKE ", Box::new(pattern), " ESCAPE '\\')");
    }
    if filter.ready || ready_for.is_some() {
        query.and_wrapped(
            "(holder IS NULL OR lease_expires_at <= ",
            Box::new(now),
            ")",
        );
    }
    let (mut sql, args) = query.finish();
    sql.push_str(" ORDER BY (assignee IS NOT NULL) DESC, priority, created_at, id");
    let mut statement = connection.prepare(&sql).map_err(store_error)?;
    let params = rusqlite::params_from_iter(args.iter().map(|a| a.as_ref()));
    let rows = statement
        .query_map(params, |row| ticket_from_row(row, now))
        .map_err(store_error)?;
    let limit = filter.limit.unwrap_or(200).max(1);
    let mut out = Vec::new();
    for row in rows {
        let mut ticket = row.map_err(store_error)?;
        if filter.open_only && workflow.is_done(&ticket.state) {
            continue;
        }
        ticket.blocked_by = open_blockers(connection, workflow, &ticket.id)?;
        if filter.ready || ready_for.is_some() {
            if workflow.lane_by_queue(&ticket.state).is_none() && !ticket.lease_expired {
                continue;
            }
            if !ticket.blocked_by.is_empty() {
                continue;
            }
            if let Some(me) = ready_for {
                if ticket.assignee.as_deref().is_some_and(|a| a != me) {
                    continue;
                }
                let lane = workflow
                    .lane_by_queue(&ticket.state)
                    .or_else(|| workflow.lane_by_active(&ticket.state));
                if lane.is_some_and(|l| l.distinct_from_previous)
                    && ticket.last_worker.as_deref() == Some(me)
                {
                    continue;
                }
            }
        }
        out.push(ticket);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// SQL assembled clause by clause with numbered placeholders, so a filter never
/// reaches the statement as text.
struct Query {
    sql: String,
    args: Vec<Box<dyn rusqlite::ToSql>>,
}

impl Query {
    fn new(sql: String) -> Self {
        Self {
            sql,
            args: Vec::new(),
        }
    }

    fn and(&mut self, clause: &str, value: Box<dyn rusqlite::ToSql>) {
        self.and_wrapped(clause, value, "");
    }

    fn and_wrapped(&mut self, before: &str, value: Box<dyn rusqlite::ToSql>, after: &str) {
        self.args.push(value);
        self.sql
            .push_str(&format!(" AND {before}?{}{after}", self.args.len()));
    }

    fn or_wrapped(&mut self, before: &str, value: Box<dyn rusqlite::ToSql>, after: &str) {
        self.args.push(value);
        self.sql
            .push_str(&format!(" OR {before}?{}{after}", self.args.len()));
    }

    fn finish(self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        (self.sql, self.args)
    }
}

/// A plain update may only move an unheld ticket along a declared transition.
fn check_transition(
    workflow: &Workflow,
    ticket: &Ticket,
    to: &str,
    actor: &str,
) -> BridgeResult<()> {
    let Some(target) = workflow.state(to) else {
        return Err(invalid(format!("unknown state {to:?}")));
    };
    let held = ticket.holder.is_some() && !ticket.lease_expired;
    if held || target.kind == StateKind::Active {
        return Err(fail(
            "use_lease_ops",
            "held and active states change only through claim, complete, release or handoff",
        ));
    }
    if !workflow.allows(&ticket.state, to) {
        return Err(fail(
            "transition_not_allowed",
            format!(
                "workflow {} has no transition {} -> {}",
                workflow.id, ticket.state, to
            ),
        ));
    }
    if workflow
        .lane_by_queue(&ticket.state)
        .is_some_and(|l| l.distinct_from_previous)
        && ticket.last_worker.as_deref() == Some(actor)
    {
        return Err(fail(
            "distinct_required",
            format!(
                "you did the previous step; someone else must move it out of {}",
                ticket.state
            ),
        ));
    }
    Ok(())
}

fn clean_labels(labels: &[String]) -> Vec<String> {
    let mut out: Vec<String> = labels
        .iter()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS fence (id INTEGER PRIMARY KEY CHECK (id = 1), value INTEGER NOT NULL);
INSERT OR IGNORE INTO fence (id, value) VALUES (1, 0);
CREATE TABLE IF NOT EXISTS tickets (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL DEFAULT 'task',
  title TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 2,
  labels TEXT NOT NULL DEFAULT '[]',
  parent TEXT REFERENCES tickets(id),
  child_seq INTEGER NOT NULL DEFAULT 0,
  assignee TEXT,
  holder TEXT,
  lease_id TEXT,
  lease_ttl_ms INTEGER,
  lease_expires_at INTEGER,
  heartbeat_at INTEGER,
  fence INTEGER NOT NULL DEFAULT 0,
  version INTEGER NOT NULL DEFAULT 1,
  expiry_count INTEGER NOT NULL DEFAULT 0,
  last_worker TEXT,
  progress TEXT,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  closed_at INTEGER,
  CHECK ((holder IS NULL) = (lease_id IS NULL))
);
CREATE INDEX IF NOT EXISTS tickets_queue ON tickets (state, priority, created_at) WHERE holder IS NULL;
CREATE INDEX IF NOT EXISTS tickets_lease ON tickets (lease_expires_at) WHERE holder IS NOT NULL;
CREATE INDEX IF NOT EXISTS tickets_parent ON tickets (parent);
CREATE TABLE IF NOT EXISTS deps (
  ticket TEXT NOT NULL REFERENCES tickets(id),
  depends_on TEXT NOT NULL REFERENCES tickets(id),
  kind TEXT NOT NULL DEFAULT 'blocks',
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (ticket, depends_on, kind),
  CHECK (ticket <> depends_on)
);
CREATE INDEX IF NOT EXISTS deps_reverse ON deps (depends_on);
CREATE TABLE IF NOT EXISTS notes (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  ticket TEXT NOT NULL REFERENCES tickets(id),
  author TEXT NOT NULL,
  kind TEXT NOT NULL,
  body TEXT NOT NULL,
  to_principal TEXT,
  fence INTEGER,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS notes_ticket ON notes (ticket, seq);
CREATE TABLE IF NOT EXISTS events (
  seq INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  actor TEXT NOT NULL,
  action TEXT NOT NULL,
  ticket TEXT,
  data TEXT NOT NULL DEFAULT '{}',
  fence INTEGER,
  prev_hash TEXT NOT NULL,
  hash TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_ticket ON events (ticket, seq);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
  BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
  BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;
CREATE TRIGGER IF NOT EXISTS notes_no_update BEFORE UPDATE ON notes
  BEGIN SELECT RAISE(ABORT, 'notes is append-only'); END;
CREATE TRIGGER IF NOT EXISTS notes_no_delete BEFORE DELETE ON notes
  BEGIN SELECT RAISE(ABORT, 'notes is append-only'); END;
";
