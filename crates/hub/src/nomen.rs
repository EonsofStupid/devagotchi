//! Shippin sign-in: checking Nomen access tokens.
//!
//! Nomen (Shippin's identity provider, a Zitadel fork) issues opaque access tokens that
//! only Nomen can read. The hub asks Nomen about each one (token introspection, RFC 7662),
//! signing in as its own API application with a private-key JWT. Nomen answers `active`
//! only when the token is live and the hub's application is in its audience; the hub
//! then re-checks issuer, audience, client, expiry and subject itself, so a misconfigured
//! Nomen cannot widen what the hub accepts.
//!
//! Any application in Nomen can ask for the hub's audience, and a person signed in to
//! Nomen can be handed a token by any of them without typing anything. So the audience
//! says only that a token may be shown to the hub; the hub also requires that it was
//! issued to one of its configured clients, and grants match the account ID exactly.
//!
//! Lessons from running Shippin, built in here:
//! - Nomen's answer about a token ("inactive", "another project's token") is reused for
//!   a minute; failing to reach Nomen never is, and it surfaces as `issuer_unavailable`,
//!   never as a refusal, so an outage cannot look like lost access.
//! - An outage does not sign anyone out: a token Nomen confirmed keeps working while Nomen
//!   cannot be reached, until the token's own expiry (and never more than 12 hours after
//!   Nomen last confirmed it). Nobody can sign out of an unreachable Nomen either, so this
//!   honours nothing Nomen could have withdrawn; the first answer after it returns decides.
//! - An outage costs a caller seconds, not minutes: Nomen gets 3 s to connect and 5 s to
//!   answer, and once it has failed, sign-ins it never confirmed are refused at once while
//!   one request at a time tries again (after 2 s, doubling to 30 s). The outage and the
//!   recovery are each logged once, with how long it lasted; `health()` reports it.
//! - A flood of unknown tokens can delay new sign-ins, but never evicts a confirmed one.
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

use crate::{
    auth::{AuthError, Subject, read_private},
    config::valid_subject,
};

/// How long Nomen's answer about one token is reused, counted from when the hub asked.
/// Signing out of Nomen reaches the hub within this window.
const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_MAX: usize = 4096;
/// Introspection calls in flight at once; later callers wait, then get `issuer_unavailable`.
const MAX_IN_FLIGHT: usize = 16;
const QUEUE_WAIT: Duration = Duration::from_secs(5);
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// Nomen's time to accept a connection, and to answer once connected.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
/// A confirmed token keeps working through an outage until its own expiry, but never
/// longer than this after Nomen last confirmed it.
const HONOUR_MAX: Duration = Duration::from_secs(12 * 3600);
/// After a failed call to Nomen, the next try waits this long, doubling up to RETRY_MAX.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
const ASSERTION_LIFETIME_SECS: u64 = 300;
/// One signed assertion serves every introspection for this long.
const ASSERTION_REUSE_SECS: u64 = 60;
/// `iat` is backdated this much, so a hub clock slightly ahead of Nomen's still signs in.
const ASSERTION_BACKDATE_SECS: u64 = 5;
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
    client_id: Option<String>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    username: Option<String>,
    /// The account's org; Nomen includes it when the token was asked for with the
    /// `urn:zitadel:iam:user:resourceowner` scope.
    #[serde(default, rename = "urn:zitadel:iam:user:resourceowner:id")]
    org: Option<String>,
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

/// What the hub accepts, as configured.
struct Accepts {
    issuer: String,
    audiences: Vec<String>,
    clients: Vec<String>,
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
    /// When the hub asked Nomen; a later answer never yields to an earlier one.
    asked: Instant,
    until: Instant,
    /// For a confirmed sign-in: how long it is honoured while Nomen cannot be reached.
    honour_until: Option<Instant>,
}

/// Whether Nomen is answering, as the hub last found it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IssuerHealth {
    pub reachable: bool,
    /// Unix seconds when Nomen stopped answering, while it is down.
    pub down_since: Option<u64>,
    /// Unix seconds of Nomen's last answer.
    pub last_answer: Option<u64>,
    /// Why the last call failed, while it is down.
    pub last_error: Option<String>,
    /// Requests answered from an earlier confirmation during the current or last outage.
    pub honoured_during_outage: u64,
}

