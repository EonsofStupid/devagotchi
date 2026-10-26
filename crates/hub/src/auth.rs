//! Bearer authentication for the hub: who is calling, proven by the identity
//! provider, never by a caller-supplied field.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

/// Unknown key ids trigger a JWKS refresh, but never more often than this, so a
/// stream of garbage tokens cannot turn the hub into a request generator.
const JWKS_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

use devagotchi_contracts::{BridgeError, BridgeResult};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;

use crate::{config::AuthConfig, nomen::NomenVerifier};

/// An authenticated caller. `id` is what grants are written against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub id: String,
    /// `nomen`, `oidc` or `dev`; shows in the audit row so a dev token is never mistaken
    /// for a person.
    pub via: &'static str,
    /// The login name the identity provider reported, shown so a person can tell their
    /// accounts apart. Never used for grants: two accounts can share a name.
    pub name: Option<String>,
    /// The organization (tenant) the account belongs to, as the identity provider proved
    /// it (Nomen's `urn:zitadel:iam:user:resourceowner:id`). Project boards belong to it:
    /// every member works them (B13). `None` when the token carries no org; the account's
    /// own boards are then its own.
    pub org: Option<String>,
}

/// The scopes a client asks Nomen for when it signs in to this hub: who the person is,
/// a refresh token, the org they belong to (so project boards are the org's), and the
/// hub's projects in the token's audience. Nothing broader.
pub fn sign_in_scopes(audiences: &[String]) -> String {
    let mut scope =
        String::from("openid profile offline_access urn:zitadel:iam:user:resourceowner");
    for audience in audiences {
        scope.push_str(&format!(" urn:zitadel:iam:org:project:id:{audience}:aud"));
    }
    scope
}

/// An org id as an identity provider issues them: 1 to 64 of `[A-Za-z0-9_-]`, so it is
/// safe as a directory name.
pub fn valid_org(org: &str) -> bool {
    !org.is_empty()
        && org.len() <= 64
        && org
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub enum Verifier {
    Nomen(NomenVerifier),
    Oidc(OidcVerifier),
    Dev(HashMap<String, String>),
}

impl Verifier {
    /// The verifier the config names. A Nomen verifier keeps the sign-ins Nomen confirmed
    /// in `state_dir`, so a restart during a Nomen outage signs nobody out.
    pub async fn from_config(config: &AuthConfig, state_dir: &Path) -> BridgeResult<Self> {
        match config {
            AuthConfig::Nomen {
                issuer,
                audiences,
                clients,
                key_file,
            } => Ok(Verifier::Nomen(
                NomenVerifier::new(issuer, audiences, clients, key_file)?
                    .remembering(&state_dir.join("signins.sqlite3")),
            )),
            AuthConfig::Oidc { issuer, audiences } => Ok(Verifier::Oidc(
                OidcVerifier::discover(issuer, audiences).await?,
            )),
            AuthConfig::Dev { tokens_file } => Ok(Verifier::Dev(read_dev_tokens(tokens_file)?)),
        }
    }

    pub async fn verify(&self, bearer: &str) -> Result<Subject, AuthError> {
        if bearer.is_empty() || bearer.len() > 16 * 1024 {
            return Err(AuthError::Invalid("empty or oversized token"));
        }
        match self {
            // A dev token's subject may name its org after an `@` (tests only).
            Verifier::Dev(tokens) => tokens
                .get(bearer)
                .map(|subject| {
                    let (id, org) = match subject.split_once('@') {
                        Some((id, org)) if valid_org(org) => (id, Some(org.to_string())),
                        _ => (subject.as_str(), None),
                    };
                    Subject {
                        id: id.to_string(),
                        via: "dev",
                        name: None,
                        org,
                    }
                })
                .ok_or(AuthError::Invalid("unknown token")),
            Verifier::Nomen(nomen) => nomen.verify(bearer).await,
            Verifier::Oidc(oidc) => oidc.verify(bearer).await,
        }
    }

    /// Whether the identity provider is answering (Nomen only; the others have no outage
    /// state to report).
    pub fn issuer_health(&self) -> Option<crate::nomen::IssuerHealth> {
        match self {
            Verifier::Nomen(nomen) => Some(nomen.health()),
            _ => None,
        }
    }

    pub fn issuer(&self) -> Option<&str> {
        match self {
            Verifier::Nomen(nomen) => Some(nomen.issuer()),
            Verifier::Oidc(oidc) => Some(&oidc.issuer),
            Verifier::Dev(_) => None,
        }
    }
}

#[derive(Debug)]
pub enum AuthError {
    /// The token is missing, malformed, expired, for another audience, or unsigned by the issuer.
    Invalid(&'static str),
    /// The provider could not be reached to refresh keys; the caller may retry.
    Unavailable(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Invalid(why) => write!(f, "invalid_token: {why}"),
            AuthError::Unavailable(why) => write!(f, "issuer_unavailable: {why}"),
        }
    }
}

/// Read a file that holds a credential, refusing it when others could read it too.
/// Errors name the setting, never the contents.
pub(crate) fn read_private(path: &str, setting: &str) -> BridgeResult<String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| BridgeError::new("invalid_config", format!("{setting}: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(BridgeError::new(
                "invalid_config",
                format!("{setting} must be readable by its owner only (chmod 0600)"),
            ));
        }
    }
    let _ = metadata;
    std::fs::read_to_string(path)
        .map_err(|error| BridgeError::new("invalid_config", format!("{setting}: {error}")))
}

