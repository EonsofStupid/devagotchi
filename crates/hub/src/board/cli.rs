//! `devagotchi-hub board …`: the agent-facing command line over one project's board.
//! Agent-first by contract (docs/agent-board.md §7): JSON whenever stdout is not a
//! terminal, `--quiet`, one exit code per failure class, every error carries `next`,
//! and `prime` is what a session runs first — the tickets it holds, what is ready for
//! it, and the one-line protocol, in under fifty lines.
//!
//! Identity on this surface is the OS user: the board file is theirs, as the broker's
//! trust scope is (plan §5.4). An agent names itself with `--as` or `DEVAGOTCHI_ACTOR`
//! and is then an agent (no force-release); a bare OS user is the human.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use serde::{Deserialize, Serialize};

use super::{
    Actor, BOARD_DIR, BOARD_FILE, Board, DepKind, Filter, NewTicket, NextFilter, Patch,
    ProjectConfig, Ticket, default_prefix, open_project, read_project_config,
};

pub const USAGE: &str = "Usage: devagotchi-hub board <command> [options]

  init      [--project NAME] [--prefix P] [--agents-md]   create .devagotchi/ here; --agents-md appends the AGENTS.md block
  prime     [--limit N]            what to do now: held, ready, notes to you, the protocol (≤ 50 lines)
  onboard                          print the AGENTS.md block
  agent                            print the full workflow for an agent
  ready     [--limit N]            tickets you could claim now, best first
  ls        [--state S] [--label L] [--text T] [--open] [--limit N]
  show      ID
  new       TITLE [--desc D] [--priority N] [--label L]... [--parent ID] [--assign ID] [--found-in ID] [--epic]
  claim     [ID] [--ttl SECONDS] [--label L] [--state S]   no ID: the best ready ticket
  beat      ID [--progress JSON] [--fence N]
  note      ID TEXT [--kind K] [--to ID] [--fence N]
  done      ID [SUMMARY] [--fence N]
  handoff   ID NOTE [--to ID] [--fence N]
  release   ID [REASON] [--fence N]
  force     ID REASON              a person takes a lease away
  set       ID --version V [--title T] [--desc D] [--priority N] [--state S] [--assign ID|-]
  dep       ID --on ID [--kind blocks|parent|related|discovered-from|duplicates|supersedes] [--remove]
  reap                             expire lapsed leases
  verify                           recompute the event chain
  export    [--out PATH]           board.jsonl (export, never the truth)

Global: --dir PATH (project directory; default: walk up from here)  --as ID (act as this agent)
        --json (also on when stdout is not a terminal)  --quiet
Exit codes: 0 ok · 2 conflict (fence, version, held, blocked, WIP) · 3 refused · 4 not found · 5 board unavailable · 64 usage";

/// Everything the command line needs from its environment, so tests can supply it.
pub struct Invocation {
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// `DEVAGOTCHI_ACTOR`, if set.
    pub actor_env: Option<String>,
    /// The OS user (`USER` / `USERNAME`).
    pub os_user: Option<String>,
    pub stdout_is_terminal: bool,
}

