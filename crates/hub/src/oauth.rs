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
use serde::Deserialize;
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
        let key_path = state_dir.join("oauth-signing.json");
        let mut keys = load_or_create_keys(&key_path)?;
        if now().saturating_sub(keys.current.created_at) > KEY_LIFE {
            keys = rotate_ring(keys);
            save_keys(&key_path, &keys)?;
        }
        let authority = Self {
            public_url,
            keys: Mutex::new(keys),
            key_path,
            connection: Mutex::new(connection),
        };
        authority.seed_cli_client()?;
        Ok(authority)
    }

    pub fn issuer(&self) -> &str {
        &self.public_url
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
            "grant_types_supported": ["authorization_code", "refresh_token"],
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

    pub fn token(&self, request: &TokenRequest) -> BridgeResult<TokenResponse> {
        match request.grant_type.as_str() {
            "authorization_code" => self.exchange_code(request),
            "refresh_token" => self.exchange_refresh(request),
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
        Ok(Subject {
            id: claims.sub,
            via: "devagotchi",
            name: None,
            org: None,
        })
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
    let access = sign_access(issuer, keys, account, client_id, scope)?;
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
) -> BridgeResult<String> {
    let keys = keys.lock().expect("oauth key lock");
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(keys.current.kid.clone());
    let issued = now();
    let claims = json!({
        "iss": issuer,
        "sub": account,
        "aud": format!("{issuer}/mcp"),
        "exp": issued + ACCESS_TTL,
        "iat": issued,
        "jti": random_token(),
        "client_id": client_id,
        "scope": scope,
    });
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
        Json, Router,
        extract::{Form, Query},
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
                            axum::http::StatusCode::UNAUTHORIZED,
                            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            format!(
                                "<!doctype html><title>Sign in</title><p>Sign in to Devagotchi to continue.</p>\
                                 <p><a href=\"/auth/login?resume={}\">Sign in</a></p>",
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
        });
        assert_eq!(reused.unwrap_err().code, "invalid_grant");
        let family_dead = auth.token(&TokenRequest {
            grant_type: "refresh_token".into(),
            code: None,
            redirect_uri: None,
            client_id: Some("ignored".into()),
            code_verifier: None,
            refresh_token: Some(next.refresh_token),
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
}
