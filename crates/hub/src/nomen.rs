//! Shippin sign-in: checking Nomen access tokens.
//!
//! Nomen (Shippin's identity provider, a Zitadel fork) issues opaque access tokens that
//! only Nomen can read. The hub asks Nomen about each one (token introspection, RFC 7662),
//! signing in as its own API application with a private-key JWT. Nomen answers `active`
//! only when the token is live and the hub's application is in its audience; the hub
//! then re-checks issuer, audience, expiry and subject itself, so a misconfigured Nomen
//! cannot widen what the hub accepts.
//!
//! Lessons from running Shippin, built in here:
//! - Nomen's answer about a token ("inactive", "another project's token") is reused for
//!   a minute; failing to reach Nomen never is, and it surfaces as `issuer_unavailable`,
//!   never as a refusal, so an outage cannot look like lost access.
//! - The hub starts without contacting Nomen, so its local commands and machine links
//!   keep working while Nomen is down.
//! - Redirects are not followed: the token and the hub's credential go to the issuer only.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::auth::{AuthError, Subject, read_private};

/// How long Nomen's answer about one token is reused. Signing out of Nomen reaches
/// the hub within this window.
const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_MAX: usize = 4096;
/// Introspection calls in flight at once; later callers wait, then get `issuer_unavailable`.
const MAX_IN_FLIGHT: usize = 16;
const QUEUE_WAIT: Duration = Duration::from_secs(5);
const MAX_ANSWER_BYTES: usize = 64 * 1024;
const ASSERTION_LIFETIME_SECS: u64 = 300;
const LEEWAY_SECS: u64 = 30;
const JWT_BEARER: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// The key file Nomen downloads for an API application that uses private-key JWT.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyFile {
    #[serde(rename = "type")]
    kind: String,
    key_id: String,
    key: String,
    client_id: String,
}

/// Nomen's answer about one token: only the fields the hub checks.
#[derive(Debug, Default, Deserialize)]
struct Introspection {
    active: bool,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    aud: Option<Audience>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn names_any(&self, accepted: &[String]) -> bool {
        match self {
            Audience::One(aud) => accepted.contains(aud),
            Audience::Many(all) => all.iter().any(|aud| accepted.contains(aud)),
        }
    }
}

#[derive(Serialize)]
struct AssertionClaims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

struct Remembered {
    outcome: Result<Subject, &'static str>,
    until: Instant,
}

pub struct NomenVerifier {
    issuer: String,
    audiences: Vec<String>,
    endpoint: String,
    client_id: String,
    key_id: String,
    key: EncodingKey,
    http: reqwest::Client,
    cache: Mutex<HashMap<[u8; 32], Remembered>>,
    in_flight: Semaphore,
}

impl NomenVerifier {
    /// Load the hub's API application key. Nomen is not contacted until the first sign-in.
    pub fn new(issuer: &str, audiences: &[String], key_file: &str) -> BridgeResult<Self> {
        Self::from_key(issuer, audiences, &read_private(key_file, "auth.key_file")?)
    }

    fn from_key(issuer: &str, audiences: &[String], key_json: &str) -> BridgeResult<Self> {
        // Errors describe the file, never its contents: it holds a private key.
        let invalid =
            |message: &str| BridgeError::new("invalid_config", format!("auth.key_file: {message}"));
        let key: KeyFile = serde_json::from_str(key_json)
            .map_err(|_| invalid("not a Nomen key file (JSON with type, keyId, key, clientId)"))?;
        if key.kind != "application" {
            return Err(invalid(
                "not an API application key; create it on the hub's API application in Nomen",
            ));
        }
        if key.key_id.trim().is_empty() || key.client_id.trim().is_empty() {
            return Err(invalid("keyId and clientId must not be empty"));
        }
        let encoding = EncodingKey::from_rsa_pem(key.key.as_bytes())
            .map_err(|_| invalid("key is not an RSA private key in PEM form"))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| BridgeError::new("auth_setup", error.to_string()))?;
        let issuer = issuer.trim_end_matches('/').to_string();
        let verifier = Self {
            endpoint: format!("{issuer}/oauth/v2/introspect"),
            issuer,
            audiences: audiences.to_vec(),
            client_id: key.client_id,
            key_id: key.key_id,
            key: encoding,
            http,
            cache: Mutex::new(HashMap::new()),
            in_flight: Semaphore::new(MAX_IN_FLIGHT),
        };
        // Sign once now: a key that cannot sign stops the hub at start, not at sign-in.
        verifier
            .assertion()
            .map_err(|_| invalid("the key cannot sign (RSA, 2048 bits or more)"))?;
        Ok(verifier)
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub async fn verify(&self, token: &str) -> Result<Subject, AuthError> {
        if !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(AuthError::Invalid(
                "token has characters no Nomen token carries",
            ));
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if let Some(outcome) = self.recall(&digest) {
            return outcome.map_err(AuthError::Invalid);
        }
        let _permit = tokio::time::timeout(QUEUE_WAIT, self.in_flight.acquire())
            .await
            .map_err(|_| AuthError::Unavailable("too many sign-in checks waiting on Nomen".into()))?
            .map_err(|_| AuthError::Unavailable("sign-in checks are shutting down".into()))?;
        // Another request may have asked about the same token while this one waited.
        if let Some(outcome) = self.recall(&digest) {
            return outcome.map_err(AuthError::Invalid);
        }
        let answer = self.introspect(token).await?;
        let now = unix_now();
        let outcome = assess(&answer, &self.issuer, &self.audiences, now);
        let ttl = match (&outcome, answer.exp) {
            (Ok(_), Some(exp)) => CACHE_TTL.min(Duration::from_secs(exp.saturating_sub(now))),
            _ => CACHE_TTL,
        };
        self.remember(digest, outcome.clone(), ttl);
        outcome.map_err(AuthError::Invalid)
    }