#[derive(Default)]
struct Outage {
    /// When (monotonic, and Unix seconds) Nomen stopped answering.
    since: Option<(Instant, u64)>,
    retry_at: Option<Instant>,
    backoff: Duration,
    last_error: Option<String>,
    last_answer: Option<u64>,
    honoured: u64,
}

pub struct NomenVerifier {
    accepts: Accepts,
    endpoint: String,
    client_id: String,
    key_id: String,
    key: EncodingKey,
    http: reqwest::Client,
    assertion: Mutex<Option<(String, u64)>>,
    cache: Mutex<HashMap<[u8; 32], Remembered>>,
    in_flight: Semaphore,
    outage: Mutex<Outage>,
    /// How long Nomen's answer is reused while it is reachable (CACHE_TTL; shorter in tests).
    fresh_for: Duration,
    /// The first and longest pause before trying Nomen again (RETRY_FIRST, RETRY_MAX).
    retry: (Duration, Duration),
}

impl NomenVerifier {
    /// Load the hub's API application key. Nomen is not contacted until the first sign-in.
    pub fn new(
        issuer: &str,
        audiences: &[String],
        clients: &[String],
        key_file: &str,
    ) -> BridgeResult<Self> {
        let key = read_private(key_file, "auth.key_file")?;
        Self::from_key(issuer, audiences, clients, &key)
    }

    fn from_key(
        issuer: &str,
        audiences: &[String],
        clients: &[String],
        key_json: &str,
    ) -> BridgeResult<Self> {
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
            .timeout(ANSWER_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| BridgeError::new("auth_setup", error.to_string()))?;
        let issuer = issuer.trim_end_matches('/').to_string();
        let verifier = Self {
            endpoint: format!("{issuer}/oauth/v2/introspect"),
            accepts: Accepts {
                issuer,
                audiences: audiences.to_vec(),
                clients: clients.to_vec(),
            },
            client_id: key.client_id,
            key_id: key.key_id,
            key: encoding,
            http,
            assertion: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            in_flight: Semaphore::new(MAX_IN_FLIGHT),
            outage: Mutex::new(Outage::default()),
            fresh_for: CACHE_TTL,
            retry: (RETRY_FIRST, RETRY_MAX),
        };
        // Sign once now: a key that cannot sign stops the hub at start, not at sign-in.
        verifier
            .sign_assertion(unix_now())
            .map_err(|_| invalid("the key cannot sign (RSA, 2048 bits or more)"))?;
        Ok(verifier)
    }

    pub fn issuer(&self) -> &str {
        &self.accepts.issuer
    }

    /// Whether Nomen is answering, for the status page and `whoami`.
    pub fn health(&self) -> IssuerHealth {
        let outage = self.outage.lock().expect("nomen outage lock");
        IssuerHealth {
            reachable: outage.since.is_none(),
            down_since: outage.since.map(|(_, unix)| unix),
            last_answer: outage.last_answer,
            last_error: outage.since.and(outage.last_error.clone()),
            honoured_during_outage: outage.honoured,
        }
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
        // While Nomen is known to be down, do not make this caller wait on it too.
        if let Some(why) = self.waiting_out_outage() {
            return self.honour(&digest).ok_or(AuthError::Unavailable(why));
        }
        let permit = tokio::time::timeout(QUEUE_WAIT, self.in_flight.acquire()).await;
        let _permit = match permit {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(AuthError::Unavailable("sign-in checks are shutting down".into())),
            Err(_) => {
                return self.honour(&digest).ok_or_else(|| {
                    AuthError::Unavailable("too many sign-in checks waiting on Nomen".into())
                });
            }
        };
        // Another request may have asked about the same token while this one waited.
        if let Some(outcome) = self.recall(&digest) {
            return outcome.map_err(AuthError::Invalid);
        }
        let asked = Instant::now();
        let answer = match self.introspect(token).await {
            Ok(answer) => {
                self.answered();
                answer
            }
            Err(AuthError::Unavailable(why)) => {
                self.failed(&why);
                return self.honour(&digest).ok_or(AuthError::Unavailable(why));
            }
            Err(refused) => return Err(refused),
        };
        let now = unix_now();
        let outcome = assess(&answer, &self.accepts, now);
        let (ttl, honour) = match (&outcome, answer.exp) {
            (Ok(_), Some(exp)) => {
                let left = Duration::from_secs(exp.saturating_sub(now));
                (self.fresh_for.min(left), Some(asked + HONOUR_MAX.min(left)))
            }
            _ => (self.fresh_for, None),
        };
        self.remember(digest, outcome.clone(), asked, ttl, honour);
        outcome.map_err(AuthError::Invalid)
    }

