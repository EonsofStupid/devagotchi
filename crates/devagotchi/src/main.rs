//! `devagotchi`: the one binary. A nested command tree over the same libraries the
//! separate binaries are built from — the process boundaries do not move (the broker
//! owns jobs, `mcp` runs only the adapter's code, the hub never admits a job); only the
//! packaging does, so a person downloads one thing and runs one command.
//!
//! ```text
//! devagotchi start                 this user's broker, no paths to remember
//! devagotchi mcp                   the stdio MCP adapter an agent runs; finds that broker
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

  start        run this user's broker (first run writes its config); Ctrl+C stops it
  mcp          the stdio MCP adapter your agent runs; finds the broker `start` left running
               (mcp --hub [URL]: the hosted hub instead, as the account you signed in with)
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
        Some("board") => devagotchi_hub::board_cli(args),
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

/// `devagotchi mcp [--socket ENDPOINT]`: without the flag, the endpoint `devagotchi start`
/// wrote to `<state>/endpoint`, so an agent's MCP config is just `devagotchi mcp`.
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
        return devagotchi_hub::account::front::cli(
            hub.as_deref(),
            config_dir.join(devagotchi_hub::account::FILE_NAME),
        );
    }
    if args.is_empty() {
        let Some(state) = paths::state_dir() else {
            eprintln!(
                "devagotchi mcp: no home directory in the environment; pass --socket ENDPOINT or set DEVAGOTCHI_STATE_DIR"
            );
            return ExitCode::from(64);
        };
        let file = state.join(devagotchi_broker::ENDPOINT_FILE);
        let endpoint = match std::fs::read_to_string(&file) {
            Ok(text) if !text.trim().is_empty() => text.trim().to_string(),
            _ => {
                eprintln!(
                    "devagotchi mcp: no broker is running for this user ({} is missing); run `devagotchi start` first, or pass --socket ENDPOINT",
                    file.display()
                );
                return ExitCode::from(69);
            }
        };
        return devagotchi_mcp::cli(vec![OsString::from("--socket"), OsString::from(endpoint)]);
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
