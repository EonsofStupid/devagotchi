//! `devagotchi mcp --hub [URL]`: the account carried into an agent's MCP session (14c).
//!
//! An agent whose MCP client can sign in itself talks to the hub's HTTP endpoint
//! directly. One that cannot — a headless seat, a client that only takes a stdio
//! `command` — runs this instead: a stdio MCP server that relays every message to the
//! hub's `/mcp` with the bearer token on file (`account.json`, kept fresh by refresh), so
//! the token never appears in the agent's config and rotates underneath it. The hub sees an
//! ordinary signed-in session; `whoami` there is the account.
//!
//! Streamable HTTP, client side: each request is one POST; the hub's session id from
//! `initialize` rides along on every later message; a JSON or SSE answer is written to
//! stdout as the JSON-RPC lines it carries; a notification is posted and nothing is
//! written. The hub never pushes to a client of its own accord (approvals are waited on
//! with `approval_wait`), so no GET stream is opened.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{Standing, Stored, current, unix_now};

pub const DEFAULT_HUB: &str = "https://devagotchi.com";
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;

/// A relay to one hub as one account.
pub struct Front {
    endpoint: String,
    account_file: PathBuf,
    http: reqwest::Client,
    session: Option<String>,
}

impl Front {
    pub fn new(hub: &str, account_file: &Path) -> Result<Self, String> {
        let hub = hub.trim_end_matches('/');
        if !(hub.starts_with("https://") || hub.starts_with("http://127.0.0.1")) {
            return Err("the hub must be an https:// URL".into());
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(310))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            endpoint: format!("{hub}/mcp"),
            account_file: account_file.to_path_buf(),
            http,
            session: None,
        })
    }

    /// The access token to send now: refreshed when due, refused when there is none.
    async fn token(&self) -> Result<String, String> {
        match current(&self.account_file).await? {
            Some(Stored::Account(account)) => match account.standing(unix_now()) {
                Standing::Expired => Err(format!(
                    "the account on file ({}) has expired; run `devagotchi login`",
                    account.display_name()
                )),
                _ => Ok(account.access_token().to_string()),
            },
            Some(Stored::Local { .. }) => {
                Err("this machine runs without an account (`devagotchi login` signs in)".into())
            }
            None => Err("not signed in: run `devagotchi login` first".into()),
        }
    }

    /// Relay one client message; the JSON-RPC lines to write back, if any.
    pub async fn relay(&mut self, line: &str) -> Result<Vec<String>, String> {
        let message: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("not JSON-RPC: {e}"))?;
        let id = message.get("id").cloned();
        let token = match self.token().await {
            Ok(token) => token,
            Err(why) => return Ok(error_lines(id, -32000, &why)),
        };
        let mut request = self
            .http
            .post(&self.endpoint)
            .bearer_auth(token)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-06-18")
            .json(&message);
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session.as_str());
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                return Ok(error_lines(
                    id,
                    -32001,
                    &format!("the hub is unreachable: {error}"),
                ));
            }
        };
        let status = response.status();
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(session.to_string());
        }
        let sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("reading the hub's answer: {error}"))?;
        if body.len() > MAX_ANSWER_BYTES {
            return Ok(error_lines(id, -32002, "the hub's answer is too large"));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(error_lines(
                id,
                -32003,
                "the hub refused the account's token (signed out there, or not let in); `devagotchi whoami`, then `devagotchi login`",
            ));
        }
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(error_lines(
                id,
                -32004,
                "the hub cannot reach its identity provider right now; try again shortly",
            ));
        }
        if status == reqwest::StatusCode::ACCEPTED || body.is_empty() {
            return Ok(Vec::new());
        }
        if !status.is_success() {
            return Ok(error_lines(
                id,
                -32005,
                &format!("the hub answered HTTP {status}"),
            ));
        }
        let text = String::from_utf8_lossy(&body);
        let lines = if sse {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .collect()
        } else {
            vec![text.trim().to_string()]
        };
        Ok(lines)
    }
}

/// A JSON-RPC error for the request `id`, as the hub would have answered; nothing for a
/// notification, which has no id to answer.
fn error_lines(id: Option<serde_json::Value>, code: i64, message: &str) -> Vec<String> {
    match id {
        Some(id) if !id.is_null() => vec![
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
                .to_string(),
        ],
        _ => {
            eprintln!("devagotchi mcp --hub: {message}");
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

/// `devagotchi mcp --hub [URL]`.
pub fn cli(hub: Option<&str>, account_file: PathBuf) -> ExitCode {
    let hub = hub.unwrap_or(DEFAULT_HUB);
    let front = match Front::new(hub, &account_file) {
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
    eprintln!("devagotchi mcp --hub: relaying stdio to {hub}/mcp as the account on file");
    match runtime.block_on(serve(front)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("devagotchi mcp --hub: {why}");
            ExitCode::FAILURE
        }
    }
}
