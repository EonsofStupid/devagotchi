//! `auth.providers` and `auth.email` in hub mode: the ways besides a passkey that people
//! sign in to the hub's own accounts, and secure one made on first connect. Each links to
//! the same account (`accounts/identities.rs`).
//!
//! ```jsonc
//! "auth": { "mode": "hub",
//!   "providers": [
//!     { "kind": "google", "client_id_env": "DEVAGOTCHI_GOOGLE_CLIENT_ID",
//!       "client_secret_env": "DEVAGOTCHI_GOOGLE_CLIENT_SECRET" },
//!     { "kind": "github", "client_id_env": "DEVAGOTCHI_GITHUB_CLIENT_ID",
//!       "client_secret_env": "DEVAGOTCHI_GITHUB_CLIENT_SECRET" },
//!     { "kind": "oidc", "name": "nomen", "label": "Shippin", "issuer": "https://id.shippin.cloud",
//!       "client_id_env": "DEVAGOTCHI_NOMEN_CLIENT_ID", "client_secret_env": "DEVAGOTCHI_NOMEN_CLIENT_SECRET" }
//!   ],
//!   "email": { "smtp_url_env": "DEVAGOTCHI_SMTP_URL", "password_env": "DEVAGOTCHI_SMTP_PASSWORD",
//!              "from": "Devagotchi <sign-in@devagotchi.com>" }
//! }
//! ```
//!
//! Client ids may sit in the file or come from the environment; secrets and the SMTP
//! password only ever come from the environment (set from the secret store) or a 0600 file.
//! Nothing here is in the code.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Google's OpenID Connect (`https://accounts.google.com`).
    Google,
    /// A GitHub OAuth app (github.com, or a GitHub Enterprise's endpoints).
    Github,
    /// Any OpenID Connect provider by its issuer: Sign in with Shippin (Nomen) is one.
    Oidc,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Its name in URLs and on linked sign-ins (`operators` say `<name>:<subject>`):
    /// `google` and `github` by default; an OpenID Connect provider names itself (`nomen`).
    #[serde(default)]
    pub name: Option<String>,
    /// What its button says after "Continue with": Google, GitHub, or the name.
    #[serde(default)]
    pub label: Option<String>,
    /// An OpenID Connect provider's issuer, exactly as it states it; Google's by default.
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    /// The environment variable holding the client id instead.
    #[serde(default)]
    pub client_id_env: Option<String>,
    /// The environment variable holding the client secret (from the secret store).
    #[serde(default)]
    pub client_secret_env: Option<String>,
    /// Or an absolute path to a 0600 file holding it.
    #[serde(default)]
    pub client_secret_file: Option<String>,
    /// Scopes to ask for; the provider's own minimum by default.
    #[serde(default)]
    pub scopes: Option<String>,
    /// GitHub only, for GitHub Enterprise (or a test's stub): its endpoints.
    #[serde(default)]
    pub authorize_url: Option<String>,
    #[serde(default)]
    pub token_url: Option<String>,
    #[serde(default)]
    pub api_url: Option<String>,
}

pub const GOOGLE_ISSUER: &str = "https://accounts.google.com";

impl ProviderConfig {
    pub fn name(&self) -> &str {
        match (&self.name, self.kind) {
            (Some(name), _) => name,
            (None, ProviderKind::Google) => "google",
            (None, ProviderKind::Github) => "github",
            (None, ProviderKind::Oidc) => "oidc",
        }
    }

    pub fn label(&self) -> String {
        match (&self.label, self.kind) {
            (Some(label), _) => label.clone(),
            (None, ProviderKind::Google) => "Google".into(),
            (None, ProviderKind::Github) => "GitHub".into(),
            (None, ProviderKind::Oidc) => self.name().to_string(),
        }
    }

    pub fn issuer(&self) -> Option<&str> {
        match (self.kind, &self.issuer) {
            (_, Some(issuer)) => Some(issuer.trim_end_matches('/')),
            (ProviderKind::Google, None) => Some(GOOGLE_ISSUER),
            _ => None,
        }
    }

