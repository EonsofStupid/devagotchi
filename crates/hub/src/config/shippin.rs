//! `shippin` in the hub's config: how an account whose person said yes to the site's offers
//! (`offers_accept`, `src/offers.rs`) is linked to their account on the network.
//!
//! ```jsonc
//! "shippin": {
//!   "provider": "nomen",          // the sign-in (auth.providers) whose accounts are the
//!                                 // network's: Sign in with Shippin. The default.
//!   "provisioning": {             // optional: link with no click at all
//!     "api": "https://id.shippin.cloud",     // Nomen's API (Zitadel-compatible user v2)
//!     "organization_id": "…",                // the Nomen organization new users go in
//!     "token_env": "DEVAGOTCHI_SHIPPIN_TOKEN" // or "token_file": "/run/secrets/…" (0600)
//!   }
//! }
//! ```
//!
//! With `provisioning`, an account with a verified email is linked to the Nomen user with
//! that email (made when there is none) at once. Without it, or for an account with no
//! verified email, the person gets one link that signs in with `provider` and links it.
//! The token (a Nomen service user's personal access token that may manage users in that
//! organization) comes only from the environment, set from the secret store (Vaultix), or
//! from a 0600 file its agent writes; never from this file.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShippinConfig {
    #[serde(default = "nomen")]
    pub provider: String,
    #[serde(default)]
    pub provisioning: Option<Provisioning>,
}

impl Default for ShippinConfig {
    fn default() -> Self {
        Self {
            provider: nomen(),
            provisioning: None,
        }
    }
}

fn nomen() -> String {
    "nomen".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provisioning {
    /// Nomen's API origin: `https://…` (or loopback `http://` for a test).
    pub api: String,
    pub organization_id: String,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default)]
    pub token_file: Option<String>,
}

impl ShippinConfig {
    pub(crate) fn check(&self) -> Result<(), String> {
        let key = |text: &str| {
            !text.is_empty()
                && text.len() <= 64
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        if !key(&self.provider) {
            return Err("shippin.provider is a sign-in provider's name, [A-Za-z0-9_-]".into());
        }
        let Some(provisioning) = &self.provisioning else {
            return Ok(());
        };
        let api = provisioning.api.trim_end_matches('/');
        let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|prefix| api.starts_with(prefix));
        if !(api.starts_with("https://") || loopback) || api.contains(char::is_whitespace) {
            return Err("shippin.provisioning.api is an https:// URL".into());
        }
        if !key(&provisioning.organization_id) {
            return Err("shippin.provisioning.organization_id is a Nomen organization id".into());
        }
        match (&provisioning.token_env, &provisioning.token_file) {
            (Some(_), Some(_)) => {
                Err("shippin.provisioning: token_env or token_file, not both".into())
            }
            (None, None) => Err(
                "shippin.provisioning needs token_env or token_file (the token is never in this file)"
                    .into(),
            ),
            (None, Some(file)) if !std::path::Path::new(file).is_absolute() => {
                Err("shippin.provisioning.token_file must be an absolute path".into())
            }
            (Some(variable), None) if !key(variable) => {
                Err("shippin.provisioning.token_env names an environment variable".into())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ShippinConfig, String> {
        let config: ShippinConfig = serde_json::from_str(text).map_err(|e| e.to_string())?;
        config.check().map(|()| config)
    }

    #[test]
    fn nomen_by_default_provisioning_optional_and_its_token_never_in_the_file() {
        let plain = parse("{}").unwrap();
        assert_eq!(plain.provider, "nomen");
        assert!(plain.provisioning.is_none());
        let full = parse(
            r#"{"provisioning": {"api": "https://id.example.org", "organization_id": "3001",
                "token_env": "SHIPPIN_TOKEN"}}"#,
        )
        .unwrap();
        assert_eq!(full.provisioning.unwrap().organization_id, "3001");
        for refused in [
            r#"{"provisioning": {"api": "https://id.example.org", "organization_id": "3001"}}"#,
            r#"{"provisioning": {"api": "https://id.example.org", "organization_id": "3001", "token": "secret"}}"#,
            r#"{"provisioning": {"api": "http://id.example.org", "organization_id": "3001", "token_env": "T"}}"#,
            r#"{"provisioning": {"api": "https://id.example.org", "organization_id": "3001", "token_env": "T", "token_file": "/t"}}"#,
            r#"{"provisioning": {"api": "https://id.example.org", "organization_id": "3001", "token_file": "t"}}"#,
            r#"{"provisioning": {"api": "https://id.example.org", "organization_id": "", "token_env": "T"}}"#,
            r#"{"provider": "no spaces"}"#,
        ] {
            assert!(parse(refused).is_err(), "{refused}");
        }
        // A token file at an absolute path on this OS (a drive letter on Windows).
        let file = std::env::temp_dir().join("shippin-token");
        let loopback = serde_json::json!({ "provisioning": {
            "api": "http://127.0.0.1:9", "organization_id": "1",
            "token_file": file.to_string_lossy() } });
        assert!(parse(&loopback.to_string()).is_ok());
    }
}
