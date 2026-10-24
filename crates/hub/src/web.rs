//! People's view of the hub in a browser: sign in with a Shippin account (Nomen) and see
//! who you are signed in as and the machines you can reach, each online or offline.
//!
//! Sign-in is the OAuth 2.0 authorization code flow with PKCE (RFC 7636) against Nomen, as
//! a public client: no secret ships with the hub for it. The access token Nomen returns is
//! checked by the same introspection as every bearer token (accepted clients, audience,
//! exact account ID), so a browser session is never weaker than an MCP call.
//!
//! - The token stays in the hub's memory; the browser holds only a random session id, and
//!   the hub keeps only that id's SHA-256.
//! - Every page re-checks the token (Nomen's answer is reused for up to a minute), so an
//!   account disabled in Nomen loses its page within a minute, and a session never
//!   outlives its token or [`SESSION_MAX`].
//! - `state` is bound to the browser with a short-lived cookie (login CSRF), is single-use,
//!   and the page to return to is kept on the hub, only ever a path on this hub.
//! - Cookies carry the `__Host-` prefix, `Secure`, `HttpOnly` and `SameSite=Lax`
//!   (plain names without `Secure` only when the hub runs on loopback http, for tests).
//! - Redirects are not followed when talking to Nomen.

mod approvals;
mod pages;
mod public;
mod routes;
#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::http::{HeaderMap, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devagotchi_contracts::{BridgeError, BridgeResult};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub use routes::router;

/// The longest a browser session lasts, however long Nomen's token would.
pub const SESSION_MAX: Duration = Duration::from_secs(12 * 3600);
/// How long a started sign-in waits for Nomen to send the person back.
const LOGIN_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 10_000;
const MAX_SESSIONS: usize = 10_000;
const MAX_ANSWER_BYTES: usize = 64 * 1024;

pub struct WebLogin {
    authorize: String,
    token: String,
    client_id: String,
    scope: String,
    redirect_uri: String,
    secure: bool,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<[u8; 32], Session>>,
}

pub struct Pending {
    verifier: String,
    pub return_to: String,
    until: Instant,
}

struct Session {
    token: String,
    until: Instant,
}

/// Why trading the code for a token failed.
#[derive(Debug)]
pub enum Exchange {
    /// Nomen refused the code: expired, used, or not this hub's. Starting again fixes it.
    Refused,
    /// Nomen could not be reached or answered something the hub cannot read.
    Unavailable(String),
}

#[derive(Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