    /// A sign-in Nomen confirmed earlier, still honoured while Nomen cannot be reached.
    fn honour(&self, digest: &[u8; 32]) -> Option<Subject> {
        let subject = {
            let cache = self.cache.lock().expect("nomen cache lock");
            let entry = cache.get(digest)?;
            let subject = entry.outcome.as_ref().ok()?;
            (entry.honour_until? > Instant::now()).then(|| subject.clone())?
        };
        self.outage.lock().expect("nomen outage lock").honoured += 1;
        Some(subject)
    }

    /// While Nomen is down and its next try is not due, why callers should not wait on it.
    /// When the try is due, this caller makes it and the next is scheduled at once, so
    /// only one request at a time waits on an unreachable Nomen.
    fn waiting_out_outage(&self) -> Option<String> {
        let mut outage = self.outage.lock().expect("nomen outage lock");
        let (since, _) = outage.since?;
        let now = Instant::now();
        let due = outage.retry_at.is_none_or(|at| at <= now);
        if due {
            outage.backoff = (outage.backoff * 2).clamp(self.retry.0, self.retry.1);
            outage.retry_at = Some(now + outage.backoff);
            return None;
        }
        let wait = outage.retry_at.map_or(0, |at| at.saturating_duration_since(now).as_secs() + 1);
        Some(format!(
            "Nomen has not answered for {} s (last: {}); trying again in {wait} s",
            since.elapsed().as_secs(),
            outage.last_error.as_deref().unwrap_or("no answer"),
        ))
    }

    fn failed(&self, why: &str) {
        let mut outage = self.outage.lock().expect("nomen outage lock");
        if outage.since.is_none() {
            outage.since = Some((Instant::now(), unix_now()));
            outage.backoff = self.retry.0;
            outage.retry_at = Some(Instant::now() + self.retry.0);
            outage.honoured = 0;
            eprintln!(
                "devagotchi hub: Nomen unreachable ({why}). Sign-ins it confirmed keep working until their tokens expire; new ones wait for Nomen."
            );
        }
        outage.last_error = Some(why.to_string());
    }

    fn answered(&self) {
        let mut outage = self.outage.lock().expect("nomen outage lock");
        if let Some((since, _)) = outage.since.take() {
            eprintln!(
                "devagotchi hub: Nomen answering again after {} s; {} requests were honoured from earlier sign-ins meanwhile.",
                since.elapsed().as_secs(),
                outage.honoured
            );
        }
        outage.retry_at = None;
        outage.backoff = Duration::ZERO;
        outage.last_answer = Some(unix_now());
    }