    pub(crate) fn check(&self, env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
        let name = self.name();
        let what = format!("auth.providers {name}");
        if !super::valid_name(name) || name.len() > 32 {
            return Err(format!("{what}: a name is 1 to 32 of [a-z0-9-]"));
        }
        if self.kind == ProviderKind::Oidc && self.name.is_none() {
            return Err(format!(
                "{what}: an oidc provider needs a name (nomen for Shippin)"
            ));
        }
        if name == "email" {
            return Err(format!("{what}: email is the email link's own name"));
        }
        if let Some(issuer) = self.issuer()
            && !issuer.starts_with("https://")
            && !super::loopback_http(issuer)
        {
            return Err(format!(
                "{what}: issuer must be https (plain http only on loopback)"
            ));
        }
        if self.kind == ProviderKind::Oidc && self.issuer.is_none() {
            return Err(format!("{what}: issuer is required"));
        }
        if self.kind != ProviderKind::Github
            && (self.authorize_url.is_some() || self.token_url.is_some() || self.api_url.is_some())
        {
            return Err(format!(
                "{what}: authorize_url, token_url and api_url are GitHub's; an OpenID Connect provider is found from its issuer"
            ));
        }
        for url in [&self.authorize_url, &self.token_url, &self.api_url]
            .into_iter()
            .flatten()
        {
            if !url.starts_with("https://") && !super::loopback_http(url) {
                return Err(format!(
                    "{what}: {url} must be https (plain http only on loopback)"
                ));
            }
        }
        match (&self.client_id, &self.client_id_env) {
            (Some(_), Some(_)) | (None, None) => {
                return Err(format!("{what}: client_id or client_id_env, one of them"));
            }
            _ => {}
        }
        if let Some(variable) = &self.client_id_env
            && env(variable).is_none_or(|value| value.trim().is_empty())
        {
            return Err(format!("{what}: {variable} is not set"));
        }
        match (&self.client_secret_env, &self.client_secret_file) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "{what}: client_secret_env or client_secret_file, not both"
                ));
            }
            (Some(variable), None) if env(variable).is_none_or(|value| value.is_empty()) => {
                return Err(format!("{what}: {variable} is not set"));
            }
            (None, Some(path)) if !Path::new(path).is_absolute() => {
                return Err(format!(
                    "{what}: client_secret_file must be an absolute path"
                ));
            }
            // GitHub and Google keep a secret for every web app; an OpenID Connect provider
            // may have a public client (PKCE only).
            (None, None) if self.kind != ProviderKind::Oidc => {
                return Err(format!(
                    "{what}: client_secret_env (or client_secret_file) is required"
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

/// The email link's mail server (`auth.email`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailConfig {
    /// `smtps://user@host:465` (TLS from the first byte) or `smtp://user@host:587`
    /// (STARTTLS, required). Plain `smtp://` without TLS only to this machine's loopback.
    #[serde(default)]
    pub smtp_url: Option<String>,
    /// The environment variable holding that URL instead.
    #[serde(default)]
    pub smtp_url_env: Option<String>,
    /// The environment variable holding the password (from the secret store).
    #[serde(default)]
    pub password_env: Option<String>,
    /// Or an absolute path to a 0600 file holding it.
    #[serde(default)]
    pub password_file: Option<String>,
    /// The sender, `Name <address>` or an address.
    pub from: String,
    /// Links sent to one address in an hour.
    #[serde(default = "default_per_address")]
    pub per_address_per_hour: u32,
}

fn default_per_address() -> u32 {
    5
}

impl EmailConfig {
    pub(crate) fn check(&self, env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
        let url = match (&self.smtp_url, &self.smtp_url_env) {
            (Some(url), None) => url.clone(),
            (None, Some(variable)) => env(variable)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| format!("auth.email: {variable} is not set"))?,
            _ => return Err("auth.email: smtp_url or smtp_url_env, one of them".into()),
        };
        crate::mail::Server::parse(&url).map_err(|why| format!("auth.email: {why}"))?;
        if self.password_env.is_some() && self.password_file.is_some() {
            return Err("auth.email: password_env or password_file, not both".into());
        }
        if let Some(path) = &self.password_file
            && !Path::new(path).is_absolute()
        {
            return Err("auth.email: password_file must be an absolute path".into());
        }
        if crate::mail::address_of(&self.from).is_none() {
            return Err(format!("auth.email: from {:?} has no address", self.from));
        }
        if !(1..=100).contains(&self.per_address_per_hour) {
            return Err("auth.email.per_address_per_hour is 1 to 100".into());
        }
        Ok(())
    }
}