impl WebLogin {
    /// Nomen's endpoints follow from its issuer, as introspection's does. Nomen is not
    /// contacted until someone signs in.
    pub fn new(
        issuer: &str,
        audiences: &[String],
        client_id: &str,
        public_url: &str,
    ) -> BridgeResult<Self> {
        let issuer = issuer.trim_end_matches('/');
        // Nomen puts a project in a token's audience only when this scope asks for it.
        let mut scope = String::from("openid profile");
        for audience in audiences {
            scope.push_str(&format!(" urn:zitadel:iam:org:project:id:{audience}:aud"));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| BridgeError::new("auth_setup", error.to_string()))?;
        Ok(Self {
            authorize: format!("{issuer}/oauth/v2/authorize"),
            token: format!("{issuer}/oauth/v2/token"),
            client_id: client_id.to_string(),
            scope,
            redirect_uri: format!("{}/auth/callback", public_url.trim_end_matches('/')),
            secure: public_url.starts_with("https://"),
            http,
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn session_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-dg_session"
        } else {
            "dg_session"
        }
    }

    pub fn login_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-dg_login"
        } else {
            "dg_login"
        }
    }

    /// A `Set-Cookie` value; `max_age` 0 deletes the cookie.
    pub fn cookie(&self, name: &str, value: &str, max_age: u64) -> String {
        let secure = if self.secure { "; Secure" } else { "" };
        format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
    }

    /// Start a sign-in: remember the PKCE verifier and where to go after, under a fresh
    /// `state`, and return that state and the Nomen URL to send the browser to.
    pub fn begin(&self, return_to: &str) -> (String, String) {
        let state = random_token();
        let verifier = random_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let now = Instant::now();
        {
            let mut pending = self.pending.lock().expect("pending sign-ins");
            pending.retain(|_, p| p.until > now);
            if pending.len() >= MAX_PENDING
                && let Some(oldest) = pending
                    .iter()
                    .min_by_key(|(_, p)| p.until)
                    .map(|(k, _)| k.clone())
            {
                pending.remove(&oldest);
            }
            pending.insert(
                state.clone(),
                Pending {
                    verifier,
                    return_to: safe_return_to(return_to).to_string(),
                    until: now + LOGIN_TTL,
                },
            );
        }
        let url = format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
            self.authorize,
            encode(&self.client_id),
            encode(&self.redirect_uri),
            encode(&self.scope),
            state,
            challenge,
        );
        (state, url)
    }

    /// The sign-in this `state` started, once: a second callback with it finds nothing.
    pub fn take(&self, state: &str) -> Option<Pending> {
        let pending = self
            .pending
            .lock()
            .expect("pending sign-ins")
            .remove(state)?;
        (pending.until > Instant::now()).then_some(pending)
    }

    /// Trade the code Nomen sent back for an access token, proving with the PKCE verifier
    /// that this hub started the sign-in.
    pub async fn exchange(&self, code: &str, pending: &Pending) -> Result<Tokens, Exchange> {
        let mut response = self
            .http
            .post(&self.token)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("client_id", self.client_id.as_str()),
                ("code_verifier", pending.verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|error| Exchange::Unavailable(format!("Nomen token endpoint: {error}")))?;
        let status = response.status();
        if matches!(status.as_u16(), 400 | 401 | 403) {
            return Err(Exchange::Refused);
        }
        if !status.is_success() {
            return Err(Exchange::Unavailable(format!(
                "Nomen token endpoint answered {status}"
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| Exchange::Unavailable(format!("Nomen token endpoint: {error}")))?
        {
            if body.len() + chunk.len() > MAX_ANSWER_BYTES {
                return Err(Exchange::Unavailable(
                    "Nomen's token answer is too large".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| {
            Exchange::Unavailable("Nomen's token answer is not JSON the hub reads".into())
        })
    }

    /// Keep the token on the hub and hand the browser a random id for it.
    pub fn open_session(&self, token: String, expires_in: Option<u64>) -> (String, Duration) {
        let lasts = expires_in
            .map(Duration::from_secs)
            .unwrap_or(SESSION_MAX)
            .min(SESSION_MAX);
        let id = random_token();
        let now = Instant::now();
        let mut sessions = self.sessions.lock().expect("sessions");
        sessions.retain(|_, s| s.until > now);
        if sessions.len() >= MAX_SESSIONS
            && let Some(soonest) = sessions
                .iter()
                .min_by_key(|(_, s)| s.until)
                .map(|(k, _)| *k)
        {
            sessions.remove(&soonest);
        }
        sessions.insert(
            digest(&id),
            Session {
                token,
                until: now + lasts,
            },
        );
        (id, lasts)
    }

    /// The access token behind this request's session cookie, while the session lasts.
    pub fn token(&self, headers: &HeaderMap) -> Option<String> {
        let id = cookie_value(headers, self.session_cookie())?;
        let key = digest(&id);
        let mut sessions = self.sessions.lock().expect("sessions");
        match sessions.get(&key) {
            Some(session) if session.until > Instant::now() => Some(session.token.clone()),
            Some(_) => {
                sessions.remove(&key);
                None
            }
            None => None,
        }
    }

    pub fn end_session(&self, headers: &HeaderMap) {
        if let Some(id) = cookie_value(headers, self.session_cookie()) {
            self.sessions.lock().expect("sessions").remove(&digest(&id));
        }
    }

    pub fn login_state(&self, headers: &HeaderMap) -> Option<String> {
        cookie_value(headers, self.login_cookie())
    }
}

/// 32 random bytes, base64url: a `state`, a PKCE verifier (43 characters) or a session id.
fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(crate::link::random::<32>())
}

fn digest(id: &str) -> [u8; 32] {
    Sha256::digest(id.as_bytes()).into()
}

/// A path on this hub to return to after sign-in; anything else becomes `/account`.
pub fn safe_return_to(path: &str) -> &str {
    let ok = path.starts_with('/')
        && !path.starts_with("//")
        && path.len() <= 512
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.~?=&%".contains(&b));
    if ok { path } else { "/account" }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
        .filter(|value| !value.is_empty() && value.len() <= 128)
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass through).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