fn read_dev_tokens(path: &str) -> BridgeResult<HashMap<String, String>> {
    let text = read_private(path, "tokens_file")?;
    let tokens: HashMap<String, String> = serde_json::from_str(&text)
        .map_err(|error| BridgeError::new("invalid_config", format!("tokens_file: {error}")))?;
    if tokens.is_empty()
        || tokens.keys().any(|t| t.len() < 16)
        || tokens.values().any(|s| s.trim().is_empty())
    {
        return Err(BridgeError::new(
            "invalid_config",
            "tokens_file needs at least one token of 16+ characters mapped to a non-empty subject",
        ));
    }
    Ok(tokens)
}

#[derive(Debug, Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    #[serde(default, rename = "urn:zitadel:iam:user:resourceowner:id")]
    org: Option<String>,
}

pub struct OidcVerifier {
    issuer: String,
    audiences: Vec<String>,
    jwks_uri: String,
    http: reqwest::Client,
    keys: RwLock<Arc<JwkSet>>,
    last_refresh: Mutex<Instant>,
}

impl OidcVerifier {
    async fn discover(issuer: &str, audiences: &[String]) -> BridgeResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| BridgeError::new("auth_setup", error.to_string()))?;
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let discovery: Discovery = http
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| BridgeError::new("auth_setup", format!("discovery {url}: {error}")))?
            .json()
            .await
            .map_err(|error| BridgeError::new("auth_setup", format!("discovery {url}: {error}")))?;
        if discovery.issuer.trim_end_matches('/') != issuer.trim_end_matches('/') {
            return Err(BridgeError::new(
                "auth_setup",
                format!(
                    "issuer mismatch: configured {issuer}, discovery says {}",
                    discovery.issuer
                ),
            ));
        }
        if !discovery.jwks_uri.starts_with("https://") {
            return Err(BridgeError::new("auth_setup", "jwks_uri must be https"));
        }
        let keys = fetch_jwks(&http, &discovery.jwks_uri)
            .await
            .map_err(|error| match error {
                AuthError::Unavailable(why) => BridgeError::new("auth_setup", why),
                AuthError::Invalid(why) => BridgeError::new("auth_setup", why),
            })?;
        Ok(Self {
            issuer: discovery.issuer,
            audiences: audiences.to_vec(),
            jwks_uri: discovery.jwks_uri,
            http,
            keys: RwLock::new(Arc::new(keys)),
            last_refresh: Mutex::new(Instant::now()),
        })
    }

    async fn verify(&self, token: &str) -> Result<Subject, AuthError> {
        let header = decode_header(token).map_err(|_| AuthError::Invalid("not a JWT"))?;
        let kid = header.kid.ok_or(AuthError::Invalid("token has no kid"))?;
        let algorithm = match header.alg {
            Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384 => header.alg,
            _ => return Err(AuthError::Invalid("unsupported signing algorithm")),
        };
        let key = match self.key(&kid) {
            Some(key) => key,
            None => {
                // An unknown kid is what key rotation looks like: refresh once, then decide.
                if !self.may_refresh() {
                    return Err(AuthError::Invalid("unknown signing key"));
                }
                let fresh = fetch_jwks(&self.http, &self.jwks_uri).await?;
                *self.keys.write().expect("jwks lock") = Arc::new(fresh);
                self.key(&kid)
                    .ok_or(AuthError::Invalid("unknown signing key"))?
            }
        };
        let mut validation = Validation::new(algorithm);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_audience(&self.audiences);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = 30;
        let data = decode::<Claims>(token, &key, &validation).map_err(|_| {
            AuthError::Invalid("signature, issuer, audience or expiry check failed")
        })?;
        if data.claims.sub.trim().is_empty() || data.claims.sub.len() > 256 {
            return Err(AuthError::Invalid("subject claim out of range"));
        }
        Ok(Subject {
            id: data.claims.sub,
            via: "oidc",
            name: None,
            org: data.claims.org.filter(|org| valid_org(org)),
        })
    }

    fn may_refresh(&self) -> bool {
        let mut last = self.last_refresh.lock().expect("refresh lock");
        if last.elapsed() < JWKS_REFRESH_INTERVAL {
            return false;
        }
        *last = Instant::now();
        true
    }

    fn key(&self, kid: &str) -> Option<DecodingKey> {
        let keys = self.keys.read().expect("jwks lock").clone();
        keys.find(kid)
            .and_then(|jwk| DecodingKey::from_jwk(jwk).ok())
    }
}

