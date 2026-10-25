//! People's view of the hub in a browser: sign in with a Shippin account (Nomen) and see
//! who you are signed in as and the machines you can reach, each online or offline.
//!
//! Sign-in is the OAuth 2.0 authorization code flow with PKCE (RFC 7636) against Nomen, as
//! a public client: no secret ships with the hub for it. The access token Nomen returns is
//! checked by the same introspection as every bearer token (accepted clients, audience,
//! exact account ID), so a browser session is never weaker than an MCP call.
//!
//! - The browser holds only a random session id; the hub keeps that id's SHA-256 and,
//!   sealed on disk, Nomen's tokens (`sessions.rs`), so a restart signs nobody out.
//! - The hub asks for `offline_access`. With a refresh token from Nomen, the session lasts
//!   up to 90 days and ends after 30 days unused. The hub renews the access token through
//!   Nomen as it runs out, one refresh at a time, keeping the rotated refresh token.
//!   Nomen refusing a refresh ends the session. Without a refresh token, a session never
//!   outlives its access token or [`NO_REFRESH_MAX`].
//! - Every page re-checks the access token (Nomen's answer is reused for up to a minute),
//!   so an account disabled in Nomen loses its page within a minute.
//! - Signing out removes the session and revokes its refresh token at Nomen.
//! - `state` is bound to the browser with a short-lived cookie (login CSRF), is single-use,
//!   and the page to return to is kept on the hub, only ever a path on this hub.
//! - Cookies carry the `__Host-` prefix, `Secure`, `HttpOnly` and `SameSite=Lax`
//!   (plain names without `Secure` only when the hub runs on loopback http, for tests).
//! - Redirects are not followed when talking to Nomen.

mod approvals;
mod notify;
mod pages;
mod public;
mod routes;
mod sessions;
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

/// Every web page's top bar says whether the browser is signed in: the account and a
/// one-tap Sign out, or Sign in back to this page (`pages::Bar`).
pub async fn bar(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let bar = match &state.web {
        None => pages::Bar::Plain,
        Some(web) if web.signed_in(request.headers()) => pages::Bar::SignedIn,
        Some(_) => pages::Bar::SignedOut {
            return_to: safe_return_to(
                request
                    .uri()
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/account"),
            )
            .to_string(),
        },
    };
    pages::BAR.scope(bar, next.run(request)).await
}

/// The longest a browser session lasts when Nomen gives no refresh token, however long
/// its access token would.
pub const NO_REFRESH_MAX: Duration = Duration::from_secs(12 * 3600);
/// An access token this close to running out is renewed first.
const RENEW_BEFORE_MS: u64 = 60_000;
/// How long a started sign-in waits for Nomen to send the person back.
const LOGIN_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 10_000;
const MAX_ANSWER_BYTES: usize = 64 * 1024;

pub struct WebLogin {
    authorize: String,
    token: String,
    revoke: String,
    client_id: String,
    scope: String,
    redirect_uri: String,
    secure: bool,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: sessions::SessionStore,
    /// One refresh at a time: Nomen rotates refresh tokens, so two requests renewing the
    /// same session at once would spend the same token twice.
    renewing: tokio::sync::Mutex<()>,
}

pub struct Pending {
    verifier: String,
    pub return_to: String,
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
    #[serde(default)]
    pub refresh_token: Option<String>,
}

/// What a request's session cookie amounts to.
pub enum SessionToken {
    /// The access token to check, renewed if it was running out.
    Token(String),
    /// No session, or one that has ended.
    None,
    /// The session needs renewing and Nomen cannot be reached.
    Unavailable,
}

impl WebLogin {
    /// Nomen's endpoints follow from its issuer, as introspection's does. Nomen is not
    /// contacted until someone signs in. Sessions are kept in `state_dir`.
    pub fn new(
        issuer: &str,
        audiences: &[String],
        client_id: &str,
        public_url: &str,
        state_dir: &std::path::Path,
    ) -> BridgeResult<Self> {
        let issuer = issuer.trim_end_matches('/');
        // Nomen puts a project in a token's audience, and the account's org in its
        // answers, only when the scope asks for them.
        let scope = crate::auth::sign_in_scopes(audiences);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| BridgeError::new("auth_setup", error.to_string()))?;
        Ok(Self {
            authorize: format!("{issuer}/oauth/v2/authorize"),
            token: format!("{issuer}/oauth/v2/token"),
            revoke: format!("{issuer}/oauth/v2/revoke"),
            client_id: client_id.to_string(),
            scope,
            redirect_uri: format!("{}/auth/callback", public_url.trim_end_matches('/')),
            secure: public_url.starts_with("https://"),
            http,
            pending: Mutex::new(HashMap::new()),
            sessions: sessions::SessionStore::open(state_dir)?,
            renewing: tokio::sync::Mutex::new(()),
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

    /// Trade the code Nomen sent back for tokens, proving with the PKCE verifier that this
    /// hub started the sign-in.
    pub async fn exchange(&self, code: &str, pending: &Pending) -> Result<Tokens, Exchange> {
        self.post_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .await
    }

    /// Renew an access token with a refresh token (RFC 6749 §6).
    async fn refresh(&self, refresh_token: &str) -> Result<Tokens, Exchange> {
        self.post_token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ])
        .await
    }