    async fn introspect(&self, token: &str) -> Result<Introspection, AuthError> {
        let assertion = self.assertion().map_err(|_| {
            AuthError::Unavailable("could not sign the hub's client assertion".into())
        })?;
        let response = self
            .http
            .post(&self.endpoint)
            .form(&[
                ("client_assertion_type", JWT_BEARER),
                ("client_assertion", assertion.as_str()),
                ("token", token),
            ])
            .send()
            .await
            .map_err(|error| AuthError::Unavailable(format!("Nomen introspection: {error}")))?;
        let status = response.status();
        if matches!(status.as_u16(), 400 | 401 | 403) {
            return Err(AuthError::Unavailable(format!(
                "Nomen refused the hub's own credentials ({status}): check auth.key_file and that the hub's API application is active"
            )));
        }
        if !status.is_success() {
            return Err(AuthError::Unavailable(format!(
                "Nomen introspection answered {status}"
            )));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| AuthError::Unavailable(format!("Nomen introspection: {error}")))?;
        if body.len() > MAX_ANSWER_BYTES {
            return Err(AuthError::Unavailable(
                "Nomen's introspection answer is too large".into(),
            ));
        }
        serde_json::from_slice(&body).map_err(|_| {
            AuthError::Unavailable("Nomen's introspection answer is not JSON the hub reads".into())
        })
    }

    /// The private-key JWT that proves to Nomen this request comes from the hub.
    fn assertion(&self) -> jsonwebtoken::errors::Result<String> {
        let now = unix_now();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.key_id.clone());
        let claims = AssertionClaims {
            iss: &self.client_id,
            sub: &self.client_id,
            aud: &self.issuer,
            iat: now,
            exp: now + ASSERTION_LIFETIME_SECS,
        };
        jsonwebtoken::encode(&header, &claims, &self.key)
    }

    fn recall(&self, digest: &[u8; 32]) -> Option<Result<Subject, &'static str>> {
        let mut cache = self.cache.lock().expect("nomen cache lock");
        match cache.get(digest) {
            Some(entry) if entry.until > Instant::now() => Some(entry.outcome.clone()),
            Some(_) => {
                cache.remove(digest);
                None
            }
            None => None,
        }
    }

    fn remember(&self, digest: [u8; 32], outcome: Result<Subject, &'static str>, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }
        let now = Instant::now();
        let mut cache = self.cache.lock().expect("nomen cache lock");
        if cache.len() >= CACHE_MAX {
            cache.retain(|_, entry| entry.until > now);
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(
            digest,
            Remembered {
                outcome,
                until: now + ttl,
            },
        );
    }
}

/// Decide from Nomen's answer alone; every refusal names its reason for the operator log.
fn assess(
    answer: &Introspection,
    issuer: &str,
    audiences: &[String],
    now: u64,
) -> Result<Subject, &'static str> {
    if !answer.active {
        return Err(
            "Nomen reports the token inactive: expired, signed out, or for another application",
        );
    }
    if answer.iss.as_deref().map(|iss| iss.trim_end_matches('/')) != Some(issuer) {
        return Err("the token's issuer is not this hub's Nomen");
    }
    if !answer
        .aud
        .as_ref()
        .is_some_and(|aud| aud.names_any(audiences))
    {
        return Err("the token was issued for a Nomen project this hub does not accept");
    }
    if !answer
        .exp
        .is_some_and(|exp| exp.saturating_add(LEEWAY_SECS) > now)
    {
        return Err("the token has expired");
    }
    let sub = answer
        .sub
        .as_deref()
        .map(str::trim)
        .filter(|sub| !sub.is_empty() && sub.len() <= 256 && !sub.contains('@'))
        .ok_or("Nomen named no usable account ID (sub) for the token")?;
    let name = answer
        .username
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
        .map(str::to_string);
    Ok(Subject {
        id: sub.to_string(),
        via: "nomen",
        name,
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