async fn fetch_jwks(http: &reqwest::Client, url: &str) -> Result<JwkSet, AuthError> {
    let set: JwkSet = http
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| AuthError::Unavailable(format!("jwks {url}: {error}")))?
        .json()
        .await
        .map_err(|error| AuthError::Unavailable(format!("jwks {url}: {error}")))?;
    if set.keys.is_empty() {
        return Err(AuthError::Unavailable(format!("jwks {url}: no keys")));
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dev_tokens_require_private_file_and_match_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, r#"{"0123456789abcdef-token": "jesse"}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                read_dev_tokens(path.to_str().unwrap()).unwrap_err().code,
                "invalid_config"
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let verifier = Verifier::Dev(read_dev_tokens(path.to_str().unwrap()).unwrap());
        let subject = verifier.verify("0123456789abcdef-token").await.unwrap();
        assert_eq!(
            subject,
            Subject {
                id: "jesse".into(),
                via: "dev",
                name: None,
                org: None,
            }
        );
        assert!(matches!(
            verifier.verify("0123456789abcdef-tokeN").await,
            Err(AuthError::Invalid(_))
        ));
        let with_org = Verifier::Dev(HashMap::from([
            ("org-token-0123456789".to_string(), "alice@acme".to_string()),
            ("bad-org-token-01234".to_string(), "bob@../etc".to_string()),
        ]));
        let alice = with_org.verify("org-token-0123456789").await.unwrap();
        assert_eq!(
            (alice.id.as_str(), alice.org.as_deref()),
            ("alice", Some("acme"))
        );
        let bob = with_org.verify("bad-org-token-01234").await.unwrap();
        assert_eq!(
            (bob.id.as_str(), bob.org),
            ("bob@../etc", None),
            "an org that is not a safe name is no org"
        );
        assert!(matches!(
            verifier.verify("").await,
            Err(AuthError::Invalid(_))
        ));
    }

    /// A real RS256 token through the OIDC path. Before jsonwebtoken had a signature
    /// backend this panicked on the first token; a test that only read config could not see it.
    #[tokio::test]
    async fn oidc_verifies_a_real_signed_token() {
        use jsonwebtoken::{EncodingKey, Header, encode};
        let key = crate::test_keys::rsa();
        let keys: JwkSet = serde_json::from_value(serde_json::json!({
            "keys": [{"kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig", "n": key.n, "e": key.e}]
        }))
        .unwrap();
        let verifier = OidcVerifier {
            issuer: "https://id.example".into(),
            audiences: vec!["hub".into()],
            jwks_uri: "https://id.example/keys".into(),
            http: reqwest::Client::new(),
            keys: RwLock::new(Arc::new(keys)),
            last_refresh: Mutex::new(Instant::now()),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let sign = |aud: &str| {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some("k1".into());
            let claims = serde_json::json!({"iss": "https://id.example", "aud": aud,
                "sub": "300000000000000001", "exp": now + 600});
            encode(
                &header,
                &claims,
                &EncodingKey::from_rsa_pem(key.pem.as_bytes()).unwrap(),
            )
            .unwrap()
        };
        let subject = verifier.verify(&sign("hub")).await.unwrap();
        assert_eq!(subject.id, "300000000000000001");
        assert_eq!(subject.via, "oidc");
        assert!(matches!(
            verifier.verify(&sign("someone-else")).await,
            Err(AuthError::Invalid(_))
        ));
    }

    #[test]
    fn short_tokens_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, r#"{"short": "jesse"}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            read_dev_tokens(path.to_str().unwrap()).unwrap_err().code,
            "invalid_config"
        );
    }
}
