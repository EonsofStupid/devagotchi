//! `devagotchi`: the one binary. A nested command tree over the same libraries the
//! separate binaries are built from — the process boundaries do not move (the broker
//! owns jobs, `mcp` runs only the adapter's code, the hub never admits a job); only the
//! packaging does, so a person downloads one thing and runs one command.
//!
//! ```text
//! devagotchi setup                 every AI client here, in one step, no account needed
//! devagotchi start                 this user's broker, no paths to remember
//! devagotchi mcp                   the stdio MCP adapter an agent runs; finds that broker
//! devagotchi connect               point Claude Code, Codex and Claude Desktop at it
//! devagotchi board <verb> …        the project's ticket board
//! devagotchi broker run …          the broker with explicit paths
//! devagotchi hub serve|enroll|revoke|devices …
//! devagotchi link enroll|run …
//! devagotchi version
//! ```

mod paths;
mod start;

use std::{ffi::OsString, process::ExitCode};

pub const USAGE: &str =
    "devagotchi — your machines, reachable by your AI; your work, on a board every agent shares.

Usage: devagotchi <command> [options]

  setup        set Devagotchi up for every AI client here in one step, no account needed
  start        run this user's broker (first run writes its config); Ctrl+C stops it
               (start --background: detached; `mcp` does this itself when none is running)
  mcp          the stdio MCP adapter your agent runs; finds your broker, or starts it
               (mcp --hub [URL]: the hosted hub instead, as the account you signed in with)
  connect      point Claude Code, Codex and Claude Desktop at Devagotchi, then check it answers
  board        the project's ticket board: prime, claim, beat, note, done, handoff … (board --help)
  broker       the broker with explicit paths: broker run --state-dir DIR --config FILE
  hub          the hub: hub serve | enroll | revoke | devices (hub --help)
  link         this machine's outbound link to a hub: link enroll | run (link --help)
  login        sign in with your Shippin account (login --local: no account, this machine only)
  logout       revoke the tokens and forget the account
  whoami       who this machine is signed in as
  version      print the version
  help [cmd]   this text, or a command's own

Every command answers --help. Exit codes: 0 ok · 64 usage · others as the command documents.";

fn main() -> ExitCode {
    run(std::env::args_os().skip(1).collect())
}

fn run(args: Vec<OsString>) -> ExitCode {
    let mut args = args;
    let Some(command) = args.first().cloned() else {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    };
    args.remove(0);
    match command.to_str() {
        Some("start") => start::run(args),
        Some("mcp") => mcp(args),
        Some("connect") => connect(args, false),
        Some("setup") => connect(args, true),
        Some("board") => devagotchi_hub::board_cli(
            args,
            paths::config_dir().map(|d| d.join(devagotchi_hub::account::FILE_NAME)),
        ),
        Some("broker") => devagotchi_broker::cli(args),
        Some("hub") => devagotchi_hub::cli(args),
        Some("link") => devagotchi_link::cli(args),
        Some(verb @ ("login" | "logout" | "whoami")) => account(verb, args),
        Some("version" | "--version" | "-V") => {
            println!("devagotchi {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help" | "--help" | "-h") => match args.first().and_then(|a| a.to_str()) {
            None => {
                println!("{USAGE}");
                ExitCode::SUCCESS
            }
            Some(topic) => run(vec![OsString::from(topic), OsString::from("--help")]),
        },
        _ => {
            eprintln!(
                "devagotchi: unknown command {:?}\n\n{USAGE}",
                command.to_string_lossy()
            );
            ExitCode::from(64)
        }
    }
}

/// `devagotchi login|logout|whoami`: the account file lives beside the broker's config.
fn account(verb: &str, args: Vec<OsString>) -> ExitCode {
    let Some(config_dir) = paths::config_dir() else {
        eprintln!(
            "devagotchi {verb}: no home directory in the environment; set DEVAGOTCHI_CONFIG_DIR"
        );
        return ExitCode::from(64);
    };
    devagotchi_hub::account::cli(
        verb,
        args,
        config_dir.join(devagotchi_hub::account::FILE_NAME),
    )
}

/// `devagotchi connect` and `devagotchi setup`: the machine as the clients' config files
/// and PATH describe it.
fn connect(args: Vec<OsString>, setup: bool) -> ExitCode {
    let exe = match std::env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(exe) => dunce_path(exe),
        Err(error) => {
            eprintln!("devagotchi connect: cannot find this binary: {error}");
            return ExitCode::from(1);
        }
    };
    let home = paths::home();
    let env_dir = |name: &str| {
        std::env::var_os(name)
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let app_config = if cfg!(windows) {
        env_dir("APPDATA")
    } else if cfg!(target_os = "macos") {
        home.as_ref()
            .map(|h| h.join("Library").join("Application Support"))
    } else {
        env_dir("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config")))
    };
    let signed_in = paths::config_dir()
        .map(|d| d.join(devagotchi_hub::account::FILE_NAME))
        .is_some_and(|f| devagotchi_hub::account::signed_in(&f));
    let machine = devagotchi_mcp::connect::Machine {
        exe,
        home,
        codex_home: env_dir("CODEX_HOME"),
        app_config,
        path: std::env::var_os("PATH"),
        signed_in,
    };
    if setup {
        devagotchi_mcp::connect::setup(args, machine)
    } else {
        devagotchi_mcp::connect::cli(args, machine)
    }
}

