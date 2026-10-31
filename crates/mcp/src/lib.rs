//! Thin MCP stdio adapter. The broker owns admission, validation, and job lifetime.
//! This library is its whole behaviour, `devagotchi mcp` its name. It never links the
//! broker: `devagotchi mcp` runs only this code.

pub mod connect;
pub mod files;
pub mod run;

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use devagotchi_contracts::{BridgeError, BridgeResult, ResponseBody};

pub const USAGE: &str = "Usage:\n  devagotchi mcp --socket ABS_PATH\n  devagotchi mcp --hub [URL]\n  devagotchi mcp --help\n\nConnects MCP tools to an already running local broker.\nABS_PATH is the endpoint on the broker's ready line: a Unix socket path, or a\n\\\\.\\pipe\\ named-pipe path on Windows. Without --socket, `devagotchi mcp` uses this user's own broker.\n--hub relays the session to the hosted hub's MCP (default https://app.devagotchi.com) as the\naccount `devagotchi login` signed in with; the token stays on this machine.";

enum Command {
    Help,
    Serve { socket: PathBuf },
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> BridgeResult<Command> {
    let args: Vec<OsString> = args.into_iter().collect();
    match args.as_slice() {
        [flag] if flag == "--help" => Ok(Command::Help),
        [flag, socket] if flag == "--socket" => {
            let socket = PathBuf::from(socket);
            if !socket.is_absolute() {
                return Err(BridgeError::new(
                    "invalid_cli",
                    "--socket requires an absolute filesystem path",
                ));
            }
            Ok(Command::Serve { socket })
        }
        _ => Err(BridgeError::new(
            "invalid_cli",
            "expected exactly --socket ABS_PATH, or --help",
        )),
    }
}

/// The adapter's command line: `--socket ABS_PATH`, or `--help`.
pub fn cli(args: Vec<OsString>) -> ExitCode {
    let socket = match parse_args(args) {
        Ok(Command::Help) => {
            // CLI help is the only non-protocol output permitted on stdout.
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Serve { socket }) => socket,
        Err(error) => {
            eprintln!("devagotchi mcp: {error}\n\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    match serve_blocking(socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("devagotchi mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Serve MCP on this process's stdio against the broker at `socket`, on a runtime of its own.
pub fn serve_blocking(socket: PathBuf) -> BridgeResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| BridgeError::new("mcp_runtime", error.to_string()))?;
    runtime.block_on(serve(socket))
}

pub async fn serve(socket: PathBuf) -> BridgeResult<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let service = adapter::BridgeAdapter::new(socket)
        .serve(stdio())
        .await
        .map_err(|error| BridgeError::new("mcp_start_failed", error.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|error| BridgeError::new("mcp_service_failed", error.to_string()))?;
    Ok(())
}

mod adapter;

/// The kind of broker answer `actual` is, by name.
pub fn response_kind(actual: &ResponseBody) -> &'static str {
    match actual {
        ResponseBody::Start(_) => "start",
        ResponseBody::Operation(_) => "operation",
        ResponseBody::Output(_) => "output",
        ResponseBody::Operations(_) => "operations",
        ResponseBody::File(_) => "file",
        ResponseBody::Listing(_) => "listing",
        ResponseBody::Written(_) => "written",
        ResponseBody::Revision(_) => "revision",
        ResponseBody::Info(_) => "info",
    }
}

/// A broker answered with another kind of response than the request calls for.
pub fn unexpected(expected: &str, actual: &ResponseBody) -> BridgeError {
    BridgeError::new(
        "unexpected_broker_response",
        format!(
            "expected broker response kind {expected}, received {}",
            response_kind(actual)
        ),
    )
}