pub const EXIT_OK: u8 = 0;
pub const EXIT_CONFLICT: u8 = 2;
pub const EXIT_REFUSED: u8 = 3;
pub const EXIT_NOT_FOUND: u8 = 4;
pub const EXIT_UNAVAILABLE: u8 = 5;
pub const EXIT_USAGE: u8 = 64;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ClaimCache {
    #[serde(default)]
    claims: BTreeMap<String, CachedClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedClaim {
    fence: i64,
    version: i64,
    lease_expires_at: i64,
}

struct Parsed {
    command: String,
    positional: Vec<String>,
    flags: BTreeMap<String, Vec<String>>,
    switches: Vec<String>,
}

const VALUE_FLAGS: &[&str] = &[
    "--dir",
    "--as",
    "--project",
    "--prefix",
    "--limit",
    "--state",
    "--label",
    "--text",
    "--desc",
    "--priority",
    "--parent",
    "--assign",
    "--found-in",
    "--ttl",
    "--progress",
    "--fence",
    "--kind",
    "--to",
    "--version",
    "--title",
    "--on",
    "--out",
];
const SWITCHES: &[&str] = &[
    "--json",
    "--quiet",
    "--agents-md",
    "--epic",
    "--open",
    "--remove",
];

fn parse(args: &[OsString]) -> Result<Parsed, String> {
    let mut command = None;
    let mut positional = Vec::new();
    let mut flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut switches = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let arg = arg.to_string_lossy().into_owned();
        if let Some(stripped) = arg.strip_prefix("--") {
            let name = format!("--{}", stripped.split('=').next().unwrap_or(""));
            if SWITCHES.contains(&name.as_str()) {
                switches.push(name);
            } else if VALUE_FLAGS.contains(&name.as_str()) {
                let value = match arg.split_once('=') {
                    Some((_, v)) => v.to_string(),
                    None => iter
                        .next()
                        .map(|v| v.to_string_lossy().into_owned())
                        .ok_or_else(|| format!("{name} needs a value"))?,
                };
                flags.entry(name).or_default().push(value);
            } else {
                return Err(format!("unknown option {name}"));
            }
        } else if command.is_none() {
            command = Some(arg);
        } else {
            positional.push(arg);
        }
    }
    Ok(Parsed {
        command: command.unwrap_or_else(|| "help".into()),
        positional,
        flags,
        switches,
    })
}

impl Parsed {
    fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
    }
    fn flag_all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }
    fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }
    fn int(&self, name: &str) -> Result<Option<i64>, String> {
        match self.flag(name) {
            None => Ok(None),
            Some(v) => v
                .parse::<i64>()
                .map(Some)
                .map_err(|_| format!("{name} must be an integer")),
        }
    }
}

/// Walk up from `start` to the first directory holding `.devagotchi/board.sqlite3`.
pub fn find_project_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        if d.join(BOARD_DIR).join(BOARD_FILE).is_file() {
            return Some(d);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

fn exit_code_for(code: &str) -> u8 {
    match code {
        "stale_fence" | "version_conflict" | "held_by_other" | "lease_expired" | "not_holder"
        | "wip_limit" | "blocked" | "distinct_required" | "dependency_cycle" | "not_held" => {
            EXIT_CONFLICT
        }
        "not_found" => EXIT_NOT_FOUND,
        "board_store" | "board_open" | "board_mismatch" | "board_version" | "board_random"
        | "board_id" | "no_board" | "board_io" => EXIT_UNAVAILABLE,
        "usage" => EXIT_USAGE,
        _ => EXIT_REFUSED,
    }
}

fn next_for(code: &str, id: Option<&str>) -> String {
    let id = id.unwrap_or("<id>");
    match code {
        "stale_fence" | "lease_expired" | "not_holder" => {
            format!("board show {id} — see who holds it; board claim {id} to take it if it is free")
        }
        "held_by_other" => {
            format!("board show {id}; wait, or board claim with no id for another ticket")
        }
        "version_conflict" => {
            format!("board show {id} --json and retry set with the current version")
        }
        "wip_limit" => "board done / handoff / release one of the tickets you hold first".into(),
        "blocked" => format!("board show {id} — finish or hand off the blockers it lists"),
        "distinct_required" => "someone else must take this lane; leave it for them".into(),
        "not_claimable" => format!(
            "board show {id} — only a lane queue is claimable; a person moves it with board set"
        ),
        "use_lease_ops" => {
            "use claim / done / handoff / release; held tickets do not move by set".into()
        }
        "transition_not_allowed" => "board agent lists the transitions the workflow allows".into(),
        "humans_only" => {
            "ask the person who owns this board; run without --as to act as yourself".into()
        }
        "not_found" => "board ls --open to see the ids on this board".into(),
        "no_board" => "run board init in the project directory, or pass --dir".into(),
        "usage" => "board --help".into(),
        _ => "board --help".into(),
    }
}

#[derive(Serialize)]
struct ErrorOut<'a> {
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    next: String,
}

struct Output<'a> {
    json: bool,
    quiet: bool,
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
}