    async fn introspect(&self, token: &str) -> Result<Introspection, AuthError> {
        let assertion = self.assertion().map_err(|_| {
            AuthError::Unavailable("could not sign the hub's client assertion".into())
        })?;
        let mut response = self
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
        let too_large =
            || AuthError::Unavailable("Nomen's introspection answer is too large".into());
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ANSWER_BYTES as u64)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| AuthError::Unavailable(format!("Nomen introspection: {error}")))?
        {
            if body.len() + chunk.len() > MAX_ANSWER_BYTES {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| {
            AuthError::Unavailable("Nomen's introspection answer is not JSON the hub reads".into())
        })
    }

    /// The private-key JWT that proves to Nomen a request comes from the hub, reused for a
    /// minute so a stream of unknown tokens does not cost an RSA signature each.
    fn assertion(&self) -> jsonwebtoken::errors::Result<String> {
        let now = unix_now();
        let mut current = self.assertion.lock().expect("assertion lock");
        if let Some((assertion, minted)) = current.as_ref()
            && now.saturating_sub(*minted) < ASSERTION_REUSE_SECS
            && *minted <= now
        {
            return Ok(assertion.clone());
        }
        let assertion = self.sign_assertion(now)?;
        *current = Some((assertion.clone(), now));
        Ok(assertion)
    }

    fn sign_assertion(&self, now: u64) -> jsonwebtoken::errors::Result<String> {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.key_id.clone());
        let claims = AssertionClaims {
            iss: &self.client_id,
            sub: &self.client_id,
            aud: &self.accepts.issuer,
            iat: now.saturating_sub(ASSERTION_BACKDATE_SECS),
            exp: now + ASSERTION_LIFETIME_SECS,
        };
        jsonwebtoken::encode(&header, &claims, &self.key)
    }

    fn recall(&self, digest: &[u8; 32]) -> Option<Result<Subject, &'static str>> {
        let mut cache = self.cache.lock().expect("nomen cache lock");
        let now = Instant::now();
        match cache.get(digest) {
            Some(entry) if entry.until > now => Some(entry.outcome.clone()),
            // Past its minute but still honoured through an outage: kept, not answered.
            Some(entry) if entry.honour_until.is_some_and(|until| until > now) => None,
            Some(_) => {
                cache.remove(digest);
                None
            }
            None => None,
        }
    }

    /// Keep Nomen's answer until `asked + ttl` (a confirmed sign-in, through an outage,
    /// until `honour_until`). A full cache drops expired answers, then refusals; a
    /// confirmed sign-in is never evicted to make room for anything.
    fn remember(
        &self,
        digest: [u8; 32],
        outcome: Result<Subject, &'static str>,
        asked: Instant,
        ttl: Duration,
        honour_until: Option<Instant>,
    ) {
        let now = Instant::now();
        let until = asked + ttl;
        if until <= now {
            return;
        }
        let mut cache = self.cache.lock().expect("nomen cache lock");
        if cache.get(&digest).is_some_and(|newer| newer.asked > asked) {
            return;
        }
        if !cache.contains_key(&digest) && cache.len() >= CACHE_MAX {
            cache.retain(|_, entry| {
                entry.until > now || entry.honour_until.is_some_and(|until| until > now)
            });
            if cache.len() >= CACHE_MAX {
                cache.retain(|_, entry| entry.outcome.is_ok());
            }
            if cache.len() >= CACHE_MAX {
                return;
            }
        }
        cache.insert(
            digest,
            Remembered {
                outcome,
                asked,
                until,
                honour_until,
            },
        );
    }
}

/// Decide from Nomen's answer alone; every refusal names its reason for the operator log.
fn assess(answer: &Introspection, accepts: &Accepts, now: u64) -> Result<Subject, &'static str> {
    if !answer.active {
        return Err(
            "Nomen reports the token inactive: expired, signed out, or for another application",
        );
    }
    if answer.iss.as_deref().map(|iss| iss.trim_end_matches('/')) != Some(&accepts.issuer) {
        return Err("the token's issuer is not this hub's Nomen");
    }
    if !answer
        .aud
        .as_ref()
        .is_some_and(|aud| aud.names_any(&accepts.audiences))
    {
        return Err("the token was issued for a Nomen project this hub does not accept");
    }
    if !answer
        .client_id
        .as_ref()
        .is_some_and(|client| accepts.clients.contains(client))
    {
        return Err("the token was issued to an application this hub does not accept");
    }
    if !answer
        .exp
        .is_some_and(|exp| exp.saturating_add(LEEWAY_SECS) > now)
    {
        return Err("the token has expired");
    }
    // Exactly as Nomen states it: grants match the account ID byte for byte, so a padded
    // or otherwise odd ID is refused here rather than normalised into someone else's.
    let sub = answer
        .sub
        .as_deref()
        .filter(|sub| valid_subject(sub).is_ok())
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
        org: answer
            .org
            .as_deref()
            .map(str::trim)
            .filter(|org| crate::auth::valid_org(org))
            .map(str::to_string),
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
