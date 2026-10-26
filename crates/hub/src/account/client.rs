//! One MCP session with a hub, as the account on file: the client side of Streamable
//! HTTP. `devagotchi mcp --hub` relays an agent's stdio through it (`front`); the board's
//! remote face (`board::remote`) calls tools through it. The token comes from
//! `account.json` at each request — refreshed when due, so a long session rides over a
//! rotation — and the seat (`X-Devagotchi-Seat`) names the account's agent on this machine.
//!
//! Each request is one POST; the hub's session id from `initialize` rides along on every
//! later message; a JSON or SSE answer is returned as the JSON-RPC messages it carries; a
//! notification is posted and yields nothing. The hub never pushes to a client of its own
//! accord (approvals are waited on with `approval_wait`), so no GET stream is opened.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use serde_json::Value;

use super::{Standing, Stored, current, unix_now};
use crate::board::mcp::SEAT_HEADER;

/// The hosted hub (D22: devagotchi.com is the site, app.devagotchi.com the app and /mcp).
/// devagotchi.com redirects there, and the relay never follows redirects.
pub const DEFAULT_HUB: &str = "https://app.devagotchi.com";
const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Why a message did not reach the hub as the account, in the words a caller shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// A stable code: `not_signed_in`, `signed_out`, `hub_unreachable`, `hub_error`.
    pub code: &'static str,
    pub message: String,
}

impl Refused {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<Refused> for BridgeError {
    fn from(refused: Refused) -> Self {
        BridgeError::new(refused.code, refused.message)
    }
}

/// A session with one hub as one account (and seat).
pub struct Session {
    hub: String,
    endpoint: String,
    account_file: PathBuf,
    seat: Option<String>,
    http: reqwest::Client,
    session: Option<String>,
    initialized: bool,
    next_id: u64,
}

/// What one posted message came back as.
pub struct Answer {
    pub status: u16,
    /// The JSON-RPC messages in the body, in order (none for a notification's 202).
    pub messages: Vec<Value>,
}

impl Session {
    pub fn new(hub: &str, account_file: &Path, seat: Option<&str>) -> Result<Self, String> {
        let hub = hub.trim_end_matches('/');
        if !(hub.starts_with("https://") || hub.starts_with("http://127.0.0.1")) {
            return Err("the hub must be an https:// URL".into());
        }
        if let Some(seat) = seat
            && !crate::config::valid_name(seat)
        {
            return Err(format!(
                "seat {seat:?} is not a name like a board's: [a-z0-9][a-z0-9-]{{0,62}}"
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(310))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            hub: hub.to_string(),
            endpoint: format!("{hub}/mcp"),
            account_file: account_file.to_path_buf(),
            seat: seat.map(str::to_string),
            http,
            session: None,
            initialized: false,
            next_id: 1,
        })
    }

    pub fn hub(&self) -> &str {
        &self.hub
    }

    /// The access token to send now: refreshed when due, refused when there is none.
    async fn token(&self) -> Result<String, Refused> {
        match current(&self.account_file)
            .await
            .map_err(|why| Refused::new("not_signed_in", why))?
        {
            Some(Stored::Account(account)) => match account.standing(unix_now()) {
                Standing::Expired => Err(Refused::new(
                    "signed_out",
                    format!(
                        "the account on file ({}) has expired; run `devagotchi login`",
                        account.display_name()
                    ),
                )),
                _ => Ok(account.access_token().to_string()),
            },
            Some(Stored::Local { .. }) => Err(Refused::new(
                "not_signed_in",
                "this machine runs without an account (`devagotchi login` signs in)",
            )),
            None => Err(Refused::new(
                "not_signed_in",
                "not signed in: run `devagotchi login` first",
            )),
        }
    }

