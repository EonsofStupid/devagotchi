//! Devagotchi as an OAuth 2.1 authorization server for its own MCP resource.
//!
//! Clients (claude.ai, ChatGPT, Cursor, the CLI) register here, send the person
//! through this hub's sign-in, and receive a JWT this hub signed. The resource
//! server checks that JWT locally. An upstream provider is not involved in this
//! module: it only proves who the account is before `consent` is called.

use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devagotchi_contracts::{BridgeError, BridgeResult};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, jwk::Jwk};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use aws_lc_rs::signature::KeyPair;
use axum::response::IntoResponse;

use crate::{
    auth::{AuthError, Subject},
    identity::is_account_id,
};

const ACCESS_TTL: u64 = 3600;
const CODE_TTL: u64 = 600;
const REFRESH_TTL: u64 = 30 * 24 * 3600;
const SESSION_TTL: u64 = 12 * 3600;
/// Rotate the signing key after this long. The previous public key keeps verifying
/// until access tokens signed with it have expired.
const KEY_LIFE: u64 = 90 * 24 * 3600;
const DAY: u64 = 24 * 3600;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA busy_timeout = 5000;
CREATE TABLE IF NOT EXISTS clients (
  client_id TEXT PRIMARY KEY,
  client_name TEXT,
  redirect_uris TEXT NOT NULL,
  auth_method TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS auth_codes (
  code_hash TEXT PRIMARY KEY,
  client_id TEXT NOT NULL,
  account_id TEXT NOT NULL,
  redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL,
  scope TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS refresh_tokens (
  token_hash TEXT PRIMARY KEY,
  family TEXT NOT NULL,
  client_id TEXT NOT NULL,
  account_id TEXT NOT NULL,
  scope TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS revoked_jti (
  jti TEXT PRIMARY KEY,
  expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pending_auth (
  id TEXT PRIMARY KEY,
  client_id TEXT NOT NULL,
  redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL,
  scope TEXT NOT NULL,
  state TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS browser_sessions (
  id_hash BLOB PRIMARY KEY,
  account_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
";

pub struct Authority {
    public_url: String,
    keys: Mutex<KeyRing>,
    key_path: std::path::PathBuf,
    connection: Mutex<Connection>,
    upstream: Vec<crate::config::UpstreamProvider>,
    local: crate::config::LocalLogin,
    outbox: std::path::PathBuf,
    identity: crate::identity::Store,
    discovery: Mutex<std::collections::HashMap<String, Discovery>>,
    http: reqwest::Client,
    starts: Mutex<std::collections::HashMap<String, Vec<u64>>>,
    limits: Mutex<LiveLimits>,
    merges: Mutex<Vec<(String, String)>>,
}

struct LiveLimits {
    ttl_secs: u64,
    starts_per_hour: usize,
    unclaimed_cap: usize,
    /// A row in `operator_limits`. `hub.json` does not override it.
    persisted: bool,
}

/// What `GET /api/v1/admin/settings` returns. Days, not seconds: the control plane
/// sets `ttl_days` and the hub stores `ttl_days * 86400`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OperatorSettings {
    pub provisional: ProvisionalNumbers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProvisionalNumbers {
    pub ttl_days: u32,
    pub starts_per_hour: u32,
    pub unclaimed_cap: u32,
}

/// A partial update. Omitted fields keep the live value.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProvisionalPatch {
    #[serde(default)]
    pub ttl_days: Option<u32>,
    #[serde(default)]
    pub starts_per_hour: Option<u32>,
    #[serde(default)]
    pub unclaimed_cap: Option<u32>,
}

#[derive(Clone)]
struct Discovery {
    authorization: String,
    token: String,
    userinfo: Option<String>,
    issuer: String,
}

struct KeyRing {
    current: SigningKey,
    previous: Vec<PublicKey>,
}

struct SigningKey {
    kid: String,
    pkcs8: Vec<u8>,
    x: String,
    y: String,
    created_at: u64,
}

struct PublicKey {
    kid: String,
    x: String,
    y: String,
    retire_at: u64,
}

#[derive(Debug, Clone)]
pub struct AuthorizeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub state: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizeResult {
    /// Show consent. `request` is the form field to post back.
    Consent {
        request: String,
        client_name: String,
        scope: String,
    },
    Redirect(String),
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub device_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    pub scope: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn store_error(error: rusqlite::Error) -> BridgeError {
    BridgeError::new("oauth", error.to_string())
}

fn oauth_error(error: &str, description: &str) -> BridgeError {
    BridgeError::new(error, description)
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(crate::link::random::<32>())
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// https, or http to loopback. No fragment, no whitespace.
pub fn acceptable_redirect(uri: &str) -> bool {
    if uri.len() > 512 || uri.contains([' ', '\t', '\n', '\r', '#']) {
        return false;
    }
    if uri.starts_with("https://") && uri.len() > "https://".len() {
        return true;
    }
    let rest = uri
        .strip_prefix("http://127.0.0.1")
        .or_else(|| uri.strip_prefix("http://localhost"))
        .or_else(|| uri.strip_prefix("http://[::1]"));
    match rest {
        Some(rest) if rest.is_empty() || rest.starts_with(':') || rest.starts_with('/') => true,
        _ => false,
    }
}

fn pkce_challenge_ok(challenge: &str, method: &str) -> bool {
    method == "S256"
        && (43..=128).contains(&challenge.len())
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn verifier_ok(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
}

impl Authority {
    pub fn open(state_dir: &Path, public_url: &str) -> BridgeResult<Self> {
        let public_url = public_url.trim_end_matches('/').to_string();
        if !public_url.starts_with("https://") && !public_url.starts_with("http://") {
            return Err(BridgeError::new(
                "invalid_config",
                "public_url must be an absolute http(s) URL",
            ));
        }
        std::fs::create_dir_all(state_dir).map_err(|error| {
            BridgeError::new("oauth", format!("{}: {error}", state_dir.display()))
        })?;
        let db_path = state_dir.join("oauth.sqlite3");
        if !db_path.exists() {
            crate::push::write_private(&db_path, &[])?;
        }
        let connection = Connection::open(&db_path).map_err(store_error)?;
        connection.execute_batch(SCHEMA).map_err(store_error)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS magic_links (
                   token_hash TEXT PRIMARY KEY,
                   email TEXT NOT NULL,
                   resume TEXT NOT NULL,
                   expires_at INTEGER NOT NULL,
                   used INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS device_grants (
                   device_hash TEXT PRIMARY KEY,
                   user_code TEXT NOT NULL UNIQUE,
                   client_id TEXT NOT NULL,
                   scope TEXT NOT NULL,
                   expires_at INTEGER NOT NULL,
                   account_id TEXT
                 );
                 CREATE TABLE IF NOT EXISTS upstream_tx (
                   state TEXT PRIMARY KEY,
                   provider TEXT NOT NULL,
                   verifier TEXT NOT NULL,
                   resume TEXT NOT NULL,
                   expires_at INTEGER NOT NULL
                 );",
            )
            .map_err(store_error)?;
        let key_path = state_dir.join("oauth-signing.json");
        let mut keys = load_or_create_keys(&key_path)?;
        if now().saturating_sub(keys.current.created_at) > KEY_LIFE {
            keys = rotate_ring(keys);
            save_keys(&key_path, &keys)?;
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| BridgeError::new("oauth", error.to_string()))?;
        let identity = crate::identity::Store::open(&state_dir.join("identities.sqlite3"))?;
        let limits = match identity.load_limits()? {
            Some(row) => LiveLimits {
                ttl_secs: row.ttl_secs,
                starts_per_hour: row.starts_per_hour as usize,
                unclaimed_cap: row.unclaimed_cap as usize,
                persisted: true,
            },
            None => LiveLimits {
                ttl_secs: u64::from(crate::config::PROVISIONAL_TTL_DAYS_DEFAULT) * DAY,
                starts_per_hour: crate::config::PROVISIONAL_STARTS_PER_HOUR_DEFAULT as usize,
                unclaimed_cap: crate::config::PROVISIONAL_UNCLAIMED_CAP_DEFAULT as usize,
                persisted: false,
            },
        };
        let authority = Self {
            public_url,
            keys: Mutex::new(keys),
            key_path,
            connection: Mutex::new(connection),
            upstream: Vec::new(),
            local: crate::config::LocalLogin::default(),
            outbox: state_dir.join("login-outbox"),
            identity,
            discovery: Mutex::new(std::collections::HashMap::new()),
            http,
            starts: Mutex::new(std::collections::HashMap::new()),
            limits: Mutex::new(limits),
            merges: Mutex::new(Vec::new()),
        };
        authority.seed_cli_client()?;
        Ok(authority)
    }

    pub fn issuer(&self) -> &str {
        &self.public_url
    }

    pub fn configure_login(
        &mut self,
        upstream: Vec<crate::config::UpstreamProvider>,
        local: crate::config::LocalLogin,
    ) {
        self.upstream = upstream;
        self.local = local;
    }

    /// Apply `hub.json` only when the operator has not saved a row yet.
    pub fn apply_config_limits(&self, policy: &crate::config::ProvisionalPolicy) {
        let mut limits = self.limits.lock().expect("limits");
        if limits.persisted {
            return;
        }
        limits.ttl_secs = u64::from(policy.ttl_days) * DAY;
        limits.starts_per_hour = policy.starts_per_hour as usize;
        limits.unclaimed_cap = policy.unclaimed_cap as usize;
    }

    fn limit_snapshot(&self) -> (u64, usize, usize) {
        let limits = self.limits.lock().expect("limits");
        (
            limits.ttl_secs,
            limits.starts_per_hour,
            limits.unclaimed_cap,
        )
    }

    pub(crate) fn operator_settings(&self) -> OperatorSettings {
        let (ttl, starts, cap) = self.limit_snapshot();
        OperatorSettings {
            provisional: ProvisionalNumbers {
                ttl_days: (ttl / DAY) as u32,
                starts_per_hour: starts as u32,
                unclaimed_cap: cap as u32,
            },
        }
    }

    /// Persist and apply immediately. Already-issued `provisional_until` values stay
    /// where they are; the next account and the expiry warning use the new ttl.
    pub(crate) fn update_operator_settings(
        &self,
        patch: &ProvisionalPatch,
    ) -> BridgeResult<OperatorSettings> {
        if patch.ttl_days.is_none()
            && patch.starts_per_hour.is_none()
            && patch.unclaimed_cap.is_none()
        {
            return Err(oauth_error(
                "invalid_settings",
                "provisional needs ttl_days, starts_per_hour, or unclaimed_cap",
            ));
        }
        let current = self.operator_settings();
        let ttl_days = patch.ttl_days.unwrap_or(current.provisional.ttl_days);
        let starts = patch
            .starts_per_hour
            .unwrap_or(current.provisional.starts_per_hour);
        let cap = patch
            .unclaimed_cap
            .unwrap_or(current.provisional.unclaimed_cap);
        if let Err(why) = crate::config::check_provisional_bounds(ttl_days, starts, cap) {
            return Err(oauth_error("invalid_settings", &why));
        }
        let stored = crate::identity::OperatorLimits {
            ttl_secs: u64::from(ttl_days) * DAY,
            starts_per_hour: starts,
            unclaimed_cap: cap,
        };
        self.identity.save_limits(&stored)?;
        {
            let mut limits = self.limits.lock().expect("limits");
            limits.ttl_secs = stored.ttl_secs;
            limits.starts_per_hour = starts as usize;
            limits.unclaimed_cap = cap as usize;
            limits.persisted = true;
        }
        Ok(self.operator_settings())
    }

    pub fn providers(&self) -> &[crate::config::UpstreamProvider] {
        &self.upstream
    }

    pub fn identity(&self) -> &crate::identity::Store {
        &self.identity
    }

    /// Whether a magic link may be written to the journal and the outbox.
    pub fn magic_delivery(&self) -> Result<(), BridgeError> {
        if !self.local.magic_link {
            return Err(oauth_error(
                "invalid_request",
                "local sign-in is turned off",
            ));
        }
        let deliver = match self.local.deliver.as_deref() {
            Some("log") => true,
            Some("off") => false,
            _ => {
                self.public_url.starts_with("http://127.0.0.1")
                    || self.public_url.starts_with("http://localhost")
                    || self.public_url.starts_with("http://[::1]")
            }
        };
        if !deliver {
            return Err(oauth_error(
                "invalid_request",
                "magic links are off on a public URL until auth.local.deliver is \"log\"",
            ));
        }
        Ok(())
    }

    /// Issue a magic link. The HTTP caller does not receive it. The URL is written
    /// to `{state}/login-outbox` and the hub journal.
    pub fn request_magic_link(&self, email: &str, resume: &str) -> BridgeResult<()> {
        self.magic_delivery()?;
        if !crate::identity::acceptable_email(email) {
            return Err(oauth_error("invalid_request", "email is not usable"));
        }
        let email = email.trim().to_lowercase();
        let token = random_token();
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .execute(
                "INSERT INTO magic_links (token_hash, email, resume, expires_at, used)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![
                    digest(&token).as_slice(),
                    email,
                    resume,
                    (now() + CODE_TTL) as i64
                ],
            )
            .map_err(store_error)?;
        drop(connection);
        let url = format!("{}/login/redeem?token={token}", self.public_url);
        std::fs::create_dir_all(&self.outbox)
            .map_err(|error| BridgeError::new("oauth", format!("login outbox: {error}")))?;
        let path = self
            .outbox
            .join(format!("{}-{}", now(), &hex::encode(digest(&token))[..8]));
        crate::push::write_private(&path, url.as_bytes())?;
        eprintln!("devagotchi hub: magic link for {email}: {url}");
        Ok(())
    }

    pub fn redeem_magic_link(&self, token: &str) -> BridgeResult<(String, String)> {
        let mut connection = self.connection.lock().expect("oauth lock");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, String, i64, i64)> = tx
            .query_row(
                "SELECT email, resume, expires_at, used FROM magic_links WHERE token_hash = ?1",
                params![digest(token).as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(store_error)?;
        let Some((email, resume, exp, used)) = row else {
            return Err(oauth_error("invalid_grant", "unknown sign-in link"));
        };
        if used != 0 || (exp as u64) <= now() {
            return Err(oauth_error(
                "invalid_grant",
                "sign-in link is no longer valid",
            ));
        }
        tx.execute(
            "UPDATE magic_links SET used = 1 WHERE token_hash = ?1",
            params![digest(token).as_slice()],
        )
        .map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        if let Some(code) = resume.strip_prefix("claim:") {
            let claimed = self.claim_with(
                code,
                &crate::identity::Claim {
                    issuer: "urn:devagotchi:magic".into(),
                    sub: email.clone(),
                    email: Some(email),
                    email_verified: true,
                    name: None,
                },
            )?;
            return Ok((claimed.account_id, String::new()));
        }
        let resolved = self.identity.redeem_local(&email, None)?;
        Ok((resolved.account_id, resume))
    }

    pub async fn start_upstream(&self, name: &str, resume: &str) -> BridgeResult<String> {
        let provider = self
            .upstream
            .iter()
            .find(|provider| provider.name == name)
            .ok_or_else(|| oauth_error("invalid_request", "unknown sign-in provider"))?;
        let discovery = self.discover(provider).await?;
        let verifier = random_token();
        let challenge = s256(&verifier);
        let state = random_token();
        let redirect = format!("{}/login/upstream/{name}/callback", self.public_url);
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .execute(
                "INSERT INTO upstream_tx (state, provider, verifier, resume, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![state, name, verifier, resume, (now() + CODE_TTL) as i64],
            )
            .map_err(store_error)?;
        drop(connection);
        let scope = provider
            .scopes
            .clone()
            .unwrap_or_else(|| "openid email profile".to_string());
        Ok(format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
            discovery.authorization,
            urlencoding(&provider.client_id),
            urlencoding(&redirect),
            urlencoding(&scope),
            urlencoding(&state),
            urlencoding(&challenge),
        ))
    }

    pub async fn finish_upstream(
        &self,
        state: &str,
        code: &str,
        signed_in: Option<&str>,
    ) -> BridgeResult<(String, String)> {
        let (provider_name, verifier, resume) = {
            let mut connection = self.connection.lock().expect("oauth lock");
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(store_error)?;
            let row: Option<(String, String, String, i64)> = tx
                .query_row(
                    "SELECT provider, verifier, resume, expires_at FROM upstream_tx WHERE state = ?1",
                    params![state],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(store_error)?;
            let Some((provider_name, verifier, resume, exp)) = row else {
                return Err(oauth_error("invalid_request", "unknown sign-in"));
            };
            tx.execute("DELETE FROM upstream_tx WHERE state = ?1", params![state])
                .map_err(store_error)?;
            tx.commit().map_err(store_error)?;
            if (exp as u64) <= now() {
                return Err(oauth_error("invalid_request", "sign-in has expired"));
            }
            (provider_name, verifier, resume)
        };
        let provider = self
            .upstream
            .iter()
            .find(|provider| provider.name == provider_name)
            .ok_or_else(|| oauth_error("invalid_request", "unknown sign-in provider"))?
            .clone();
        let discovery = self.discover(&provider).await?;
        let redirect = format!(
            "{}/login/upstream/{provider_name}/callback",
            self.public_url
        );
        let response = self
            .http
            .post(&discovery.token)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect.as_str()),
                ("client_id", provider.client_id.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|error| oauth_error("invalid_request", &format!("provider token: {error}")))?;
        if !response.status().is_success() {
            return Err(oauth_error(
                "invalid_request",
                "the sign-in provider refused the code",
            ));
        }
        let token_body: Value = response
            .json()
            .await
            .map_err(|_| oauth_error("invalid_request", "provider token was not JSON"))?;
        let access = token_body["access_token"]
            .as_str()
            .ok_or_else(|| oauth_error("invalid_request", "provider sent no access token"))?;
        let userinfo = discovery
            .userinfo
            .ok_or_else(|| oauth_error("invalid_request", "provider has no userinfo endpoint"))?;
        let info: Value = self
            .http
            .get(&userinfo)
            .bearer_auth(access)
            .send()
            .await
            .map_err(|error| oauth_error("invalid_request", &format!("userinfo: {error}")))?
            .json()
            .await
            .map_err(|_| oauth_error("invalid_request", "userinfo was not JSON"))?;
        let sub = info["sub"]
            .as_str()
            .filter(|sub| !sub.is_empty())
            .ok_or_else(|| oauth_error("invalid_request", "userinfo has no sub"))?;
        let email = info["email"].as_str().map(str::to_string);
        let verified = info["email_verified"].as_bool().unwrap_or(false);
        let claim = crate::identity::Claim {
            issuer: discovery.issuer.clone(),
            sub: sub.to_string(),
            email,
            email_verified: verified,
            name: info["name"].as_str().map(str::to_string),
        };
        if let Some(code) = resume.strip_prefix("claim:") {
            let claimed = self.claim_with(code, &claim)?;
            return Ok((claimed.account_id, String::new()));
        }
        let resolved = if let Some(account) = signed_in {
            self.identity.link_to(account, &claim)?
        } else {
            self.identity.resolve(&claim)?
        };
        Ok((resolved.account_id, resume))
    }

    async fn discover(
        &self,
        provider: &crate::config::UpstreamProvider,
    ) -> BridgeResult<Discovery> {
        let issuer = provider.issuer.trim_end_matches('/').to_string();
        if let Some(cached) = self
            .discovery
            .lock()
            .expect("discovery")
            .get(&issuer)
            .cloned()
        {
            return Ok(cached);
        }
        let url = format!("{issuer}/.well-known/openid-configuration");
        let doc: Value = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|error| oauth_error("invalid_request", &format!("discovery: {error}")))?
            .json()
            .await
            .map_err(|_| oauth_error("invalid_request", "discovery was not JSON"))?;
        let got = doc["issuer"].as_str().unwrap_or("").trim_end_matches('/');
        if got != issuer {
            return Err(oauth_error(
                "invalid_request",
                "discovery issuer does not match the configured issuer",
            ));
        }
        let discovery = Discovery {
            authorization: doc["authorization_endpoint"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            token: doc["token_endpoint"].as_str().unwrap_or("").to_string(),
            userinfo: doc["userinfo_endpoint"].as_str().map(str::to_string),
            issuer,
        };
        if discovery.authorization.is_empty() || discovery.token.is_empty() {
            return Err(oauth_error(
                "invalid_request",
                "discovery is missing authorization or token endpoint",
            ));
        }
        self.discovery.lock().expect("discovery").insert(
            provider.issuer.trim_end_matches('/').to_string(),
            discovery.clone(),
        );
        Ok(discovery)
    }

    /// Readiness for `/healthz`. A down upstream is degraded, not a dead process.
    /// No upstreams is a healthy local-only hub.
    pub async fn readiness(&self) -> Value {
        let mut upstreams = Vec::new();
        let mut degraded = Vec::new();
        for provider in &self.upstream {
            match self.discover(provider).await {
                Ok(_) => upstreams.push(json!({
                    "name": provider.name,
                    "issuer": provider.issuer,
                    "discovery": true,
                })),
                Err(error) => {
                    let message = error.message.clone();
                    upstreams.push(json!({
                        "name": provider.name,
                        "issuer": provider.issuer,
                        "discovery": false,
                        "error": message,
                    }));
                    degraded.push(format!("{}: {message}", provider.name));
                }
            }
        }
        let signing_key = self
            .jwks()
            .get("keys")
            .and_then(|keys| keys.as_array())
            .is_some_and(|keys| !keys.is_empty());
        if !signing_key {
            degraded.insert(0, "signing key is missing".into());
        }
        let ready = signing_key && upstreams.iter().all(|row| row["discovery"] == true);
        json!({
            "mode": "devagotchi",
            "ready": ready,
            "identity_store": true,
            "signing_key": signing_key,
            "upstreams": upstreams,
            "degraded": degraded,
        })
    }

    pub fn login_page(&self, resume: &str) -> String {
        let mut providers = String::new();
        for provider in &self.upstream {
            providers.push_str(&format!(
                "<p><a href=\"/login/upstream/{}?resume={}\">Sign in with {}</a></p>",
                escape(&provider.name),
                escape(resume),
                escape(&provider.name)
            ));
        }
        let magic = if self.magic_delivery().is_ok() {
            format!(
                "<form method=\"post\" action=\"/login/magic\"><input type=\"hidden\" name=\"resume\" value=\"{}\">\
                 <label>Email <input name=\"email\" type=\"email\" required></label>\
                 <button type=\"submit\">Email me a sign-in link</button></form>",
                escape(resume)
            )
        } else {
            "<p>Email sign-in is off on this hub until mail is configured.</p>".to_string()
        };
        let claim = resume
            .strip_prefix("claim:")
            .map(|code| {
                format!(
                    "<form method=\"post\" action=\"/claim\"><input type=\"hidden\" name=\"code\" value=\"{}\">\
                     <label>Setup code from the hub host <input name=\"setup\" autocomplete=\"one-time-code\" required></label>\
                     <button type=\"submit\">Claim with the setup code</button></form>\
                     <p>A passkey is enrolled from the one-time link <code>devagotchi hub owner</code> prints on the hub host. A recovery code signs in below.</p>\
                     <form method=\"post\" action=\"/login/recovery\"><label>Recovery code <input name=\"code\" required></label>\
                     <button type=\"submit\">Sign in with a recovery code</button></form>",
                    escape(code)
                )
            })
            .unwrap_or_default();
        format!(
            "<!doctype html><title>Sign in</title><h1>Sign in to Devagotchi</h1>{providers}{magic}{claim}"
        )
    }

    pub fn resource(&self) -> String {
        format!("{}/mcp", self.public_url)
    }

    pub fn metadata(&self) -> Value {
        let base = &self.public_url;
        json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "registration_endpoint": format!("{base}/register"),
            "revocation_endpoint": format!("{base}/revoke"),
            "jwks_uri": format!("{base}/.well-known/jwks.json"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token", "urn:ietf:params:oauth:grant-type:device_code"],
            "device_authorization_endpoint": format!("{base}/device"),
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": ["openid", "email", "profile", "offline_access", "mcp"],
        })
    }

    pub fn protected_resource(&self, mcp: bool) -> Value {
        json!({
            "resource": if mcp { self.resource() } else { self.public_url.clone() },
            "authorization_servers": [self.public_url],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ["openid", "email", "profile", "offline_access", "mcp"],
            "resource_name": "Devagotchi",
        })
    }

    pub fn jwks(&self) -> Value {
        let keys = self.keys.lock().expect("oauth key lock");
        let mut list = vec![public_jwk(
            &keys.current.kid,
            &keys.current.x,
            &keys.current.y,
        )];
        let now = now();
        for key in &keys.previous {
            if key.retire_at > now {
                list.push(public_jwk(&key.kid, &key.x, &key.y));
            }
        }
        json!({ "keys": list })
    }

    /// Dynamic client registration. Public clients only. Redirects must be https
    /// or loopback http, matched exactly later.
    pub fn register(&self, body: &Value) -> BridgeResult<Value> {
        let redirects = body["redirect_uris"]
            .as_array()
            .ok_or_else(|| oauth_error("invalid_client_metadata", "redirect_uris is required"))?;
        if redirects.is_empty() || redirects.len() > 8 {
            return Err(oauth_error(
                "invalid_client_metadata",
                "redirect_uris needs 1..=8 entries",
            ));
        }
        let mut uris = Vec::new();
        for uri in redirects {
            let uri = uri.as_str().ok_or_else(|| {
                oauth_error("invalid_client_metadata", "redirect_uris must be strings")
            })?;
            if !acceptable_redirect(uri) {
                return Err(oauth_error(
                    "invalid_redirect_uri",
                    "redirect_uris must be https, or http to loopback",
                ));
            }
            uris.push(uri.to_string());
        }
        let method = body["token_endpoint_auth_method"]
            .as_str()
            .unwrap_or("none");
        if method != "none" {
            return Err(oauth_error(
                "invalid_client_metadata",
                "only public clients (token_endpoint_auth_method none) can register",
            ));
        }
        if let Some(grants) = body["grant_types"].as_array() {
            for grant in grants {
                let grant = grant.as_str().unwrap_or("");
                if grant != "authorization_code" && grant != "refresh_token" {
                    return Err(oauth_error(
                        "invalid_client_metadata",
                        "unsupported grant_type",
                    ));
                }
            }
        }
        if let Some(types) = body["response_types"].as_array() {
            for kind in types {
                if kind.as_str() != Some("code") {
                    return Err(oauth_error(
                        "invalid_client_metadata",
                        "only response_type code is supported",
                    ));
                }
            }
        }
        let name = body["client_name"]
            .as_str()
            .map(str::trim)
            .filter(|name| {
                !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
            })
            .unwrap_or("MCP client");
        let client_id = format!("dg_client_{}", hex::encode(crate::link::random::<16>()));
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .execute(
                "INSERT INTO clients (client_id, client_name, redirect_uris, auth_method, created_at)
                 VALUES (?1, ?2, ?3, 'none', ?4)",
                params![client_id, name, serde_json::to_string(&uris).unwrap(), now() as i64],
            )
            .map_err(store_error)?;
        Ok(json!({
            "client_id": client_id,
            "client_name": name,
            "redirect_uris": uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        }))
    }

    /// Check an authorization request. Does not redirect to an unregistered URI.
    pub fn start_authorization(&self, request: &AuthorizeRequest) -> BridgeResult<String> {
        if !pkce_challenge_ok(&request.code_challenge, &request.code_challenge_method) {
            return Err(self.auth_error(request, "invalid_request", "PKCE S256 is required")?);
        }
        if request.state.is_empty()
            || request.state.len() > 512
            || request.state.chars().any(char::is_control)
        {
            return Err(self.auth_error(request, "invalid_request", "state is required")?);
        }
        let client = self.client(&request.client_id)?;
        if !client
            .redirects
            .iter()
            .any(|uri| uri == &request.redirect_uri)
        {
            return Err(oauth_error(
                "invalid_request",
                "redirect_uri is not registered for this client",
            ));
        }
        let scope = if request.scope.trim().is_empty() {
            "openid email profile mcp".to_string()
        } else {
            request.scope.trim().to_string()
        };
        if scope.len() > 256 {
            return Err(self.auth_error(request, "invalid_scope", "scope is too long")?);
        }
        let id = random_token();
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .execute(
                "INSERT INTO pending_auth
                 (id, client_id, redirect_uri, challenge, scope, state, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    request.client_id,
                    request.redirect_uri,
                    request.code_challenge,
                    scope,
                    request.state,
                    (now() + CODE_TTL) as i64
                ],
            )
            .map_err(store_error)?;
        Ok(id)
    }

    pub fn pending_client(&self, id: &str) -> BridgeResult<String> {
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .query_row(
                "SELECT client_id FROM pending_auth WHERE id = ?1 AND expires_at > ?2",
                params![id, now() as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_error)?
            .ok_or_else(|| oauth_error("invalid_request", "this sign-in has expired"))
    }

    pub fn pending(&self, id: &str) -> BridgeResult<Option<(String, String)>> {
        let connection = self.connection.lock().expect("oauth lock");
        let row: Option<(String, String, i64)> = connection
            .query_row(
                "SELECT c.client_name, p.scope, p.expires_at FROM pending_auth p
                 JOIN clients c ON c.client_id = p.client_id WHERE p.id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(store_error)?;
        match row {
            Some((name, scope, exp)) if (exp as u64) > now() => Ok(Some((name, scope))),
            _ => Ok(None),
        }
    }

    /// Allow or deny. Allow issues a code and returns the client's redirect.
    pub fn consent(&self, request_id: &str, account_id: &str, allow: bool) -> BridgeResult<String> {
        if !is_account_id(account_id) {
            return Err(oauth_error("invalid_request", "unknown account"));
        }
        let mut connection = self.connection.lock().expect("oauth lock");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, String, String, String, String, i64)> = tx
            .query_row(
                "SELECT client_id, redirect_uri, challenge, scope, state, expires_at
                 FROM pending_auth WHERE id = ?1",
                params![request_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(store_error)?;
        let Some((client_id, redirect_uri, challenge, scope, state, exp)) = row else {
            return Err(oauth_error("invalid_request", "this sign-in has expired"));
        };
        tx.execute(
            "DELETE FROM pending_auth WHERE id = ?1",
            params![request_id],
        )
        .map_err(store_error)?;
        if (exp as u64) <= now() {
            tx.commit().map_err(store_error)?;
            return Err(oauth_error("invalid_request", "this sign-in has expired"));
        }
        if !allow {
            tx.commit().map_err(store_error)?;
            return Ok(redirect_with(
                &redirect_uri,
                &[("error", "access_denied"), ("state", &state)],
            ));
        }
        let code = random_token();
        tx.execute(
            "INSERT INTO auth_codes
             (code_hash, client_id, account_id, redirect_uri, challenge, scope, expires_at, used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            params![
                digest(&code).as_slice(),
                client_id,
                account_id,
                redirect_uri,
                challenge,
                scope,
                (now() + CODE_TTL) as i64
            ],
        )
        .map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(redirect_with(
            &redirect_uri,
            &[("code", &code), ("state", &state)],
        ))
    }

    pub fn start_device(&self, client_id: &str) -> BridgeResult<Value> {
        let _ = self.client(client_id)?;
        let device_code = random_token();
        let user_code = format!(
            "{}-{}",
            &hex::encode(crate::link::random::<2>()).to_uppercase(),
            &hex::encode(crate::link::random::<2>()).to_uppercase()
        );
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .execute(
                "INSERT INTO device_grants
                 (device_hash, user_code, client_id, scope, expires_at, account_id)
                 VALUES (?1, ?2, ?3, 'openid mcp', ?4, NULL)",
                params![
                    digest(&device_code).as_slice(),
                    user_code,
                    client_id,
                    (now() + CODE_TTL) as i64
                ],
            )
            .map_err(store_error)?;
        Ok(json!({
            "device_code": device_code,
            "user_code": user_code,
            "verification_uri": format!("{}/device", self.public_url),
            "expires_in": CODE_TTL,
            "interval": 1,
        }))
    }

    pub fn approve_device(&self, user_code: &str, account_id: &str) -> BridgeResult<()> {
        if !is_account_id(account_id) {
            return Err(oauth_error("invalid_request", "unknown account"));
        }
        let connection = self.connection.lock().expect("oauth lock");
        let updated = connection
            .execute(
                "UPDATE device_grants SET account_id = ?1
                 WHERE user_code = ?2 AND account_id IS NULL AND expires_at > ?3",
                params![account_id, user_code, now() as i64],
            )
            .map_err(store_error)?;
        if updated == 0 {
            return Err(oauth_error(
                "invalid_grant",
                "unknown or expired device code",
            ));
        }
        Ok(())
    }

    pub fn token(&self, request: &TokenRequest) -> BridgeResult<TokenResponse> {
        match request.grant_type.as_str() {
            "authorization_code" => self.exchange_code(request),
            "refresh_token" => self.exchange_refresh(request),
            "urn:ietf:params:oauth:grant-type:device_code" => self.exchange_device(request),
            _ => Err(oauth_error(
                "unsupported_grant_type",
                "unsupported grant_type",
            )),
        }
    }

    pub fn revoke(&self, token: &str) -> BridgeResult<()> {
        if token.is_empty() || token.len() > 16 * 1024 {
            return Ok(());
        }
        if token.matches('.').count() == 2 {
            if let Ok(claims) = self.access_claims(token) {
                let connection = self.connection.lock().expect("oauth lock");
                connection
                    .execute(
                        "INSERT OR IGNORE INTO revoked_jti (jti, expires_at) VALUES (?1, ?2)",
                        params![claims.jti, claims.exp as i64],
                    )
                    .map_err(store_error)?;
            }
            return Ok(());
        }
        let connection = self.connection.lock().expect("oauth lock");
        let family: Option<String> = connection
            .query_row(
                "SELECT family FROM refresh_tokens WHERE token_hash = ?1",
                params![digest(token).as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_error)?;
        if let Some(family) = family {
            connection
                .execute(
                    "DELETE FROM refresh_tokens WHERE family = ?1",
                    params![family],
                )
                .map_err(store_error)?;
        }
        Ok(())
    }

    pub fn verify_access(&self, token: &str) -> Result<Subject, AuthError> {
        let claims = self.access_claims(token)?;
        if self.jti_revoked(&claims.jti) {
            return Err(AuthError::Invalid("token was revoked"));
        }
        let id = self
            .identity
            .canonical(&claims.sub)
            .unwrap_or(claims.sub.clone());
        let provisional_until = self.identity.provisional_until(&id).ok().flatten();
        if provisional_until.is_some_and(|until| until <= now()) {
            return Err(AuthError::Invalid("this provisional account has expired"));
        }
        if let Some(agent) = &claims.agent
            && self.identity.agent_revoked(&id, agent).unwrap_or(false)
        {
            return Err(AuthError::Invalid("this agent was revoked"));
        }
        let claim_code = provisional_until
            .as_ref()
            .and_then(|_| self.identity.claim_code_for(&id).ok().flatten());
        Ok(Subject {
            id,
            via: "devagotchi",
            name: None,
            org: None,
            provisional_until,
            agent: claims.agent,
            claim_code,
            provisional_warning: provisional_until.is_some_and(|until| {
                let left = until.saturating_sub(now());
                let ttl = self.limit_snapshot().0;
                left < (ttl / 7).max(3600)
            }),
        })
    }

    /// Open an account for an agent and hand it tokens immediately. `ip` is the
    /// rate-limit key. The claim code is how a person later attaches a sign-in
    /// to this same account.
    pub fn agent_start(
        &self,
        client_id: &str,
        agent: &str,
        ip: &str,
    ) -> BridgeResult<AgentSession> {
        let _ = self.client(client_id)?;
        let agent = if crate::config::valid_name(agent) {
            agent.to_string()
        } else {
            "agent".to_string()
        };
        self.allow_start(ip)?;
        let ttl = self.limit_snapshot().0;
        let created = self.identity.create_provisional(ttl)?;
        self.identity
            .register_agent(&created.account_id, &agent, client_id)?;
        let claim_code = self.identity.issue_claim_code(&created.account_id, ttl)?;
        let until = self
            .identity
            .provisional_until(&created.account_id)?
            .unwrap_or(now() + ttl);
        let mut connection = self.connection.lock().expect("oauth lock");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let tokens = issue_agent_tokens(
            &tx,
            &self.public_url,
            &self.keys,
            client_id,
            &created.account_id,
            "openid mcp",
            &agent,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(AgentSession {
            account_id: created.account_id,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_in: tokens.expires_in,
            claim_code,
            provisional_until: until,
        })
    }

    pub fn claim_with(
        &self,
        code: &str,
        proved: &crate::identity::Claim,
    ) -> BridgeResult<crate::identity::Claimed> {
        let claimed = self.identity.claim(code, proved)?;
        if let Some(from) = &claimed.merged_from {
            self.merges
                .lock()
                .expect("merge lock")
                .push((from.clone(), claimed.account_id.clone()));
            let connection = self.connection.lock().expect("oauth lock");
            connection
                .execute(
                    "UPDATE refresh_tokens SET account_id = ?1 WHERE account_id = ?2",
                    params![claimed.account_id, from],
                )
                .map_err(store_error)?;
        }
        Ok(claimed)
    }

    #[cfg(test)]
    pub fn testing_limits(&self, cap: usize, per_hour: usize) {
        let mut limits = self.limits.lock().expect("limits");
        limits.unclaimed_cap = cap;
        limits.starts_per_hour = per_hour;
        limits.ttl_secs = 2;
    }

    pub fn drain_merges(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.merges.lock().expect("merge lock"))
    }

    fn allow_start(&self, ip: &str) -> BridgeResult<()> {
        let (_, per_hour, cap) = self.limit_snapshot();
        if self.identity.provisional_count()? >= cap as u64 {
            return Err(oauth_error(
                "provisional_quota",
                "this hub is not opening more unclaimed accounts right now",
            ));
        }
        let mut starts = self.starts.lock().expect("rate lock");
        let now = now();
        let window = starts.entry(ip.to_string()).or_default();
        window.retain(|seen| now.saturating_sub(*seen) < 3600);
        if window.len() >= per_hour {
            return Err(oauth_error(
                "rate_limited",
                "too many new accounts from this address; try again later",
            ));
        }
        window.push(now);
        Ok(())
    }

    fn access_claims(&self, token: &str) -> Result<AccessClaims, AuthError> {
        let header =
            jsonwebtoken::decode_header(token).map_err(|_| AuthError::Invalid("not a JWT"))?;
        if header.alg != Algorithm::ES256 {
            return Err(AuthError::Invalid("unsupported signing algorithm"));
        }
        let kid = header.kid.ok_or(AuthError::Invalid("token has no kid"))?;
        let key = self
            .decoding_key(&kid)
            .ok_or(AuthError::Invalid("unknown signing key"))?;
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_issuer(&[self.public_url.as_str()]);
        let resource = self.resource();
        validation.set_audience(&[resource.as_str(), self.public_url.as_str()]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = 30;
        let data = decode::<AccessClaims>(token, &key, &validation).map_err(|_| {
            AuthError::Invalid("signature, issuer, audience or expiry check failed")
        })?;
        if !is_account_id(&data.claims.sub) {
            return Err(AuthError::Invalid("subject is not a Devagotchi account"));
        }
        Ok(data.claims)
    }

    /// A browser session for the consent screen. The cookie value is returned;
    /// only its hash is stored.
    pub fn open_browser_session(&self, account_id: &str) -> BridgeResult<String> {
        if !is_account_id(account_id) {
            return Err(oauth_error("invalid_request", "unknown account"));
        }
        let id = random_token();
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .execute(
                "INSERT INTO browser_sessions (id_hash, account_id, expires_at) VALUES (?1, ?2, ?3)",
                params![digest(&id).as_slice(), account_id, (now() + SESSION_TTL) as i64],
            )
            .map_err(store_error)?;
        Ok(id)
    }

    pub fn browser_account(&self, cookie: &str) -> Option<String> {
        if cookie.is_empty() || cookie.len() > 128 {
            return None;
        }
        let connection = self.connection.lock().expect("oauth lock");
        let row = connection
            .query_row(
                "SELECT account_id, expires_at FROM browser_sessions WHERE id_hash = ?1",
                params![digest(cookie).as_slice()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .ok()
            .flatten()?;
        (row.1 as u64 > now()).then_some(row.0)
    }

    pub fn end_browser_session(&self, cookie: &str) {
        if cookie.is_empty() {
            return;
        }
        let connection = self.connection.lock().expect("oauth lock");
        let _ = connection.execute(
            "DELETE FROM browser_sessions WHERE id_hash = ?1",
            params![digest(cookie).as_slice()],
        );
    }

    fn seed_cli_client(&self) -> BridgeResult<()> {
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .execute(
                "INSERT OR IGNORE INTO clients
                 (client_id, client_name, redirect_uris, auth_method, created_at)
                 VALUES ('devagotchi-cli', 'Devagotchi CLI', '[]', 'none', ?1)",
                params![now() as i64],
            )
            .map_err(store_error)?;
        Ok(())
    }

    fn client(&self, client_id: &str) -> BridgeResult<ClientRow> {
        let connection = self.connection.lock().expect("oauth lock");
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT client_name, redirect_uris FROM clients WHERE client_id = ?1",
                params![client_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_error)?;
        let Some((name, redirects)) = row else {
            return Err(oauth_error("invalid_client", "unknown client_id"));
        };
        let redirects: Vec<String> = serde_json::from_str(&redirects).unwrap_or_default();
        Ok(ClientRow { name, redirects })
    }

    fn auth_error(
        &self,
        request: &AuthorizeRequest,
        error: &str,
        description: &str,
    ) -> BridgeResult<BridgeError> {
        // Only send the error to a redirect we already trust.
        if self.client(&request.client_id).ok().is_some_and(|client| {
            client
                .redirects
                .iter()
                .any(|uri| uri == &request.redirect_uri)
        }) {
            let _ = description;
            return Ok(oauth_error(
                "redirect",
                &redirect_with(
                    &request.redirect_uri,
                    &[("error", error), ("state", &request.state)],
                ),
            ));
        }
        Err(oauth_error(error, description))
    }

    fn exchange_code(&self, request: &TokenRequest) -> BridgeResult<TokenResponse> {
        let code = request
            .code
            .as_deref()
            .filter(|code| !code.is_empty())
            .ok_or_else(|| oauth_error("invalid_request", "code is required"))?;
        let verifier = request
            .code_verifier
            .as_deref()
            .filter(|v| verifier_ok(v))
            .ok_or_else(|| oauth_error("invalid_request", "code_verifier is required"))?;
        let client_id = request
            .client_id
            .as_deref()
            .ok_or_else(|| oauth_error("invalid_client", "client_id is required"))?;
        let redirect = request
            .redirect_uri
            .as_deref()
            .ok_or_else(|| oauth_error("invalid_request", "redirect_uri is required"))?;
        let mut connection = self.connection.lock().expect("oauth lock");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, String, String, String, String, i64, i64)> = tx
            .query_row(
                "SELECT client_id, account_id, redirect_uri, challenge, scope, expires_at, used
                 FROM auth_codes WHERE code_hash = ?1",
                params![digest(code).as_slice()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()
            .map_err(store_error)?;
        let Some((bound_client, account, bound_redirect, challenge, scope, exp, used)) = row else {
            return Err(oauth_error("invalid_grant", "unknown code"));
        };
        if used != 0
            || (exp as u64) <= now()
            || bound_client != client_id
            || bound_redirect != redirect
        {
            tx.execute(
                "UPDATE auth_codes SET used = 1 WHERE code_hash = ?1",
                params![digest(code).as_slice()],
            )
            .map_err(store_error)?;
            tx.commit().map_err(store_error)?;
            return Err(oauth_error("invalid_grant", "code is not valid"));
        }
        if s256(verifier) != challenge {
            return Err(oauth_error("invalid_grant", "code_verifier does not match"));
        }
        tx.execute(
            "UPDATE auth_codes SET used = 1 WHERE code_hash = ?1",
            params![digest(code).as_slice()],
        )
        .map_err(store_error)?;
        let response = issue_tokens(
            &tx,
            &self.public_url,
            &self.keys,
            client_id,
            &account,
            &scope,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(response)
    }

    fn exchange_device(&self, request: &TokenRequest) -> BridgeResult<TokenResponse> {
        let device = request
            .device_code
            .as_deref()
            .filter(|code| !code.is_empty())
            .ok_or_else(|| oauth_error("invalid_request", "device_code is required"))?;
        let client_id = request
            .client_id
            .as_deref()
            .ok_or_else(|| oauth_error("invalid_client", "client_id is required"))?;
        let mut connection = self.connection.lock().expect("oauth lock");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, String, Option<String>, i64)> = tx
            .query_row(
                "SELECT client_id, scope, account_id, expires_at FROM device_grants WHERE device_hash = ?1",
                params![digest(device).as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(store_error)?;
        let Some((bound, scope, account, exp)) = row else {
            return Err(oauth_error("invalid_grant", "unknown device code"));
        };
        if bound != client_id || (exp as u64) <= now() {
            return Err(oauth_error("invalid_grant", "device code is not valid"));
        }
        let Some(account) = account else {
            return Err(oauth_error(
                "authorization_pending",
                "waiting for the person",
            ));
        };
        tx.execute(
            "DELETE FROM device_grants WHERE device_hash = ?1",
            params![digest(device).as_slice()],
        )
        .map_err(store_error)?;
        let response = issue_tokens(
            &tx,
            &self.public_url,
            &self.keys,
            client_id,
            &account,
            &scope,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(response)
    }

    fn exchange_refresh(&self, request: &TokenRequest) -> BridgeResult<TokenResponse> {
        let token = request
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| oauth_error("invalid_request", "refresh_token is required"))?;
        let client_id = request
            .client_id
            .as_deref()
            .ok_or_else(|| oauth_error("invalid_client", "client_id is required"))?;
        let mut connection = self.connection.lock().expect("oauth lock");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let row: Option<(String, String, String, String, i64, i64)> = tx
            .query_row(
                "SELECT family, client_id, account_id, scope, expires_at, used
                 FROM refresh_tokens WHERE token_hash = ?1",
                params![digest(token).as_slice()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(store_error)?;
        let Some((family, bound_client, account, scope, exp, used)) = row else {
            return Err(oauth_error("invalid_grant", "unknown refresh token"));
        };
        if bound_client != client_id {
            return Err(oauth_error(
                "invalid_grant",
                "refresh token was issued to another client",
            ));
        }
        if used != 0 {
            tx.execute(
                "DELETE FROM refresh_tokens WHERE family = ?1",
                params![family],
            )
            .map_err(store_error)?;
            tx.commit().map_err(store_error)?;
            return Err(oauth_error(
                "invalid_grant",
                "refresh token was already used; the session was revoked",
            ));
        }
        if (exp as u64) <= now() {
            return Err(oauth_error("invalid_grant", "refresh token has expired"));
        }
        tx.execute(
            "UPDATE refresh_tokens SET used = 1 WHERE token_hash = ?1",
            params![digest(token).as_slice()],
        )
        .map_err(store_error)?;
        let response = issue_tokens_in_family(
            &tx,
            &self.public_url,
            &self.keys,
            client_id,
            &account,
            &scope,
            &family,
        )?;
        tx.commit().map_err(store_error)?;
        Ok(response)
    }

    fn decoding_key(&self, kid: &str) -> Option<DecodingKey> {
        let keys = self.keys.lock().expect("oauth key lock");
        let now = now();
        let public = if keys.current.kid == kid {
            Some((keys.current.x.as_str(), keys.current.y.as_str()))
        } else {
            keys.previous.iter().find_map(|key| {
                (key.kid == kid && key.retire_at > now).then_some((key.x.as_str(), key.y.as_str()))
            })
        }?;
        let jwk: Jwk = serde_json::from_value(public_jwk(kid, public.0, public.1)).ok()?;
        DecodingKey::from_jwk(&jwk).ok()
    }

    fn jti_revoked(&self, jti: &str) -> bool {
        let connection = self.connection.lock().expect("oauth lock");
        connection
            .query_row(
                "SELECT 1 FROM revoked_jti WHERE jti = ?1 AND expires_at > ?2",
                params![jti, now() as i64],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }
}

struct ClientRow {
    #[allow(dead_code)]
    name: String,
    redirects: Vec<String>,
}

#[derive(Deserialize)]
struct AccessClaims {
    sub: String,
    exp: u64,
    jti: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    provisional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    pub claim_code: String,
    pub provisional_until: u64,
}

fn issue_agent_tokens(
    tx: &rusqlite::Transaction<'_>,
    issuer: &str,
    keys: &Mutex<KeyRing>,
    client_id: &str,
    account: &str,
    scope: &str,
    agent: &str,
) -> BridgeResult<TokenResponse> {
    let family = random_token();
    let refresh = random_token();
    tx.execute(
        "INSERT INTO refresh_tokens
         (token_hash, family, client_id, account_id, scope, expires_at, used)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        params![
            digest(&refresh).as_slice(),
            family,
            client_id,
            account,
            scope,
            (now() + REFRESH_TTL) as i64
        ],
    )
    .map_err(store_error)?;
    let access = sign_access(issuer, keys, account, client_id, scope, Some(agent), true)?;
    Ok(TokenResponse {
        access_token: access,
        refresh_token: refresh,
        expires_in: ACCESS_TTL,
        scope: scope.to_string(),
    })
}

fn issue_tokens(
    tx: &rusqlite::Transaction<'_>,
    issuer: &str,
    keys: &Mutex<KeyRing>,
    client_id: &str,
    account: &str,
    scope: &str,
) -> BridgeResult<TokenResponse> {
    let family = random_token();
    issue_tokens_in_family(tx, issuer, keys, client_id, account, scope, &family)
}

fn issue_tokens_in_family(
    tx: &rusqlite::Transaction<'_>,
    issuer: &str,
    keys: &Mutex<KeyRing>,
    client_id: &str,
    account: &str,
    scope: &str,
    family: &str,
) -> BridgeResult<TokenResponse> {
    let refresh = random_token();
    tx.execute(
        "INSERT INTO refresh_tokens
         (token_hash, family, client_id, account_id, scope, expires_at, used)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        params![
            digest(&refresh).as_slice(),
            family,
            client_id,
            account,
            scope,
            (now() + REFRESH_TTL) as i64
        ],
    )
    .map_err(store_error)?;
    let access = sign_access(issuer, keys, account, client_id, scope, None, false)?;
    Ok(TokenResponse {
        access_token: access,
        refresh_token: refresh,
        expires_in: ACCESS_TTL,
        scope: scope.to_string(),
    })
}

fn sign_access(
    issuer: &str,
    keys: &Mutex<KeyRing>,
    account: &str,
    client_id: &str,
    scope: &str,
    agent: Option<&str>,
    provisional: bool,
) -> BridgeResult<String> {
    let keys = keys.lock().expect("oauth key lock");
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(keys.current.kid.clone());
    let issued = now();
    let mut claims = json!({
        "iss": issuer,
        "sub": account,
        "aud": format!("{issuer}/mcp"),
        "exp": issued + ACCESS_TTL,
        "iat": issued,
        "jti": random_token(),
        "client_id": client_id,
        "scope": scope,
    });
    if let Some(agent) = agent {
        claims["agent"] = json!(agent);
    }
    if provisional {
        claims["provisional"] = json!(true);
    }
    let encoding = EncodingKey::from_ec_der(&keys.current.pkcs8);
    jsonwebtoken::encode(&header, &claims, &encoding)
        .map_err(|_| BridgeError::new("oauth", "could not sign an access token"))
}

fn redirect_with(uri: &str, pairs: &[(&str, &str)]) -> String {
    let query = pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencoding(value)))
        .collect::<Vec<_>>()
        .join("&");
    let joiner = if uri.contains('?') { '&' } else { '?' };
    format!("{uri}{joiner}{query}")
}

fn urlencoding(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn public_jwk(kid: &str, x: &str, y: &str) -> Value {
    json!({
        "kty": "EC",
        "crv": "P-256",
        "alg": "ES256",
        "use": "sig",
        "kid": kid,
        "x": x,
        "y": y,
    })
}

fn generate_signing_key() -> BridgeResult<SigningKey> {
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
    let rng = aws_lc_rs::rand::SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|_| BridgeError::new("oauth", "could not generate a signing key"))?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref())
        .map_err(|_| BridgeError::new("oauth", "could not read the signing key"))?;
    let public = pair.public_key().as_ref().to_vec();
    if public.len() != 65 || public[0] != 0x04 {
        return Err(BridgeError::new("oauth", "signing key has no P-256 point"));
    }
    let x = URL_SAFE_NO_PAD.encode(&public[1..33]);
    let y = URL_SAFE_NO_PAD.encode(&public[33..65]);
    let kid = hex::encode(&Sha256::digest(&public)[..8]);
    Ok(SigningKey {
        kid,
        pkcs8: pkcs8.as_ref().to_vec(),
        x,
        y,
        created_at: now(),
    })
}

fn rotate_ring(mut keys: KeyRing) -> KeyRing {
    let fresh = generate_signing_key().expect("signing key");
    keys.previous.push(PublicKey {
        kid: keys.current.kid.clone(),
        x: keys.current.x.clone(),
        y: keys.current.y.clone(),
        retire_at: now() + ACCESS_TTL + 60,
    });
    keys.current = fresh;
    keys
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredRing {
    current: StoredKey,
    #[serde(default)]
    previous: Vec<StoredPublic>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredKey {
    kid: String,
    pkcs8: String,
    x: String,
    y: String,
    created_at: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredPublic {
    kid: String,
    x: String,
    y: String,
    retire_at: u64,
}

fn load_or_create_keys(path: &Path) -> BridgeResult<KeyRing> {
    if path.exists() {
        let text = crate::auth::read_private(path.to_str().unwrap_or(""), "oauth signing key")?;
        let stored: StoredRing = serde_json::from_str(&text)
            .map_err(|_| BridgeError::new("oauth", "oauth signing key is not readable"))?;
        let pkcs8 = URL_SAFE_NO_PAD
            .decode(stored.current.pkcs8)
            .map_err(|_| BridgeError::new("oauth", "oauth signing key is not readable"))?;
        return Ok(KeyRing {
            current: SigningKey {
                kid: stored.current.kid,
                pkcs8,
                x: stored.current.x,
                y: stored.current.y,
                created_at: stored.current.created_at,
            },
            previous: stored
                .previous
                .into_iter()
                .map(|key| PublicKey {
                    kid: key.kid,
                    x: key.x,
                    y: key.y,
                    retire_at: key.retire_at,
                })
                .collect(),
        });
    }
    let keys = KeyRing {
        current: generate_signing_key()?,
        previous: Vec::new(),
    };
    save_keys(path, &keys)?;
    Ok(keys)
}

fn save_keys(path: &Path, keys: &KeyRing) -> BridgeResult<()> {
    let stored = StoredRing {
        current: StoredKey {
            kid: keys.current.kid.clone(),
            pkcs8: URL_SAFE_NO_PAD.encode(&keys.current.pkcs8),
            x: keys.current.x.clone(),
            y: keys.current.y.clone(),
            created_at: keys.current.created_at,
        },
        previous: keys
            .previous
            .iter()
            .map(|key| StoredPublic {
                kid: key.kid.clone(),
                x: key.x.clone(),
                y: key.y.clone(),
                retire_at: key.retire_at,
            })
            .collect(),
    };
    let text = serde_json::to_string_pretty(&stored)
        .map_err(|_| BridgeError::new("oauth", "could not store the signing key"))?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| BridgeError::new("oauth", error.to_string()))?;
    }
    crate::push::write_private(path, text.as_bytes())
}

impl Authority {
    pub fn rotate(&self) -> BridgeResult<()> {
        let mut keys = self.keys.lock().expect("oauth key lock");
        let fresh = generate_signing_key()?;
        let retired = PublicKey {
            kid: keys.current.kid.clone(),
            x: keys.current.x.clone(),
            y: keys.current.y.clone(),
            retire_at: now() + ACCESS_TTL + 60,
        };
        keys.previous.push(retired);
        keys.current = fresh;
        save_keys(&self.key_path, &keys)
    }
}

pub fn http_router<S>(auth: std::sync::Arc<Authority>) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    use axum::{
        Json,
        extract::{ConnectInfo, Form, Path, Query},
        response::IntoResponse,
        routing::{get, post},
    };
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct AuthorizeQuery {
        #[serde(default)]
        response_type: String,
        #[serde(default)]
        client_id: String,
        #[serde(default)]
        redirect_uri: String,
        #[serde(default)]
        code_challenge: String,
        #[serde(default)]
        code_challenge_method: String,
        #[serde(default)]
        state: String,
        #[serde(default)]
        scope: String,
    }
    #[derive(Deserialize)]
    struct ConsentForm {
        request: String,
        decision: String,
    }

    let metadata_auth = auth.clone();
    let jwks_auth = auth.clone();
    let register_auth = auth.clone();
    let authorize_auth = auth.clone();
    let consent_auth = auth.clone();
    let token_auth = auth.clone();
    let revoke_auth = auth.clone();
    let resume_auth = auth.clone();
    let account_auth = auth.clone();
    let revoke_agent_auth = auth.clone();
    let unlink_auth = auth.clone();
    let provisional_auth = auth.clone();
    let agent_auth = auth.clone();
    let device_auth = auth.clone();
    let login_auth = auth.clone();
    let claim_get = auth.clone();
    let claim_post = auth.clone();
    let recovery_auth = auth.clone();
    let magic_auth = auth.clone();
    let redeem_auth = auth.clone();
    let up_auth = auth.clone();
    let up_cb_auth = auth.clone();

    axum::Router::<S>::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(move || {
                let auth = metadata_auth.clone();
                async move { Json(auth.metadata()) }
            }),
        )
        .route(
            "/.well-known/jwks.json",
            get(move || {
                let auth = jwks_auth.clone();
                async move { Json(auth.jwks()) }
            }),
        )
        .route(
            "/register",
            post(move |Json(body): Json<Value>| {
                let auth = register_auth.clone();
                async move {
                    match auth.register(&body) {
                        Ok(doc) => (axum::http::StatusCode::CREATED, Json(doc)).into_response(),
                        Err(error) => oauth_http_error(&error),
                    }
                }
            }),
        )
        .route(
            "/authorize",
            get(move |Query(query): Query<AuthorizeQuery>, headers: axum::http::HeaderMap| {
                let auth = authorize_auth.clone();
                async move {
                    if query.response_type != "code" {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            "response_type must be code",
                        )
                            .into_response();
                    }
                    let request = AuthorizeRequest {
                        client_id: query.client_id,
                        redirect_uri: query.redirect_uri,
                        code_challenge: query.code_challenge,
                        code_challenge_method: query.code_challenge_method,
                        state: query.state,
                        scope: query.scope,
                    };
                    let pending = match auth.start_authorization(&request) {
                        Ok(id) => id,
                        Err(error) if error.code == "redirect" => {
                            return axum::response::Redirect::to(&error.message).into_response();
                        }
                        Err(error) => return oauth_http_error(&error),
                    };
                    let Some(account) = cookie_value(&headers, "dg_as").and_then(|cookie| auth.browser_account(&cookie)) else {
                        return (
                            axum::http::StatusCode::OK,
                            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            format!(
                                "<!doctype html><title>Allow</title>\
                                 <h1>Allow this app</h1>\
                                 <p>This opens a Devagotchi account for you. You can claim it later.</p>\
                                 <form method=\"post\" action=\"/authorize/provisional\">\
                                 <input type=\"hidden\" name=\"request\" value=\"{}\">\
                                 <button type=\"submit\">Allow</button></form>\
                                 <p><a href=\"/login?resume={}\">I already have an account</a></p>",
                                escape(&pending),
                                escape(&pending)
                            ),
                        )
                            .into_response();
                    };
                    let _ = account;
                    let Some((name, scope)) = auth.pending(&pending).ok().flatten() else {
                        return (axum::http::StatusCode::BAD_REQUEST, "this sign-in has expired").into_response();
                    };
                    (
                        axum::http::StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        format!(
                            "<!doctype html><title>Allow access</title>\
                             <h1>Allow {}</h1><p>Scopes: {}</p>\
                             <form method=\"post\" action=\"/authorize/consent\">\
                             <input type=\"hidden\" name=\"request\" value=\"{}\">\
                             <button name=\"decision\" value=\"allow\">Allow</button>\
                             <button name=\"decision\" value=\"deny\">Deny</button></form>",
                            escape(&name),
                            escape(&scope),
                            escape(&pending)
                        ),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/authorize/consent",
            post(move |headers: axum::http::HeaderMap, Form(form): Form<ConsentForm>| {
                let auth = consent_auth.clone();
                async move {
                    let Some(account) = cookie_value(&headers, "dg_as")
                        .and_then(|cookie| auth.browser_account(&cookie))
                    else {
                        return (axum::http::StatusCode::UNAUTHORIZED, "sign in required").into_response();
                    };
                    match auth.consent(&form.request, &account, form.decision == "allow") {
                        Ok(location) => axum::response::Redirect::to(&location).into_response(),
                        Err(error) => oauth_http_error(&error),
                    }
                }
            }),
        )
        .route(
            "/token",
            post(move |Form(form): Form<TokenRequest>| {
                let auth = token_auth.clone();
                async move {
                    match auth.token(&form) {
                        Ok(tokens) => Json(json!({
                            "access_token": tokens.access_token,
                            "token_type": "Bearer",
                            "expires_in": tokens.expires_in,
                            "refresh_token": tokens.refresh_token,
                            "scope": tokens.scope,
                        }))
                        .into_response(),
                        Err(error) => oauth_http_error(&error),
                    }
                }
            }),
        )
        .route(
            "/revoke",
            post(move |Form(form): Form<RevokeForm>| {
                let auth = revoke_auth.clone();
                async move {
                    let _ = auth.revoke(&form.token);
                    axum::http::StatusCode::OK.into_response()
                }
            }),
        )
        .route(
            "/authorize/resume",
            get(move |headers: axum::http::HeaderMap, Query(query): Query<ResumeRequest>| {
                let auth = resume_auth.clone();
                async move {
                    let Some(cookie) = cookie_value(&headers, "dg_as") else {
                        return (axum::http::StatusCode::UNAUTHORIZED, "sign in required").into_response();
                    };
                    if auth.browser_account(&cookie).is_none() {
                        return (axum::http::StatusCode::UNAUTHORIZED, "sign in required").into_response();
                    }
                    let Some((name, scope)) = auth.pending(&query.request).ok().flatten() else {
                        return (axum::http::StatusCode::BAD_REQUEST, "this sign-in has expired").into_response();
                    };
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        format!(
                            "<!doctype html><title>Allow access</title><h1>Allow {}</h1><p>Scopes: {}</p>\
                             <form method=\"post\" action=\"/authorize/consent\">\
                             <input type=\"hidden\" name=\"request\" value=\"{}\">\
                             <button name=\"decision\" value=\"allow\">Allow</button>\
                             <button name=\"decision\" value=\"deny\">Deny</button></form>",
                            escape(&name),
                            escape(&scope),
                            escape(&query.request)
                        ),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/account/identities",
            get(move |headers: axum::http::HeaderMap| {
                let auth = account_auth.clone();
                async move {
                    let Some(account) = cookie_value(&headers, "dg_as")
                        .and_then(|cookie| auth.browser_account(&cookie))
                    else {
                        return (axum::http::StatusCode::UNAUTHORIZED, "sign in required")
                            .into_response();
                    };
                    let identities = auth.identity().identities(&account).unwrap_or_default();
                    let mut rows = String::new();
                    for identity in identities {
                        rows.push_str(&format!(
                            "<li>{} {} <form method=\"post\" action=\"/account/identities/unlink\">\
                             <input type=\"hidden\" name=\"issuer\" value=\"{}\">\
                             <input type=\"hidden\" name=\"sub\" value=\"{}\">\
                             <button type=\"submit\">Unlink</button></form></li>",
                            escape(&identity.issuer),
                            escape(&identity.sub),
                            escape(&identity.issuer),
                            escape(&identity.sub),
                        ));
                    }
                    let mut agents = String::new();
                    for (name, revoked) in auth.identity().agents(&account).unwrap_or_default() {
                        let state = if revoked { "revoked" } else { "active" };
                        agents.push_str(&format!(
                            "<li>{} ({state}) <form method=\"post\" action=\"/account/agents/revoke\">\
                             <input type=\"hidden\" name=\"name\" value=\"{}\">\
                             <button type=\"submit\">Revoke</button></form></li>",
                            escape(&name),
                            escape(&name),
                        ));
                    }
                    let mut links = String::new();
                    for provider in auth.providers() {
                        links.push_str(&format!(
                            "<p><a href=\"/login/upstream/{}?resume=\">Link {}</a></p>",
                            escape(&provider.name),
                            escape(&provider.name),
                        ));
                    }
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        format!(
                            "<!doctype html><title>Sign-ins</title><h1>Sign-ins</h1>\
                             <p>Account {}</p><ul>{rows}</ul><h2>Agents</h2><ul>{agents}</ul>{links}",
                            escape(&account)
                        ),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/account/agents/revoke",
            post(move |headers: axum::http::HeaderMap, Form(form): Form<AgentRevokeForm>| {
                let auth = revoke_agent_auth.clone();
                async move {
                    let Some(account) = cookie_value(&headers, "dg_as")
                        .and_then(|cookie| auth.browser_account(&cookie))
                    else {
                        return (axum::http::StatusCode::UNAUTHORIZED, "sign in required")
                            .into_response();
                    };
                    match auth.identity().revoke_agent(&account, &form.name) {
                        Ok(()) => {
                            axum::response::Redirect::to("/account/identities").into_response()
                        }
                        Err(error) => {
                            (axum::http::StatusCode::BAD_REQUEST, error.message).into_response()
                        }
                    }
                }
            }),
        )
        .route(
            "/account/identities/unlink",
            post(move |headers: axum::http::HeaderMap, Form(form): Form<UnlinkForm>| {
                let auth = unlink_auth.clone();
                async move {
                    let Some(account) = cookie_value(&headers, "dg_as")
                        .and_then(|cookie| auth.browser_account(&cookie))
                    else {
                        return (axum::http::StatusCode::UNAUTHORIZED, "sign in required")
                            .into_response();
                    };
                    match auth.identity().unlink(&account, &form.issuer, &form.sub) {
                        Ok(()) => axum::response::Redirect::to("/account/identities").into_response(),
                        Err(error) => (
                            axum::http::StatusCode::BAD_REQUEST,
                            error.message,
                        )
                            .into_response(),
                    }
                }
            }),
        )
        .route(
            "/authorize/provisional",
            post(
                move |ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
                      Form(form): Form<ResumeRequest>| {
                    let auth = provisional_auth.clone();
                    async move {
                        let pending = match auth.pending(&form.request) {
                            Ok(Some(_)) => form.request.clone(),
                            _ => {
                                return (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    "this sign-in has expired",
                                )
                                    .into_response();
                            }
                        };
                        let client_id = match auth.pending_client(&pending) {
                            Ok(id) => id,
                            Err(error) => return oauth_http_error(&error),
                        };
                        let session = match auth.agent_start(&client_id, "connector", &peer.ip().to_string())
                        {
                            Ok(session) => session,
                            Err(error) => return oauth_http_error(&error),
                        };
                        let location = match auth.consent(&pending, &session.account_id, true) {
                            Ok(location) => location,
                            Err(error) => return oauth_http_error(&error),
                        };
                        let cookie = match auth.open_browser_session(&session.account_id) {
                            Ok(cookie) => cookie,
                            Err(error) => return oauth_http_error(&error),
                        };
                        let secure = if auth.issuer().starts_with("https://") {
                            "; Secure"
                        } else {
                            ""
                        };
                        (
                            axum::http::StatusCode::SEE_OTHER,
                            [
                                (
                                    axum::http::header::SET_COOKIE,
                                    format!("dg_as={cookie}; HttpOnly; SameSite=Lax; Path=/{secure}"),
                                ),
                                (axum::http::header::LOCATION, location),
                            ],
                        )
                            .into_response()
                    }
                },
            ),
        )
        .route(
            "/auth/agent/start",
            post(
                move |ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
                      Form(form): Form<AgentForm>| {
                    let auth = agent_auth.clone();
                    async move {
                        let agent = if form.agent.is_empty() {
                            "agent"
                        } else {
                            form.agent.as_str()
                        };
                        match auth.agent_start(&form.client_id, agent, &peer.ip().to_string()) {
                            Ok(session) => axum::Json(json!({
                                "account_id": session.account_id,
                                "access_token": session.access_token,
                                "refresh_token": session.refresh_token,
                                "expires_in": session.expires_in,
                                "token_type": "Bearer",
                                "claim_code": session.claim_code,
                                "claim_url": format!("{}/claim?code={}", auth.issuer(), session.claim_code),
                                "provisional_until": session.provisional_until,
                            }))
                            .into_response(),
                            Err(error) => oauth_http_error(&error),
                        }
                    }
                },
            ),
        )
        .route(
            "/device",
            post(move |Form(form): Form<DeviceForm>| {
                let auth = device_auth.clone();
                async move {
                    match auth.start_device(&form.client_id) {
                        Ok(body) => axum::Json(body).into_response(),
                        Err(error) => oauth_http_error(&error),
                    }
                }
            }),
        )
        .route(
            "/login",
            get(move |Query(query): Query<ResumeQuery>| {
                let auth = login_auth.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        auth.login_page(&query.resume.unwrap_or_default()),
                    )
                }
            }),
        )
        .route(
            "/claim",
            get(move |Query(query): Query<ClaimQuery>| {
                let auth = claim_get.clone();
                async move {
                    let resume = query
                        .code
                        .map(|code| format!("claim:{code}"))
                        .unwrap_or_default();
                    (
                        axum::http::StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        auth.login_page(&resume),
                    )
                }
            })
            .post(move |Form(form): Form<SetupClaimForm>| {
                let auth = claim_post.clone();
                async move {
                    let account = match auth.identity().account_for_claim_code(&form.code) {
                        Ok(account) => account,
                        Err(error) => return oauth_http_error(&error),
                    };
                    if let Err(error) = auth.identity().consume_setup_code(&form.setup, &account) {
                        return oauth_http_error(&error);
                    }
                    let claimed = match auth.claim_with(
                        &form.code,
                        &crate::identity::Claim {
                            issuer: "urn:devagotchi:setup".into(),
                            sub: "host".into(),
                            email: None,
                            email_verified: false,
                            name: None,
                        },
                    ) {
                        Ok(claimed) => claimed,
                        Err(error) => return oauth_http_error(&error),
                    };
                    let codes = auth
                        .identity()
                        .issue_recovery_codes(&claimed.account_id)
                        .unwrap_or_default();
                    let Ok(cookie) = auth.open_browser_session(&claimed.account_id) else {
                        return (axum::http::StatusCode::BAD_REQUEST, "could not start a session")
                            .into_response();
                    };
                    let secure = if auth.issuer().starts_with("https://") {
                        "; Secure"
                    } else {
                        ""
                    };
                    let listed = codes.join(", ");
                    (
                        axum::http::StatusCode::OK,
                        [(
                            axum::http::header::SET_COOKIE,
                            format!("dg_as={cookie}; HttpOnly; SameSite=Lax; Path=/{secure}"),
                        )],
                        format!(
                            "<!doctype html><title>Claimed</title><h1>This account is the hub owner</h1>\
                             <p>Account {}</p><p>Recovery codes, shown once: {}</p>\
                             <p><a href=\"/account/identities\">Account</a></p>",
                            escape(&claimed.account_id),
                            escape(&listed)
                        ),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/login/recovery",
            post(move |Form(form): Form<RecoveryForm>| {
                let auth = recovery_auth.clone();
                async move {
                    let Ok(account) = auth.identity().consume_recovery_code(&form.code) else {
                        return (axum::http::StatusCode::BAD_REQUEST, "recovery code is not valid")
                            .into_response();
                    };
                    let Ok(cookie) = auth.open_browser_session(&account) else {
                        return (axum::http::StatusCode::BAD_REQUEST, "could not start a session")
                            .into_response();
                    };
                    let secure = if auth.issuer().starts_with("https://") {
                        "; Secure"
                    } else {
                        ""
                    };
                    (
                        axum::http::StatusCode::SEE_OTHER,
                        [
                            (
                                axum::http::header::SET_COOKIE,
                                format!("dg_as={cookie}; HttpOnly; SameSite=Lax; Path=/{secure}"),
                            ),
                            (
                                axum::http::header::LOCATION,
                                "/account/identities".to_string(),
                            ),
                        ],
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/login/magic",
            post(move |Form(form): Form<MagicForm>| {
                let auth = magic_auth.clone();
                async move {
                    if auth
                        .request_magic_link(&form.email, form.resume.as_deref().unwrap_or(""))
                        .is_err()
                        && !crate::identity::acceptable_email(&form.email)
                    {
                        return (axum::http::StatusCode::BAD_REQUEST, "email is not usable").into_response();
                    }
                    (
                        axum::http::StatusCode::OK,
                        "If that address can sign in, a link was issued.",
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/login/redeem",
            get(move |Query(query): Query<TokenQuery>| {
                let auth = redeem_auth.clone();
                async move {
                    let Ok((account, resume)) = auth.redeem_magic_link(&query.token) else {
                        return (axum::http::StatusCode::BAD_REQUEST, "sign-in link is not valid").into_response();
                    };
                    let Ok(cookie) = auth.open_browser_session(&account) else {
                        return (axum::http::StatusCode::BAD_REQUEST, "could not start a session").into_response();
                    };
                    let secure = if auth.issuer().starts_with("https://") { "; Secure" } else { "" };
                    let set = format!("dg_as={cookie}; HttpOnly; SameSite=Lax; Path=/{secure}");
                    let next = if resume.is_empty() {
                        "/account/identities".to_string()
                    } else {
                        format!("/authorize/resume?request={resume}")
                    };
                    (
                        axum::http::StatusCode::SEE_OTHER,
                        [
                            (axum::http::header::SET_COOKIE, set),
                            (axum::http::header::LOCATION, next),
                        ],
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/login/upstream/{name}",
            get(move |Path(name): Path<String>, Query(query): Query<ResumeQuery>| {
                let auth = up_auth.clone();
                async move {
                    match auth.start_upstream(&name, query.resume.as_deref().unwrap_or("")).await {
                        Ok(location) => axum::response::Redirect::to(&location).into_response(),
                        Err(error) => oauth_http_error(&error),
                    }
                }
            }),
        )
        .route(
            "/login/upstream/{name}/callback",
            get(move |headers: axum::http::HeaderMap, Query(query): Query<CallbackQuery>| {
                let auth = up_cb_auth.clone();
                async move {
                    let signed_in = cookie_value(&headers, "dg_as").and_then(|cookie| auth.browser_account(&cookie));
                    let Ok((account, resume)) = auth
                        .finish_upstream(&query.state, &query.code, signed_in.as_deref())
                        .await
                    else {
                        return (axum::http::StatusCode::BAD_REQUEST, "sign-in did not finish").into_response();
                    };
                    let account = signed_in.unwrap_or(account);
                    let Ok(cookie) = auth.open_browser_session(&account) else {
                        return (axum::http::StatusCode::BAD_REQUEST, "could not start a session").into_response();
                    };
                    let secure = if auth.issuer().starts_with("https://") { "; Secure" } else { "" };
                    let set = format!("dg_as={cookie}; HttpOnly; SameSite=Lax; Path=/{secure}");
                    let next = if resume.is_empty() {
                        "/account/identities".into()
                    } else {
                        format!("/authorize/resume?request={resume}")
                    };
                    (
                        axum::http::StatusCode::SEE_OTHER,
                        [
                            (axum::http::header::SET_COOKIE, set),
                            (axum::http::header::LOCATION, next),
                        ],
                    )
                        .into_response()
                }
            }),
        )
}

#[derive(Deserialize)]
struct AgentRevokeForm {
    name: String,
}
#[derive(Deserialize)]
struct AgentForm {
    client_id: String,
    #[serde(default)]
    agent: String,
}
#[derive(Deserialize)]
struct UnlinkForm {
    issuer: String,
    sub: String,
}
#[derive(Deserialize)]
struct DeviceForm {
    client_id: String,
}
#[derive(Deserialize)]
struct ResumeRequest {
    #[serde(default)]
    request: String,
}
#[derive(Deserialize)]
struct ResumeQuery {
    #[serde(default)]
    resume: Option<String>,
}
#[derive(Deserialize)]
struct ClaimQuery {
    #[serde(default)]
    code: Option<String>,
}
#[derive(Deserialize)]
struct SetupClaimForm {
    code: String,
    setup: String,
}
#[derive(Deserialize)]
struct RecoveryForm {
    code: String,
}
#[derive(Deserialize)]
struct MagicForm {
    email: String,
    #[serde(default)]
    resume: Option<String>,
}
#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
}
#[derive(Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
}

#[derive(Deserialize)]
struct RevokeForm {
    token: String,
}

fn cookie_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn oauth_http_error(error: &devagotchi_contracts::BridgeError) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({
            "error": error.code,
            "error_description": error.message,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority() -> (tempfile::TempDir, Authority) {
        let dir = tempfile::tempdir().unwrap();
        let auth = Authority::open(dir.path(), "http://127.0.0.1:9").unwrap();
        (dir, auth)
    }

    fn authorize(auth: &Authority, redirect: &str) -> (String, String) {
        let registered = auth
            .register(&json!({
                "client_name": "Claude",
                "redirect_uris": [redirect],
                "token_endpoint_auth_method": "none"
            }))
            .unwrap();
        let client_id = registered["client_id"].as_str().unwrap().to_string();
        let verifier = "a".repeat(43);
        let challenge = s256(&verifier);
        let pending = auth
            .start_authorization(&AuthorizeRequest {
                client_id: client_id.clone(),
                redirect_uri: redirect.into(),
                code_challenge: challenge,
                code_challenge_method: "S256".into(),
                state: "state-1".into(),
                scope: "openid mcp".into(),
            })
            .unwrap();
        (client_id, pending)
    }

    #[test]
    fn registration_rejects_a_secret_client_and_a_public_http_redirect() {
        let (_dir, auth) = authority();
        let bad = auth.register(&json!({
            "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
            "token_endpoint_auth_method": "client_secret_basic"
        }));
        assert_eq!(bad.unwrap_err().code, "invalid_client_metadata");
        let http = auth.register(&json!({
            "redirect_uris": ["http://evil.example/callback"]
        }));
        assert_eq!(http.unwrap_err().code, "invalid_redirect_uri");
        let ok = auth
            .register(&json!({
                "redirect_uris": ["https://claude.ai/api/mcp/auth_callback", "http://127.0.0.1:9/cb"]
            }))
            .unwrap();
        assert!(ok["client_id"].as_str().unwrap().starts_with("dg_client_"));
    }

    #[test]
    fn pkce_consent_and_refresh_rotation_and_reuse() {
        let (_dir, auth) = authority();
        let redirect = "https://claude.ai/api/mcp/auth_callback";
        let (client, pending) = authorize(&auth, redirect);
        let account = format!("dg_{}", "ab".repeat(16));
        let location = auth.consent(&pending, &account, true).unwrap();
        assert!(location.contains("code="));
        assert!(location.contains("state=state-1"));
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let verifier = "a".repeat(43);
        let mismatch = auth.token(&TokenRequest {
            grant_type: "authorization_code".into(),
            code: Some(code.into()),
            redirect_uri: Some(redirect.into()),
            client_id: Some(client.clone()),
            code_verifier: Some("b".repeat(43)),
            refresh_token: None,
            device_code: None,
        });
        assert_eq!(mismatch.unwrap_err().code, "invalid_grant");
        // The failed verifier did not burn the code (the challenge check is before used=1
        // only if we return before the update). See exchange_code: mismatch returns
        // before marking used. A second try with the right verifier works.
        let tokens = auth
            .token(&TokenRequest {
                grant_type: "authorization_code".into(),
                code: Some(code.into()),
                redirect_uri: Some(redirect.into()),
                client_id: Some(client.clone()),
                code_verifier: Some(verifier),
                refresh_token: None,
                device_code: None,
            })
            .unwrap();
        let subject = auth.verify_access(&tokens.access_token).unwrap();
        assert_eq!(subject.id, account);
        assert_eq!(subject.via, "devagotchi");
        let next = auth
            .token(&TokenRequest {
                grant_type: "refresh_token".into(),
                code: None,
                redirect_uri: None,
                client_id: Some(client.clone()),
                code_verifier: None,
                refresh_token: Some(tokens.refresh_token.clone()),
                device_code: None,
            })
            .unwrap();
        assert_ne!(next.refresh_token, tokens.refresh_token);
        auth.verify_access(&next.access_token).unwrap();
        let reused = auth.token(&TokenRequest {
            grant_type: "refresh_token".into(),
            code: None,
            redirect_uri: None,
            client_id: Some(client),
            code_verifier: None,
            refresh_token: Some(tokens.refresh_token),
            device_code: None,
        });
        assert_eq!(reused.unwrap_err().code, "invalid_grant");
        let family_dead = auth.token(&TokenRequest {
            grant_type: "refresh_token".into(),
            code: None,
            redirect_uri: None,
            client_id: Some("ignored".into()),
            code_verifier: None,
            refresh_token: Some(next.refresh_token),
            device_code: None,
        });
        assert!(family_dead.is_err());
    }

    #[test]
    fn a_redirect_that_was_not_registered_is_not_followed() {
        let (_dir, auth) = authority();
        let (client, _) = authorize(&auth, "https://claude.ai/api/mcp/auth_callback");
        let err = auth
            .start_authorization(&AuthorizeRequest {
                client_id: client,
                redirect_uri: "https://evil.example/steal".into(),
                code_challenge: s256(&"a".repeat(43)),
                code_challenge_method: "S256".into(),
                state: "s".into(),
                scope: "mcp".into(),
            })
            .unwrap_err();
        assert_eq!(err.code, "invalid_request");
        assert!(!err.message.contains("evil.example"));
    }

    #[test]
    fn revoke_rejects_the_access_token_and_the_previous_key_still_verifies() {
        let (_dir, auth) = authority();
        let redirect = "http://127.0.0.1:9/cb";
        let (client, pending) = authorize(&auth, redirect);
        let account = format!("dg_{}", "ef".repeat(16));
        let location = auth.consent(&pending, &account, true).unwrap();
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let tokens = auth
            .token(&TokenRequest {
                grant_type: "authorization_code".into(),
                code: Some(code.into()),
                redirect_uri: Some(redirect.into()),
                client_id: Some(client.clone()),
                code_verifier: Some("a".repeat(43)),
                refresh_token: None,
                device_code: None,
            })
            .unwrap();
        let still_valid = auth
            .token(&TokenRequest {
                grant_type: "refresh_token".into(),
                code: None,
                redirect_uri: None,
                client_id: Some(client),
                code_verifier: None,
                refresh_token: Some(tokens.refresh_token),
                device_code: None,
            })
            .unwrap();
        auth.rotate().unwrap();
        auth.verify_access(&still_valid.access_token).unwrap();
        auth.revoke(&still_valid.access_token).unwrap();
        assert!(auth.verify_access(&still_valid.access_token).is_err());
        auth.revoke(&tokens.access_token).unwrap();
        assert!(auth.verify_access(&tokens.access_token).is_err());
    }

    #[test]
    fn metadata_points_at_this_hub() {
        let (_dir, auth) = authority();
        let meta = auth.metadata();
        assert_eq!(meta["issuer"], "http://127.0.0.1:9");
        assert_eq!(meta["code_challenge_methods_supported"][0], "S256");
        let resource = auth.protected_resource(true);
        assert_eq!(resource["resource"], "http://127.0.0.1:9/mcp");
        assert_eq!(resource["authorization_servers"][0], "http://127.0.0.1:9");
        let origin = auth.protected_resource(false);
        assert_eq!(origin["resource"], "http://127.0.0.1:9");
        assert!(auth.jwks()["keys"].as_array().unwrap().len() >= 1);
    }

    #[test]
    fn deny_redirects_with_access_denied_and_plain_pkce_is_refused() {
        let (_dir, auth) = authority();
        let redirect = "https://claude.ai/api/mcp/auth_callback";
        let (client, pending) = authorize(&auth, redirect);
        let account = format!("dg_{}", "11".repeat(16));
        let denied = auth.consent(&pending, &account, false).unwrap();
        assert!(denied.contains("error=access_denied"));
        assert!(denied.starts_with(redirect));
        let err = auth
            .start_authorization(&AuthorizeRequest {
                client_id: client,
                redirect_uri: redirect.into(),
                code_challenge: "not-s256".into(),
                code_challenge_method: "plain".into(),
                state: "s".into(),
                scope: "mcp".into(),
            })
            .unwrap_err();
        assert_eq!(err.code, "redirect");
        assert!(err.message.contains("error=invalid_request"));
    }

    #[tokio::test]
    async fn readiness_names_a_provider_that_cannot_be_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let mut auth = Authority::open(dir.path(), "http://127.0.0.1:9").unwrap();
        let alone = auth.readiness().await;
        assert_eq!(alone["ready"], true);
        assert_eq!(alone["signing_key"], true);
        assert_eq!(alone["identity_store"], true);
        auth.configure_login(
            vec![crate::config::UpstreamProvider {
                name: "gone".into(),
                issuer: "http://127.0.0.1:1".into(),
                client_id: "hub".into(),
                client_secret_file: None,
                scopes: None,
            }],
            crate::config::LocalLogin::default(),
        );
        let down = auth.readiness().await;
        assert_eq!(down["ready"], false);
        assert_eq!(down["signing_key"], true);
        let text = down["degraded"][0].as_str().unwrap();
        assert!(text.contains("gone"), "{text}");
    }

    #[test]
    fn a_provisional_account_expires() {
        let dir = tempfile::tempdir().unwrap();
        let auth = Authority::open(dir.path(), "http://127.0.0.1:9").unwrap();
        auth.testing_limits(10, 8);
        let session = auth
            .agent_start("devagotchi-cli", "claude", "203.0.113.8")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(auth.verify_access(&session.access_token).is_err());
    }

    #[test]
    fn saved_limits_win_over_hub_json_and_warnings_follow_the_live_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let auth = Authority::open(dir.path(), "http://127.0.0.1:9").unwrap();
        assert_eq!(auth.operator_settings().provisional.ttl_days, 14);
        auth.apply_config_limits(&crate::config::ProvisionalPolicy {
            ttl_days: 30,
            starts_per_hour: 4,
            unclaimed_cap: 9,
        });
        assert_eq!(
            auth.operator_settings().provisional,
            ProvisionalNumbers {
                ttl_days: 30,
                starts_per_hour: 4,
                unclaimed_cap: 9,
            }
        );
        let rejected = auth
            .update_operator_settings(&ProvisionalPatch {
                ttl_days: Some(0),
                ..ProvisionalPatch::default()
            })
            .unwrap_err();
        assert_eq!(rejected.code, "invalid_settings");
        assert_eq!(auth.operator_settings().provisional.ttl_days, 30);

        auth.update_operator_settings(&ProvisionalPatch {
            ttl_days: Some(1),
            starts_per_hour: Some(8),
            unclaimed_cap: Some(500),
        })
        .unwrap();
        let session = auth
            .agent_start("devagotchi-cli", "claude", "203.0.113.20")
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let left = session.provisional_until.saturating_sub(now);
        assert!((86_390..=86_400).contains(&left), "left={left}");
        let quiet = auth.verify_access(&session.access_token).unwrap();
        assert!(!quiet.provisional_warning);
        let until = quiet.provisional_until;

        auth.update_operator_settings(&ProvisionalPatch {
            ttl_days: Some(90),
            ..ProvisionalPatch::default()
        })
        .unwrap();
        let warned = auth.verify_access(&session.access_token).unwrap();
        assert!(warned.provisional_warning);
        assert_eq!(warned.provisional_until, until);

        let again = Authority::open(dir.path(), "http://127.0.0.1:9").unwrap();
        assert_eq!(again.operator_settings().provisional.ttl_days, 90);
        again.apply_config_limits(&crate::config::ProvisionalPolicy::default());
        assert_eq!(again.operator_settings().provisional.ttl_days, 90);
        assert_eq!(again.operator_settings().provisional.starts_per_hour, 8);
    }

    #[test]
    fn an_agent_opens_an_account_and_a_claim_keeps_or_merges_it() {
        let dir = tempfile::tempdir().unwrap();
        let auth = Authority::open(dir.path(), "http://127.0.0.1:9").unwrap();
        auth.testing_limits(10, 2);
        let session = auth
            .agent_start("devagotchi-cli", "claude", "203.0.113.5")
            .unwrap();
        let subject = auth.verify_access(&session.access_token).unwrap();
        assert_eq!(subject.id, session.account_id);
        assert!(subject.provisional_until.is_some());
        assert_eq!(subject.agent.as_deref(), Some("claude"));
        assert_eq!(
            subject.claim_code.as_deref(),
            Some(session.claim_code.as_str())
        );
        auth.agent_start("devagotchi-cli", "claude", "203.0.113.5")
            .unwrap();
        assert_eq!(
            auth.agent_start("devagotchi-cli", "claude", "203.0.113.5")
                .unwrap_err()
                .code,
            "rate_limited"
        );
        let kept = auth
            .claim_with(
                &session.claim_code,
                &crate::identity::Claim {
                    issuer: "https://idp.example".into(),
                    sub: "person-new".into(),
                    email: Some("new@example.com".into()),
                    email_verified: true,
                    name: None,
                },
            )
            .unwrap();
        assert_eq!(kept.account_id, session.account_id);
        assert!(kept.merged_from.is_none());
        let after = auth.verify_access(&session.access_token).unwrap();
        assert_eq!(after.id, session.account_id);
        assert!(after.provisional_until.is_none());
        assert!(after.claim_code.is_none());

        let existing = auth
            .identity()
            .resolve(&crate::identity::Claim {
                issuer: "https://idp.example".into(),
                sub: "person-old".into(),
                email: Some("ada@example.com".into()),
                email_verified: true,
                name: None,
            })
            .unwrap();
        let provisional = auth
            .agent_start("devagotchi-cli", "claude", "203.0.113.9")
            .unwrap();
        let merged = auth
            .claim_with(
                &provisional.claim_code,
                &crate::identity::Claim {
                    issuer: "https://other.example".into(),
                    sub: "person-other".into(),
                    email: Some("ada@example.com".into()),
                    email_verified: true,
                    name: None,
                },
            )
            .unwrap();
        assert_eq!(merged.account_id, existing.account_id);
        assert_eq!(
            merged.merged_from.as_deref(),
            Some(provisional.account_id.as_str())
        );
        let followed = auth.verify_access(&provisional.access_token).unwrap();
        assert_eq!(followed.id, existing.account_id);
        auth.identity()
            .revoke_agent(&existing.account_id, "claude")
            .unwrap();
        assert!(auth.verify_access(&provisional.access_token).is_err());
    }

    #[test]
    fn a_device_code_waits_until_the_person_approves() {
        let (_dir, auth) = authority();
        let started = auth.start_device("devagotchi-cli").unwrap();
        let device = started["device_code"].as_str().unwrap();
        let user = started["user_code"].as_str().unwrap();
        let pending = auth.token(&TokenRequest {
            grant_type: "urn:ietf:params:oauth:grant-type:device_code".into(),
            code: None,
            redirect_uri: None,
            client_id: Some("devagotchi-cli".into()),
            code_verifier: None,
            refresh_token: None,
            device_code: Some(device.into()),
        });
        assert_eq!(pending.unwrap_err().code, "authorization_pending");
        let account = format!("dg_{}", "22".repeat(16));
        auth.approve_device(user, &account).unwrap();
        let tokens = auth
            .token(&TokenRequest {
                grant_type: "urn:ietf:params:oauth:grant-type:device_code".into(),
                code: None,
                redirect_uri: None,
                client_id: Some("devagotchi-cli".into()),
                code_verifier: None,
                refresh_token: None,
                device_code: Some(device.into()),
            })
            .unwrap();
        assert_eq!(
            auth.verify_access(&tokens.access_token).unwrap().id,
            account
        );
    }

    #[tokio::test]
    async fn magic_link_and_a_second_provider_share_one_account() {
        let dir = tempfile::tempdir().unwrap();
        let idp = fake_provider("person-1", "ada@example.com", true).await;
        let other = fake_provider("person-2", "ada@example.com", true).await;
        let mut auth = Authority::open(dir.path(), "http://127.0.0.1:9").unwrap();
        auth.configure_login(
            vec![
                crate::config::UpstreamProvider {
                    name: "one".into(),
                    issuer: idp,
                    client_id: "hub".into(),
                    client_secret_file: None,
                    scopes: None,
                },
                crate::config::UpstreamProvider {
                    name: "two".into(),
                    issuer: other,
                    client_id: "hub".into(),
                    client_secret_file: None,
                    scopes: None,
                },
            ],
            crate::config::LocalLogin {
                magic_link: true,
                deliver: Some("log".into()),
            },
        );
        auth.request_magic_link("Ada@Example.com", "pending-1")
            .unwrap();
        let url = std::fs::read_dir(dir.path().join("login-outbox"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let body = std::fs::read_to_string(url).unwrap();
        let token = body.split("token=").nth(1).unwrap();
        let (account, resume) = auth.redeem_magic_link(token).unwrap();
        assert_eq!(resume, "pending-1");
        assert!(is_account_id(&account));
        let started = auth.start_upstream("two", "pending-2").await.unwrap();
        let state = started
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let state = urlencoding_decode(state);
        let (linked, _) = auth
            .finish_upstream(&state, "the-code", None)
            .await
            .unwrap();
        assert_eq!(linked, account);
        let unverified = fake_provider("person-3", "ada@example.com", false).await;
        auth.configure_login(
            vec![crate::config::UpstreamProvider {
                name: "plain".into(),
                issuer: unverified,
                client_id: "hub".into(),
                client_secret_file: None,
                scopes: None,
            }],
            crate::config::LocalLogin::default(),
        );
        let started = auth.start_upstream("plain", "x").await.unwrap();
        let state = started
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let (other_account, _) = auth
            .finish_upstream(&urlencoding_decode(state), "the-code", None)
            .await
            .unwrap();
        assert_ne!(other_account, account);
    }

    async fn fake_provider(sub: &str, email: &str, verified: bool) -> String {
        use axum::Router;
        use axum::routing::get;
        let sub = sub.to_string();
        let email = email.to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let issuer = format!("http://{addr}");
        let issuer_body = issuer.clone();
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let issuer = issuer_body.clone();
                    async move {
                        axum::Json(json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/authorize"),
                            "token_endpoint": format!("{issuer}/token"),
                            "userinfo_endpoint": format!("{issuer}/userinfo"),
                        }))
                    }
                }),
            )
            .route(
                "/token",
                axum::routing::post(|| async {
                    axum::Json(json!({"access_token": "provider-access", "token_type": "Bearer"}))
                }),
            )
            .route(
                "/userinfo",
                get(move || {
                    let sub = sub.clone();
                    let email = email.clone();
                    async move {
                        axum::Json(json!({
                            "sub": sub,
                            "email": email,
                            "email_verified": verified,
                        }))
                    }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        issuer
    }

    fn urlencoding_decode(value: &str) -> String {
        let mut out = Vec::new();
        let bytes = value.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'%' if i + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
                    out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                    i += 3;
                }
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                byte => {
                    out.push(byte);
                    i += 1;
                }
            }
        }
        String::from_utf8(out).unwrap_or_default()
    }
}