impl Output<'_> {
    fn value(&mut self, value: &impl Serialize, text: impl FnOnce() -> String) {
        if self.json {
            let _ = writeln!(
                self.out,
                "{}",
                serde_json::to_string(value).unwrap_or_default()
            );
        } else if !self.quiet {
            let _ = writeln!(self.out, "{}", text());
        }
    }

    fn lines(&mut self, text: &str) {
        if !self.quiet {
            let _ = writeln!(self.out, "{text}");
        }
    }

    fn error(&mut self, error: &BridgeError, id: Option<&str>) -> u8 {
        let next = next_for(&error.code, id);
        if self.json {
            let body = ErrorOut {
                error: ErrorBody {
                    code: &error.code,
                    message: &error.message,
                    next: next.clone(),
                },
            };
            let _ = writeln!(
                self.out,
                "{}",
                serde_json::to_string(&body).unwrap_or_default()
            );
        } else {
            let _ = writeln!(
                self.err,
                "error {}: {}\n  next: {}",
                error.code, error.message, next
            );
        }
        exit_code_for(&error.code)
    }
}

/// Run one invocation. Returns the process exit code; writes to `out` and `err`.
pub fn run(invocation: Invocation, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let parsed = match parse(&invocation.args) {
        Ok(p) => p,
        Err(message) => {
            let _ = writeln!(err, "error usage: {message}\n  next: board --help");
            return EXIT_USAGE;
        }
    };
    let json = parsed.switch("--json") || !invocation.stdout_is_terminal;
    let mut output = Output {
        json,
        quiet: parsed.switch("--quiet"),
        out,
        err,
    };
    if matches!(parsed.command.as_str(), "help" | "--help") {
        let _ = writeln!(output.out, "{USAGE}");
        return EXIT_OK;
    }
    let (actor_id, human) = match parsed
        .flag("--as")
        .map(str::to_string)
        .or(invocation.actor_env.clone())
    {
        Some(id) => (id, false),
        None => (
            invocation
                .os_user
                .clone()
                .unwrap_or_else(|| "operator".into()),
            true,
        ),
    };
    let actor = Actor {
        id: &actor_id,
        human,
        wip_limit: None,
    };
    match parsed.command.as_str() {
        "onboard" => {
            output.lines(&agents_md_block());
            return EXIT_OK;
        }
        "agent" => {
            output.lines(AGENT_WORKFLOW);
            return EXIT_OK;
        }
        "init" => return init(&parsed, &invocation, &mut output),
        _ => {}
    }
    let project_dir = match parsed.flag("--dir") {
        Some(dir) => {
            let dir = invocation.cwd.join(dir);
            if dir.join(BOARD_DIR).join(BOARD_FILE).is_file() {
                dir
            } else {
                return output.error(
                    &BridgeError::new("no_board", format!("no board under {}", dir.display())),
                    None,
                );
            }
        }
        None => match find_project_dir(&invocation.cwd) {
            Some(dir) => dir,
            None => {
                return output.error(
                    &BridgeError::new("no_board", "no .devagotchi/board.sqlite3 here or above"),
                    None,
                );
            }
        },
    };
    let board_dir = project_dir.join(BOARD_DIR);
    let (_config, board) = match open_project(&project_dir) {
        Ok(opened) => opened,
        Err(error) => return output.error(&error, None),
    };
    let mut cache = read_cache(&board_dir, &actor_id);
    let result = dispatch(&parsed, &board, &actor, &mut cache, &board_dir, &mut output);
    write_cache(&board_dir, &actor_id, &cache);
    match result {
        Ok(code) => code,
        Err((error, id)) => output.error(&error, id.as_deref()),
    }
}

type CommandResult = Result<u8, (BridgeError, Option<String>)>;

fn usage_error(message: impl Into<String>) -> (BridgeError, Option<String>) {
    (BridgeError::new("usage", message), None)
}

fn with_id<T>(id: &str, result: BridgeResult<T>) -> Result<T, (BridgeError, Option<String>)> {
    result.map_err(|error| (error, Some(id.to_string())))
}

fn no_id<T>(result: BridgeResult<T>) -> Result<T, (BridgeError, Option<String>)> {
    result.map_err(|error| (error, None))
}

