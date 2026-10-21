//! Hub configuration: who may reach which broker, and how callers are authenticated.
//!
//! Unknown fields and unsafe values fail closed, like the broker's own config.

use std::{collections::BTreeSet, path::Path};

use devagotchi_contracts::{BridgeError, BridgeResult};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    /// Socket address the hub listens on. Put it behind the edge (Caddy); the hub
    /// speaks plain HTTP and trusts nothing from the network but the bearer token.
    pub listen: String,
    /// The hub's own HTTPS URL as clients see it. It is the protected resource
    /// identifier (RFC 9728) and the `resource` hosted clients send.
    pub public_url: String,
    pub auth: AuthConfig,
    /// Brokers this hub can route to. In H1 every broker is local to the hub
    /// host (a Unix socket); the mesh (M1) adds remote brokers behind the same names.
    pub brokers: Vec<BrokerEntry>,
    /// Which authenticated subject may use which brokers. No grant, no route.
    pub grants: Vec<Grant>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, tag = "mode", rename_all = "snake_case")]
pub enum AuthConfig {
    /// Bearer tokens are JWTs issued by an OpenID Connect provider (Nomen).
    /// Discovery runs at start; keys come from the provider's JWKS.
    Oidc {
        /// Issuer URL exactly as the provider states it (`iss` claim).
        issuer: String,
        /// Accepted `aud` values: the hub's own client/project ids at the provider,
        /// and the dynamic-registration project when hosted clients register there.
        audiences: Vec<String>,
    },
    /// Static tokens from a file the hub owner controls (mode 0600). For local
    /// tests and single-machine self-hosting without an identity provider.
    Dev {
        /// Absolute path to a JSON object `{ "<token>": "<subject>" }`.
        tokens_file: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerEntry {
    /// Stable name callers use in `broker` fields; `[a-z0-9][a-z0-9-]{0,62}`.
    pub name: String,
    /// Absolute path to the broker's Unix socket (the broker's ready line).
    pub socket: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// The subject as the identity provider names it (`sub`), or the dev token's subject.
    pub subject: String,
    /// Broker names this subject may route to.
    pub brokers: Vec<String>,
}

impl HubConfig {
    pub fn parse(text: &str) -> BridgeResult<Self> {
        let config: HubConfig = serde_json::from_str(text)
            .map_err(|error| BridgeError::new("invalid_config", error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> BridgeResult<()> {
        let invalid = |message: String| BridgeError::new("invalid_config", message);
        if self.listen.parse::<std::net::SocketAddr>().is_err() {
            return Err(invalid(format!(
                "listen must be host:port, got {:?}",
                self.listen
            )));
        }
        if !self.public_url.starts_with("https://") && !self.public_url.starts_with("http://") {
            return Err(invalid("public_url must be an absolute http(s) URL".into()));
        }
        match &self.auth {
            AuthConfig::Oidc { issuer, audiences } => {
                if !issuer.starts_with("https://") {
                    return Err(invalid("auth.issuer must be an https URL".into()));
                }
                if audiences.is_empty() || audiences.iter().any(|a| a.trim().is_empty()) {
                    return Err(invalid(
                        "auth.audiences must name at least one audience".into(),
                    ));
                }
            }
            AuthConfig::Dev { tokens_file } => {
                if !Path::new(tokens_file).is_absolute() {
                    return Err(invalid("auth.tokens_file must be an absolute path".into()));
                }
            }
        }
        if self.brokers.is_empty() || self.brokers.len() > 256 {
            return Err(invalid(
                "brokers must list between 1 and 256 entries".into(),
            ));
        }
        let mut names = BTreeSet::new();
        for broker in &self.brokers {
            if !valid_name(&broker.name) {
                return Err(invalid(format!(
                    "broker name {:?} is not [a-z0-9][a-z0-9-]{{0,62}}",
                    broker.name
                )));
            }
            if !names.insert(broker.name.as_str()) {
                return Err(invalid(format!(
                    "broker name {:?} is listed twice",
                    broker.name
                )));
            }
            if !Path::new(&broker.socket).is_absolute() {
                return Err(invalid(format!(
                    "broker {:?}: socket must be an absolute path",
                    broker.name
                )));
            }
        }
        if self.grants.len() > 4096 {
            return Err(invalid("at most 4096 grants".into()));
        }
        for grant in &self.grants {
            if grant.subject.trim().is_empty() || grant.subject.len() > 256 {
                return Err(invalid("grant subject must be 1..=256 characters".into()));
            }
            for name in &grant.brokers {
                if !names.contains(name.as_str()) {
                    return Err(invalid(format!(
                        "grant for {:?} names unknown broker {:?}",
                        grant.subject, name
                    )));
                }
            }
        }
        Ok(())
    }

    /// Broker names the subject may route to, in config order, without duplicates.
    pub fn brokers_for(&self, subject: &str) -> Vec<&BrokerEntry> {
        let mut allowed = BTreeSet::new();
        for grant in self.grants.iter().filter(|g| g.subject == subject) {
            allowed.extend(grant.brokers.iter().map(String::as_str));
        }
        self.brokers
            .iter()
            .filter(|b| allowed.contains(b.name.as_str()))
            .collect()
    }

    pub fn broker(&self, name: &str) -> Option<&BrokerEntry> {
        self.brokers.iter().find(|b| b.name == name)
    }
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_lowercase() | bytes[0].is_ascii_digit()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute paths in the platform's own form: `/…` is not absolute on Windows.
    #[cfg(unix)]
    const TOKENS: &str = "/tmp/tokens.json";
    #[cfg(windows)]
    const TOKENS: &str = r"C:\tmp\tokens.json";
    #[cfg(unix)]
    const SOCKET: &str = "/run/broker.sock";
    #[cfg(windows)]
    const SOCKET: &str = r"\\.\pipe\devagotchi-broker-test";

    fn good() -> serde_json::Value {
        serde_json::json!({
            "listen": "127.0.0.1:8787",
            "public_url": "https://hub.example",
            "auth": { "mode": "dev", "tokens_file": TOKENS },
            "brokers": [ { "name": "gold-ws", "socket": SOCKET } ],
            "grants": [ { "subject": "jesse", "brokers": ["gold-ws"] } ]
        })
    }

    fn parse(value: &serde_json::Value) -> BridgeResult<HubConfig> {
        HubConfig::parse(&value.to_string())
    }

    #[test]
    fn accepts_a_minimal_dev_config_and_resolves_grants() {
        let config = parse(&good()).unwrap();
        assert_eq!(config.brokers_for("jesse").len(), 1);
        assert!(config.brokers_for("someone-else").is_empty());
        assert!(config.broker("gold-ws").is_some());
    }

    #[test]
    fn unknown_fields_unknown_brokers_and_bad_names_fail_closed() {
        let mut unknown = good();
        let grants = unknown.as_object_mut().unwrap().remove("grants").unwrap();
        unknown["grant"] = grants;
        assert_eq!(parse(&unknown).unwrap_err().code, "invalid_config");

        let mut dangling = good();
        dangling["grants"][0]["brokers"] = serde_json::json!(["other"]);
        assert_eq!(parse(&dangling).unwrap_err().code, "invalid_config");

        let mut bad_name = good();
        bad_name["brokers"][0]["name"] = serde_json::json!("Gold WS");
        assert_eq!(parse(&bad_name).unwrap_err().code, "invalid_config");

        let mut relative = good();
        relative["brokers"][0]["socket"] = serde_json::json!("broker.sock");
        assert_eq!(parse(&relative).unwrap_err().code, "invalid_config");

        let mut relative_tokens = good();
        relative_tokens["auth"]["tokens_file"] = serde_json::json!("tokens.json");
        assert_eq!(parse(&relative_tokens).unwrap_err().code, "invalid_config");
    }

    #[test]
    fn oidc_requires_https_issuer_and_an_audience() {
        let mut oidc = good();
        oidc["auth"] = serde_json::json!({ "mode": "oidc", "issuer": "https://id.example", "audiences": ["hub"] });
        assert!(parse(&oidc).is_ok());

        let mut no_aud = oidc.clone();
        no_aud["auth"]["audiences"] = serde_json::json!([]);
        assert_eq!(parse(&no_aud).unwrap_err().code, "invalid_config");

        let mut plain = oidc.clone();
        plain["auth"]["issuer"] = serde_json::json!("http://id.example");
        assert_eq!(parse(&plain).unwrap_err().code, "invalid_config");
    }
}