/// Windows `canonicalize` answers `\\?\C:\…`, which other programs may not accept.
fn dunce_path(path: std::path::PathBuf) -> std::path::PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => std::path::PathBuf::from(rest),
        _ => path,
    }
}

/// This user's broker for `devagotchi mcp`: the one answering at `<state>/endpoint`, or,
/// when none answers, one started in the background (`devagotchi start --background`, run as
/// its own process so the broker is not left a child of the AI client's adapter).
#[cfg(any(target_os = "linux", windows))]
fn local_broker(state: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if let Some(endpoint) = devagotchi_broker::background::answering(state) {
        return Ok(endpoint);
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
    // stdout is the MCP stream: the starter writes only to stderr, and that is kept away.
    let started = std::process::Command::new(exe)
        .args(["start", "--background"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("could not start your broker: {e}"))?;
    devagotchi_broker::background::answering(state).ok_or_else(|| {
        format!(
            "your broker did not start ({started}); see {}",
            state
                .join(devagotchi_broker::background::LOG_FILE)
                .display()
        )
    })
}

/// Where no execution provider exists yet there is no broker to start.
#[cfg(not(any(target_os = "linux", windows)))]
fn local_broker(state: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let file = state.join(devagotchi_broker::ENDPOINT_FILE);
    match std::fs::read_to_string(&file) {
        Ok(text) if !text.trim().is_empty() => Ok(std::path::PathBuf::from(text.trim())),
        _ => Err(format!(
            "no broker is running for this user ({} is missing); run `devagotchi start` first, or pass --socket ENDPOINT",
            file.display()
        )),
    }
}

/// `devagotchi mcp [--socket ENDPOINT]`: without the flag, this user's broker, started in the
/// background when none answers, so an agent's whole MCP config is `devagotchi mcp`.
/// `devagotchi mcp --hub [URL]`: the hosted hub's MCP instead, as the account on file.
fn mcp(args: Vec<OsString>) -> ExitCode {
    if args.first().is_some_and(|a| a == "--hub") {
        let hub = match args.get(1).map(|a| a.to_string_lossy().into_owned()) {
            Some(url) if url.starts_with('-') => {
                eprintln!("devagotchi mcp: --hub takes a URL, got {url}");
                return ExitCode::from(64);
            }
            other => other,
        };
        if args.len() > 1 + usize::from(hub.is_some()) {
            eprintln!("devagotchi mcp: --hub takes nothing but a URL");
            return ExitCode::from(64);
        }
        let Some(config_dir) = paths::config_dir() else {
            eprintln!(
                "devagotchi mcp: no home directory in the environment; set DEVAGOTCHI_CONFIG_DIR"
            );
            return ExitCode::from(64);
        };
        let seat = std::env::var("DEVAGOTCHI_ACTOR")
            .ok()
            .filter(|a| !a.trim().is_empty());
        return devagotchi_hub::account::front::cli(
            hub.as_deref(),
            &config_dir.join(devagotchi_hub::account::FILE_NAME),
            seat.as_deref(),
        );
    }
    if args.is_empty() {
        let Some(state) = paths::state_dir() else {
            eprintln!(
                "devagotchi mcp: no home directory in the environment; pass --socket ENDPOINT or set DEVAGOTCHI_STATE_DIR"
            );
            return ExitCode::from(64);
        };
        let endpoint = match local_broker(&state) {
            Ok(endpoint) => endpoint,
            Err(why) => {
                eprintln!("devagotchi mcp: {why}");
                return ExitCode::from(69);
            }
        };
        return devagotchi_mcp::cli(vec![OsString::from("--socket"), endpoint.into_os_string()]);
    }
    devagotchi_mcp::cli(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_routes_help_and_refuses_the_unknown() {
        assert_eq!(run(vec![]), ExitCode::SUCCESS);
        assert_eq!(run(vec!["help".into()]), ExitCode::SUCCESS);
        assert_eq!(run(vec!["version".into()]), ExitCode::SUCCESS);
        assert_eq!(run(vec!["help".into(), "board".into()]), ExitCode::SUCCESS);
        assert_eq!(run(vec!["hub".into(), "--help".into()]), ExitCode::SUCCESS);
        assert_eq!(run(vec!["link".into(), "--help".into()]), ExitCode::SUCCESS);
        assert_eq!(
            run(vec!["broker".into(), "--help".into()]),
            ExitCode::SUCCESS
        );
        assert_eq!(run(vec!["mcp".into(), "--help".into()]), ExitCode::SUCCESS);
        for verb in ["login", "logout", "whoami"] {
            assert_eq!(run(vec![verb.into(), "--help".into()]), ExitCode::SUCCESS);
        }
        assert_eq!(
            run(vec!["start".into(), "--help".into()]),
            ExitCode::SUCCESS
        );
        assert_eq!(run(vec!["frobnicate".into()]), ExitCode::from(64));
        assert_eq!(
            run(vec!["broker".into(), "run".into(), "--state-dir".into()]),
            ExitCode::from(64)
        );
    }
}