fn init(parsed: &Parsed, invocation: &Invocation, output: &mut Output<'_>) -> u8 {
    let project_dir = parsed
        .flag("--dir")
        .map(|d| invocation.cwd.join(d))
        .unwrap_or_else(|| invocation.cwd.clone());
    let board_dir = project_dir.join(BOARD_DIR);
    let existing = read_project_config(&board_dir).ok();
    let project = parsed
        .flag("--project")
        .map(str::to_string)
        .or_else(|| existing.as_ref().map(|c| c.project.clone()))
        .or_else(|| {
            project_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "project".into());
    let prefix = parsed
        .flag("--prefix")
        .map(str::to_string)
        .or_else(|| existing.as_ref().map(|c| c.prefix.clone()))
        .unwrap_or_else(|| default_prefix(&project));
    if let Err(error) = std::fs::create_dir_all(&board_dir) {
        return output.error(&BridgeError::new("board_io", error.to_string()), None);
    }
    let board = match Board::open(&board_dir.join(BOARD_FILE), &project) {
        Ok(b) => b,
        Err(error) => return output.error(&error, None),
    };
    let config = ProjectConfig {
        project: project.clone(),
        prefix: board.prefix().to_string(),
        created_at: existing
            .map(|c| c.created_at)
            .unwrap_or_else(|| board.now()),
    };
    if prefix != config.prefix {
        output.lines(&format!(
            "note: this board already uses prefix {}; --prefix {} ignored",
            config.prefix, prefix
        ));
    }
    if let Err(error) = write_json(&board_dir.join("config.json"), &config) {
        return output.error(&error, None);
    }
    let gitignore = board_dir.join(".gitignore");
    if !gitignore.exists() {
        let _ = std::fs::write(
            &gitignore,
            "# the board itself is local state; board.jsonl is the export to commit if you want diffs\nboard.sqlite3\nboard.sqlite3-wal\nboard.sqlite3-shm\nclaims/\n",
        );
    }
    let mut agents_md = "unchanged";
    if parsed.switch("--agents-md") {
        let path = project_dir.join("AGENTS.md");
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current.contains(AGENTS_MD_START) {
            agents_md = "already present";
        } else {
            let mut next = current;
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            if !next.is_empty() {
                next.push('\n');
            }
            next.push_str(&agents_md_block());
            if let Err(error) = std::fs::write(&path, next) {
                return output.error(&BridgeError::new("board_io", error.to_string()), None);
            }
            agents_md = "block appended";
        }
    }
    output.value(
        &serde_json::json!({
            "project": config.project,
            "prefix": config.prefix,
            "board": board_dir.join(BOARD_FILE),
            "agents_md": agents_md,
        }),
        || {
            format!(
                "board ready: {} (ids {}-…)\n  {}\n  AGENTS.md: {}\n  next: devagotchi-hub board prime — or board onboard to paste the block elsewhere",
                config.project,
                config.prefix,
                board_dir.join(BOARD_FILE).display(),
                agents_md
            )
        },
    );
    EXIT_OK
}

fn dispatch(
    parsed: &Parsed,
    board: &Board,
    actor: &Actor<'_>,
    cache: &mut ClaimCache,
    board_dir: &Path,
    output: &mut Output<'_>,
) -> CommandResult {
    let limit = parsed
        .int("--limit")
        .map_err(usage_error)?
        .unwrap_or(20)
        .max(1) as usize;
    let arg = |n: usize| parsed.positional.get(n).map(String::as_str);
    let need =
        |n: usize, what: &str| arg(n).ok_or_else(|| usage_error(format!("{what} is required")));
    let fence_for = |id: &str| -> Result<i64, (BridgeError, Option<String>)> {
        match parsed.int("--fence").map_err(usage_error)? {
            Some(f) => Ok(f),
            None => cache.claims.get(id).map(|c| c.fence).ok_or_else(|| {
                (
                    BridgeError::new(
                        "stale_fence",
                        format!("no cached claim for {id}; pass --fence or claim it first"),
                    ),
                    Some(id.to_string()),
                )
            }),
        }
    };
    match parsed.command.as_str() {
        "prime" => {
            let prime = no_id(board.prime(actor, limit))?;
            output.value(&prime, || {
                let mut lines = vec![format!(
                    "board {} · you are {}{}",
                    prime.project,
                    prime.actor,
                    if prime.human { " (person)" } else { "" }
                )];
                if prime.held.is_empty() {
                    lines.push("held by you: none".into());
                } else {
                    lines.push(format!("held by you ({}): finish or hand off before you stop", prime.held.len()));
                    for t in prime.held.iter().take(10) {
                        let left = t.lease_expires_in_ms.max(0) / 60_000;
                        lines.push(format!("  {}  fence {}  lease {}m left  {}", t.id, t.fence, left, trim(&t.title, 60)));
                    }
                }
                if prime.ready.is_empty() {
                    lines.push("ready for you: nothing".into());
                } else {
                    lines.push(format!("ready for you ({}): board claim <id>, or board claim for the first", prime.ready.len()));
                    for t in &prime.ready {
                        lines.push(format!("  {}  P{}  {}", t.id, t.priority, trim(&t.title, 60)));
                    }
                }
                if !prime.attention.is_empty() {
                    lines.push(format!("needs a person ({}):", prime.attention.len()));
                    for t in &prime.attention {
                        lines.push(format!("  {}  {}", t.id, trim(&t.title, 60)));
                    }
                }
                if !prime.notes_to_me.is_empty() {
                    lines.push(format!("notes to you ({}):", prime.notes_to_me.len()));
                    for note in prime.notes_to_me.iter().take(5) {
                        lines.push(format!("  {}  from {}: {}", note.ticket, note.from, trim(&note.body, 80)));
                    }
                }
                lines.push(format!(
                    "protocol: {} Heartbeat at least every {} min. `board agent` for the whole workflow.",
                    PROTOCOL_LINE,
                    prime.heartbeat_every_ms / 60_000
                ));
                lines.join("\n")
            });
            Ok(EXIT_OK)
        }
        "ready" => {
            let ready = no_id(board.ready(actor, limit))?;
            output.value(&ready, || table(&ready));
            Ok(EXIT_OK)
        }
        "ls" => {
            let list = no_id(board.list(&Filter {
                state: parsed.flag("--state").map(str::to_string),
                label: parsed.flag("--label").map(str::to_string),
                text: parsed.flag("--text").map(str::to_string),
                open_only: parsed.switch("--open"),
                limit: Some(limit),
                ..Filter::default()
            }))?;
            output.value(&list, || table(&list));
            Ok(EXIT_OK)
        }
        "show" => {
            let id = need(0, "ID")?;
            let view = with_id(id, board.get(id))?;
            output.value(&view, || {
                let t = &view.ticket;
                let mut lines = vec![
                    format!("{}  [{}]  P{}  {}", t.id, t.state, t.priority, t.title),
                    format!(
                        "  kind {} · labels {} · holder {} · assignee {} · version {} · fence {}",
                        t.kind,
                        if t.labels.is_empty() {
                            "-".into()
                        } else {
                            t.labels.join(",")
                        },
                        t.holder.as_deref().unwrap_or("-"),
                        t.assignee.as_deref().unwrap_or("-"),
                        t.version,
                        t.fence
                    ),
                ];
                if !t.blocked_by.is_empty() {
                    lines.push(format!("  blocked by {}", t.blocked_by.join(", ")));
                }
                if !t.description.is_empty() {
                    lines.push(format!("  {}", t.description.replace('\n', "\n  ")));
                }
                for d in &view.deps {
                    lines.push(format!("  dep {} {}", d.kind.as_str(), d.depends_on));
                }
                for c in &view.children {
                    lines.push(format!("  child {c}"));
                }
                for n in &view.notes {
                    lines.push(format!(
                        "  note #{} {} by {}{}: {}",
                        n.seq,
                        n.kind,
                        n.author,
                        n.to_principal
                            .as_ref()
                            .map(|p| format!(" → {p}"))
                            .unwrap_or_default(),
                        trim(&n.body, 200)
                    ));
                }
                lines.join("\n")
            });
            Ok(EXIT_OK)
        }
        "new" => {
            let title = need(0, "TITLE")?.to_string();
            let ticket = no_id(board.create(
                actor,
                NewTicket {
                    title,
                    description: parsed.flag("--desc").unwrap_or("").to_string(),
                    kind: parsed.switch("--epic").then(|| "epic".to_string()),
                    priority: parsed.int("--priority").map_err(usage_error)?,
                    labels: parsed.flag_all("--label"),
                    parent: parsed.flag("--parent").map(str::to_string),
                    assignee: parsed.flag("--assign").map(str::to_string),
                    discovered_from: parsed.flag("--found-in").map(str::to_string),
                },
            ))?;
            output.value(&ticket, || format!("{}  {}", ticket.id, ticket.title));
            Ok(EXIT_OK)
        }
        "claim" => {
            let ttl = parsed.int("--ttl").map_err(usage_error)?.map(|s| s * 1000);
            let lease = match arg(0) {
                Some(id) => with_id(id, board.claim(actor, id, ttl))?,
                None => {
                    let filter = NextFilter {
                        label: parsed.flag("--label").map(str::to_string),
                        states: parsed.flag_all("--state"),
                        parent: parsed.flag("--parent").map(str::to_string),
                        ttl_ms: ttl,
                    };
                    match no_id(board.claim_next(actor, &filter))? {
                        Some(lease) => lease,
                        None => {
                            output.value(&serde_json::json!({ "claimed": false }), || {
                                "nothing ready for you".into()
                            });
                            return Ok(EXIT_OK);
                        }
                    }
                }
            };
            cache.claims.insert(
                lease.ticket.id.clone(),
                CachedClaim {
                    fence: lease.fence,
                    version: lease.ticket.version,
                    lease_expires_at: lease.lease_expires_at,
                },
            );
            output.value(&lease, || {
                format!(
                    "{}  claimed (fence {}, heartbeat every {}s): {}\n  board beat {} · board done {} \"summary\" · board handoff {} \"note\"",
                    lease.ticket.id,
                    lease.fence,
                    lease.heartbeat_every_ms / 1000,
                    lease.ticket.title,
                    lease.ticket.id,
                    lease.ticket.id,
                    lease.ticket.id
                )
            });
            Ok(EXIT_OK)
        }
        "beat" => {
            let id = need(0, "ID")?;
            let fence = fence_for(id)?;
            let progress = match parsed.flag("--progress") {
                Some(raw) => Some(
                    serde_json::from_str(raw)
                        .map_err(|_| usage_error("--progress must be JSON"))?,
                ),
                None => None,
            };
            let lease = with_id(id, board.heartbeat(actor, id, fence, progress))?;
            cache.claims.insert(
                id.to_string(),
                CachedClaim {
                    fence: lease.fence,
                    version: lease.ticket.version,
                    lease_expires_at: lease.lease_expires_at,
                },
            );
            output.value(&lease, || {
                format!("{id}  lease extended to {}", lease.lease_expires_at)
            });
            Ok(EXIT_OK)
        }
        "note" => {
            let id = need(0, "ID")?;
            let text = need(1, "TEXT")?;
            let fence = parsed
                .int("--fence")
                .map_err(usage_error)?
                .or_else(|| cache.claims.get(id).map(|c| c.fence));
            let note = with_id(
                id,
                board.add_note(
                    actor,
                    id,
                    parsed.flag("--kind").unwrap_or("note"),
                    text,
                    parsed.flag("--to"),
                    fence,
                ),
            )?;
            output.value(&note, || format!("{id}  note #{}", note.seq));
            Ok(EXIT_OK)
        }
        "done" => {
            let id = need(0, "ID")?;
            let fence = fence_for(id)?;
            let ticket = with_id(id, board.complete(actor, id, fence, arg(1).unwrap_or("")))?;
            cache.claims.remove(id);
            output.value(&ticket, || format!("{id}  → {}", ticket.state));
            Ok(EXIT_OK)
        }
        "handoff" => {
            let id = need(0, "ID")?;
            let note = need(1, "NOTE")?;
            let fence = fence_for(id)?;
            let ticket = with_id(
                id,
                board.handoff(actor, id, fence, note, parsed.flag("--to")),
            )?;
            cache.claims.remove(id);
            output.value(&ticket, || {
                format!(
                    "{id}  → {} (assigned to {})",
                    ticket.state,
                    ticket.assignee.as_deref().unwrap_or("anyone")
                )
            });
            Ok(EXIT_OK)
        }
        "release" => {
            let id = need(0, "ID")?;
            let fence = fence_for(id)?;
            let ticket = with_id(id, board.release(actor, id, fence, arg(1).unwrap_or("")))?;
            cache.claims.remove(id);
            output.value(&ticket, || format!("{id}  → {}", ticket.state));
            Ok(EXIT_OK)
        }
        "force" => {
            let id = need(0, "ID")?;
            let reason = need(1, "REASON")?;
            let ticket = with_id(id, board.force_release(actor, id, reason))?;
            output.value(&ticket, || {
                format!("{id}  → {} (lease taken away)", ticket.state)
            });
            Ok(EXIT_OK)
        }
        "set" => {
            let id = need(0, "ID")?;
            let version = parsed
                .int("--version")
                .map_err(usage_error)?
                .ok_or_else(|| usage_error("--version V is required (board show ID --json)"))?;
            let patch = Patch {
                title: parsed.flag("--title").map(str::to_string),
                description: parsed.flag("--desc").map(str::to_string),
                priority: parsed.int("--priority").map_err(usage_error)?,
                labels: parsed.flags.get("--label").cloned(),
                assignee: parsed
                    .flag("--assign")
                    .map(|a| if a == "-" { None } else { Some(a.to_string()) }),
                state: parsed.flag("--state").map(str::to_string),
            };
            let ticket = with_id(id, board.update(actor, id, version, patch))?;
            output.value(&ticket, || {
                format!("{id}  version {} [{}]", ticket.version, ticket.state)
            });
            Ok(EXIT_OK)
        }
        "dep" => {
            let id = need(0, "ID")?;
            let on = parsed
                .flag("--on")
                .ok_or_else(|| usage_error("--on ID is required"))?;
            let kind = match parsed.flag("--kind") {
                None => DepKind::Blocks,
                Some(k) => DepKind::parse(k)
                    .ok_or_else(|| usage_error(format!("unknown dependency kind {k}")))?,
            };
            let deps = if parsed.switch("--remove") {
                with_id(id, board.remove_dep(actor, id, on, kind))?
            } else {
                with_id(id, board.add_dep(actor, id, on, kind))?
            };
            output.value(&deps, || {
                deps.iter()
                    .map(|d| format!("{} {} {}", d.ticket, d.kind.as_str(), d.depends_on))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(EXIT_OK)
        }
        "reap" => {
            let n = no_id(board.reap())?;
            output.value(&serde_json::json!({ "expired": n }), || {
                format!("{n} lease(s) expired")
            });
            Ok(EXIT_OK)
        }
        "verify" => {
            let status = no_id(board.verify_chain())?;
            output.value(&status, || {
                format!(
                    "{} events · chain {} · head {}",
                    status.entries,
                    if status.ok { "ok" } else { "BROKEN" },
                    &status.head[..12.min(status.head.len())]
                )
            });
            Ok(if status.ok { EXIT_OK } else { EXIT_REFUSED })
        }
        "export" => {
            let path = parsed
                .flag("--out")
                .map(PathBuf::from)
                .unwrap_or_else(|| board_dir.join("board.jsonl"));
            let tickets = no_id(board.list(&Filter {
                limit: Some(100_000),
                ..Filter::default()
            }))?;
            let mut text = String::new();
            for ticket in &tickets {
                let view = with_id(&ticket.id, board.get(&ticket.id))?;
                text.push_str(&serde_json::to_string(&view).unwrap_or_default());
                text.push('\n');
            }
            std::fs::write(&path, text)
                .map_err(|e| (BridgeError::new("board_io", e.to_string()), None))?;
            output.value(
                &serde_json::json!({ "path": path, "tickets": tickets.len() }),
                || format!("{} tickets → {}", tickets.len(), path.display()),
            );
            Ok(EXIT_OK)
        }
        other => Err(usage_error(format!("unknown command {other}"))),
    }
}

fn table(tickets: &[Ticket]) -> String {
    if tickets.is_empty() {
        return "(none)".into();
    }
    tickets
        .iter()
        .map(|t| {
            format!(
                "{:<14} {:<16} P{} {:<10} {}",
                t.id,
                t.state,
                t.priority,
                t.holder.as_deref().unwrap_or("-"),
                trim(&t.title, 70)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn trim(text: &str, max: usize) -> String {
    let one_line = text.lines().next().unwrap_or("");
    if one_line.chars().count() <= max {
        one_line.to_string()
    } else {
        format!("{}…", one_line.chars().take(max - 1).collect::<String>())
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> BridgeResult<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| BridgeError::new("board_io", e.to_string()))?;
    std::fs::write(path, text + "\n")
        .map_err(|e| BridgeError::new("board_io", format!("{}: {e}", path.display())))
}

fn cache_path(board_dir: &Path, actor: &str) -> PathBuf {
    let safe: String = actor
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    board_dir.join("claims").join(format!("{safe}.json"))
}

fn read_cache(board_dir: &Path, actor: &str) -> ClaimCache {
    std::fs::read_to_string(cache_path(board_dir, actor))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_cache(board_dir: &Path, actor: &str, cache: &ClaimCache) {
    let path = cache_path(board_dir, actor);
    if cache.claims.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = write_json(&path, cache);
}

pub const PROTOCOL_LINE: &str =
    "claim → beat → note → done (or handoff with a note). Never stop holding a ticket.";

pub const AGENTS_MD_START: &str = "<!-- devagotchi-board:start -->";
pub const AGENTS_MD_END: &str = "<!-- devagotchi-board:end -->";

pub fn agents_md_block() -> String {
    format!(
        "{AGENTS_MD_START}
## Board

This project's tickets live in `.devagotchi/board.sqlite3` (the agents' board; contract in the
devagotchi repository, `docs/agent-board.md`). Work from the board, not from memory:

- Start every session with `devagotchi-hub board prime` (set `DEVAGOTCHI_ACTOR=<your-name>` first,
  for example `claude`, `codex`, `gemini`, `cursor`, `grok`): it lists what you hold, what is ready
  for you, and notes addressed to you.
- Take work with `board claim` (or `board claim <id>`), heartbeat with `board beat <id>` at least as
  often as `prime` says, record what you learn with `board note <id> \"…\"`, finish with
  `board done <id> \"summary\"`, or `board handoff <id> \"note\" --to <who>` when someone else must
  continue. Work you discover goes on the board: `board new \"title\" --found-in <id>`.
- Never stop while you hold a ticket: hand it off or finish it. Reviewers are never the author.
- Every command takes `--json`; errors carry a `next` field that says what to run.
{AGENTS_MD_END}"
    )
}

pub const AGENT_WORKFLOW: &str = "The board workflow for an agent

1. prime      devagotchi-hub board prime            what you hold, what is ready for you, notes to you
2. claim      board claim [<id>]                   a lease with a fence; re-claiming what you hold is free
3. work       board beat <id> [--progress '{...}']  before the heartbeat interval runs out, every time
              board note <id> \"what you learned\"  notes are append-only and travel with the ticket
              board new \"title\" --found-in <id>   work you discovered, linked back, never silently done
4. finish     board done <id> \"summary\"           → the next lane (review); someone else reviews
   or hand off board handoff <id> \"note\" --to <who>
   or release board release <id> \"reason\"         back to the queue, nothing recorded as done
5. blocked?   board dep <id> --on <other>           readiness follows `blocks` dependencies
              board set <id> --version V --state blocked   a person moves tickets by hand

Rules the store enforces, not you: a stale fence is refused (your lease lapsed; claim again);
the reviewer must differ from the author; a lapsed lease goes back to the queue, three lapses
send the ticket to a person; WIP is limited; states change only through these verbs.
Exit codes: 0 ok · 2 conflict · 3 refused · 4 not found · 5 board unavailable · 64 usage.";