    async fn post_token(&self, form: &[(&str, &str)]) -> Result<Tokens, Exchange> {
        let mut response = self
            .http
            .post(&self.token)
            .form(form)
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

    /// Keep Nomen's tokens on the hub, sealed, and hand the browser a random id for them.
    /// Returns the id and how long the browser should keep it.
    pub fn open_session(&self, tokens: Tokens) -> BridgeResult<(String, Duration)> {
        let now = sessions::now_millis();
        let access_for = tokens
            .expires_in
            .map(Duration::from_secs)
            .unwrap_or(NO_REFRESH_MAX)
            .min(NO_REFRESH_MAX);
        let lasts = if tokens.refresh_token.is_some() {
            Duration::from_millis(sessions::MAX)
        } else {
            access_for
        };
        let id = random_token();
        self.sessions.put(
            &digest(&id),
            &sessions::Held {
                access: tokens.access_token,
                refresh: tokens.refresh_token,
            },
            now + access_for.as_millis() as u64,
            now + lasts.as_millis() as u64,
            now,
        )?;
        Ok((id, lasts))
    }

    /// The access token behind this request's session cookie, renewed through Nomen when
    /// it is about to run out.
    pub async fn token(&self, headers: &HeaderMap) -> SessionToken {
        let Some(id) = cookie_value(headers, self.session_cookie()) else {
            return SessionToken::None;
        };
        let key = digest(&id);
        let now = sessions::now_millis();
        let stored = match self.sessions.get(&key, now) {
            Ok(Some(stored)) => stored,
            Ok(None) => return SessionToken::None,
            Err(error) => {
                eprintln!("devagotchi-hub: session store: {}", error.message);
                return SessionToken::Unavailable;
            }
        };
        if stored.access_until > now + RENEW_BEFORE_MS || stored.held.refresh.is_none() {
            if stored.access_until <= now {
                self.sessions.remove(&key);
                return SessionToken::None;
            }
            if now >= stored.last_seen + 60_000 {
                let _ = self.sessions.touch(&key, now);
            }
            return SessionToken::Token(stored.held.access);
        }
        let _one_at_a_time = self.renewing.lock().await;
        // Another request may have renewed it while this one waited.
        let stored = match self.sessions.get(&key, now) {
            Ok(Some(stored)) => stored,
            Ok(None) => return SessionToken::None,
            Err(_) => return SessionToken::Unavailable,
        };
        let Some(refresh) = stored.held.refresh.clone() else {
            return SessionToken::None;
        };
        if stored.access_until > now + RENEW_BEFORE_MS {
            return SessionToken::Token(stored.held.access);
        }
        match self.refresh(&refresh).await {
            Ok(fresh) => {
                let access_for = fresh
                    .expires_in
                    .map(Duration::from_secs)
                    .unwrap_or(NO_REFRESH_MAX)
                    .min(NO_REFRESH_MAX);
                let held = sessions::Held {
                    access: fresh.access_token,
                    refresh: fresh.refresh_token.or(Some(refresh)),
                };
                let now = sessions::now_millis();
                if let Err(error) =
                    self.sessions
                        .renew(&key, &held, now + access_for.as_millis() as u64, now)
                {
                    eprintln!("devagotchi-hub: session store: {}", error.message);
                }
                SessionToken::Token(held.access)
            }
            Err(Exchange::Refused) => {
                self.sessions.remove(&key);
                SessionToken::None
            }
            Err(Exchange::Unavailable(why)) => {
                eprintln!("devagotchi-hub: issuer unavailable: {why}");
                if stored.access_until > now {
                    SessionToken::Token(stored.held.access)
                } else {
                    SessionToken::Unavailable
                }
            }
        }
    }

    /// End the session and revoke its refresh token at Nomen (RFC 7009), best effort.
    pub fn end_session(&self, headers: &HeaderMap) {
        let Some(id) = cookie_value(headers, self.session_cookie()) else {
            return;
        };
        let Some(refresh) = self.sessions.remove(&digest(&id)).and_then(|h| h.refresh) else {
            return;
        };
        let request = self.http.post(&self.revoke).form(&[
            ("token", refresh.as_str()),
            ("token_type_hint", "refresh_token"),
            ("client_id", self.client_id.as_str()),
        ]);
        tokio::spawn(async move {
            if let Err(error) = request.send().await {
                eprintln!("devagotchi-hub: could not revoke a refresh token: {error}");
            }
        });
    }

    /// Whether this request's session cookie names a live session. Local only: no call to
    /// Nomen, so every page can say "signed in" or offer Sign in at no cost.
    pub fn signed_in(&self, headers: &HeaderMap) -> bool {
        cookie_value(headers, self.session_cookie()).is_some_and(|id| {
            matches!(
                self.sessions.get(&digest(&id), sessions::now_millis()),
                Ok(Some(stored)) if stored.held.refresh.is_some()
                    || stored.access_until > sessions::now_millis()
            )
        })
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
