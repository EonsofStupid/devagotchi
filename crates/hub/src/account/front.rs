//! `devagotchi mcp --hub [URL]`: the account carried into an agent's MCP session (14c).
//!
//! An agent whose MCP client can sign in itself talks to the hub's HTTP endpoint
//! directly. One that cannot — a headless seat, a client that only takes a stdio
//! `command` — runs this instead: a stdio MCP server that relays every message to the
//! hub's `/mcp` through one `client::Session` as the account on file, so the token never
//! appears in the agent's config and rotates underneath it. The hub sees an ordinary
//! signed-in session; `whoami` there is the account, and `DEVAGOTCHI_ACTOR` names the seat
//! (`X-Devagotchi-Seat`) so one account's agents are distinct on its boards.
//!
//! A JSON or SSE answer is written to stdout as the JSON-RPC lines it carries; a
//! notification is posted and nothing is written; a refused, expired or missing account is
//! answered on the request's id, in plain words.

use std::{path::Path, process::ExitCode};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::client::{Refused, Session};

pub use super::client::DEFAULT_HUB;
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// A relay to one hub as one account.
pub struct Front {
    session: Session,
}

impl Front {
    pub fn new(hub: &str, account_file: &Path, seat: Option<&str>) -> Result<Self, String> {
        Ok(Self {
            session: Session::new(hub, account_file, seat)?,
        })
    }

    /// Relay one client message; the JSON-RPC lines to write back, if any.
    pub async fn relay(&mut self, line: &str) -> Result<Vec<String>, String> {
        let message: Value =
            serde_json::from_str(line).map_err(|e| format!("not JSON-RPC: {e}"))?;
        let id = message.get("id").cloned();
        match self.session.post(&message).await {
            Ok(answer) => Ok(answer.messages.iter().map(Value::to_string).collect()),
            Err(refused) => Ok(error_lines(id, &refused)),
        }
    }
}

/// A JSON-RPC error for the request `id`, as the hub would have answered; nothing for a
/// notification, which has no id to answer.
fn error_lines(id: Option<Value>, refused: &Refused) -> Vec<String> {
    let code = match refused.code {
        "not_signed_in" => -32000,
        "hub_unreachable" => -32001,
        "signed_out" => -32003,
        _ => -32005,
    };
    match id {
        Some(id) if !id.is_null() => vec![
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": refused.message } })
                .to_string(),
        ],
        _ => {
            eprintln!("devagotchi mcp --hub: {}", refused.message);
            Vec::new()
        }
    }
}

/// Serve stdio until the client closes it.
pub async fn serve(mut front: Front) -> Result<(), String> {
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    loop {
        let line = match stdin.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) => return Err(format!("stdin: {error}")),
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            eprintln!("devagotchi mcp --hub: a message over 4 MiB was dropped");
            continue;
        }
        for mut out in front.relay(&line).await? {
            out.push('\n');
            stdout
                .write_all(out.as_bytes())
                .await
                .map_err(|e| format!("stdout: {e}"))?;
            stdout.flush().await.map_err(|e| format!("stdout: {e}"))?;
        }
    }
}

/// `devagotchi mcp --hub [URL]`; `seat` is `DEVAGOTCHI_ACTOR` when set.
pub fn cli(hub: Option<&str>, account_file: &Path, seat: Option<&str>) -> ExitCode {
    let hub = hub.unwrap_or(DEFAULT_HUB);
    let front = match Front::new(hub, account_file, seat) {
        Ok(front) => front,
        Err(why) => {
            eprintln!("devagotchi mcp --hub: {why}");
            return ExitCode::from(64);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("devagotchi mcp --hub: runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "devagotchi mcp --hub: relaying stdio to {hub}/mcp as the account on file{}",
        seat.map(|s| format!(", seat {s}")).unwrap_or_default()
    );
    match runtime.block_on(serve(front)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("devagotchi mcp --hub: {why}");
            ExitCode::FAILURE
        }
    }
}