    /// Post one JSON-RPC message as the account; the hub's answer, parsed.
    pub async fn post(&mut self, message: &Value) -> Result<Answer, Refused> {
        let token = self.token().await?;
        let mut request = self
            .http
            .post(&self.endpoint)
            .bearer_auth(token)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(message);
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session.as_str());
        }
        if let Some(seat) = &self.seat {
            request = request.header(SEAT_HEADER, seat.as_str());
        }
        let response = request.send().await.map_err(|error| {
            Refused::new(
                "hub_unreachable",
                format!("{} is unreachable: {error}", self.hub),
            )
        })?;
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
        let body = response.bytes().await.map_err(|error| {
            Refused::new("hub_error", format!("reading the hub's answer: {error}"))
        })?;
        if body.len() > MAX_ANSWER_BYTES {
            return Err(Refused::new("hub_error", "the hub's answer is too large"));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Refused::new(
                "signed_out",
                "the hub refused the account's token (signed out there, or not let in); `devagotchi whoami`, then `devagotchi login`",
            ));
        }
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Err(Refused::new(
                "hub_unreachable",
                "the hub cannot reach its identity provider right now; try again shortly",
            ));
        }
        if status == reqwest::StatusCode::ACCEPTED || body.is_empty() {
            return Ok(Answer {
                status: status.as_u16(),
                messages: Vec::new(),
            });
        }
        if !status.is_success() {
            return Err(Refused::new(
                "hub_error",
                format!("the hub answered HTTP {status}"),
            ));
        }
        let text = String::from_utf8_lossy(&body);
        let raw: Vec<&str> = if sse {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .collect()
        } else {
            vec![text.trim()]
        };
        let messages = raw
            .into_iter()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Value>, _>>()
            .map_err(|_| Refused::new("hub_error", "the hub's answer is not JSON-RPC"))?;
        Ok(Answer {
            status: status.as_u16(),
            messages,
        })
    }

    /// A request by this client's own numbering: the reply with that id.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, Refused> {
        let id = self.next_id;
        self.next_id += 1;
        let answer = self
            .post(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        answer
            .messages
            .into_iter()
            .find(|m| m.get("id").and_then(Value::as_u64) == Some(id))
            .ok_or_else(|| Refused::new("hub_error", format!("no answer to {method}")))
    }

    /// `initialize` once, as this binary.
    async fn ensure_initialized(&mut self) -> Result<(), Refused> {
        if self.initialized {
            return Ok(());
        }
        let reply = self
            .request(
                "initialize",
                serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "devagotchi", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        if reply.get("error").is_some() {
            return Err(Refused::new(
                "hub_error",
                format!("the hub refused initialize: {}", reply["error"]),
            ));
        }
        let _ = self
            .post(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await?;
        self.initialized = true;
        Ok(())
    }

    /// Call one tool. A tool refusal (`isError`) comes back as the hub's own
    /// `{code, message, …}`, a `BridgeError` with that code; a transport refusal likewise.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> BridgeResult<Value> {
        self.ensure_initialized().await?;
        let reply = self
            .request(
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        if let Some(error) = reply.get("error") {
            return Err(BridgeError::new(
                "hub_error",
                format!(
                    "{name}: {}",
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("refused")
                ),
            ));
        }
        let result = &reply["result"];
        let content = match result.get("structuredContent") {
            Some(structured) => structured.clone(),
            None => result["content"][0]["text"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null),
        };
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let code = content["code"].as_str().unwrap_or("refused").to_string();
            let message = content["message"]
                .as_str()
                .unwrap_or("the hub refused the call")
                .to_string();
            return Err(BridgeError::new(&code, message));
        }
        Ok(content)
    }
}

/// The session for synchronous callers (the command line): one runtime, one hub.
pub struct HubClient {
    runtime: tokio::runtime::Runtime,
    session: Session,
}

impl HubClient {
    pub fn new(hub: &str, account_file: &Path, seat: Option<&str>) -> BridgeResult<Self> {
        let session =
            Session::new(hub, account_file, seat).map_err(|why| BridgeError::new("usage", why))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| BridgeError::new("hub_error", format!("runtime: {error}")))?;
        Ok(Self { runtime, session })
    }

    pub fn hub(&self) -> &str {
        self.session.hub()
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> BridgeResult<Value> {
        self.runtime
            .block_on(self.session.call_tool(name, arguments))
    }

    /// Call a tool and read its answer as `T`.
    pub fn call<T: serde::de::DeserializeOwned>(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> BridgeResult<T> {
        let value = self.call_tool(name, arguments)?;
        serde_json::from_value(value).map_err(|error| {
            BridgeError::new(
                "hub_error",
                format!("{name}: the hub's answer is not the record expected: {error}"),
            )
        })
    }
}
