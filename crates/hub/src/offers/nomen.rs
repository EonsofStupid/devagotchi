//! Making or finding a person's account on Nomen, the network's identity provider, through
//! its user API (Zitadel-compatible, `nomen.user.v2.UserService` over HTTP/JSON), with a
//! service user's personal access token. Two calls, both in the configured organization:
//!
//! - `POST /v2/users` (ListUsers): the user whose email is the account's verified one.
//! - `POST /v2/users/new` (CreateUser): when there is none, a human user with that email,
//!   marked verified (the hub verified it), no password: they sign in at Nomen the ways it
//!   offers, and their "Sign in with Shippin" reaches this account from then on.
//!
//! A user found whose email Nomen has not verified is not linked: someone else may have
//! typed that address. The token is read once, at start, from the environment or a 0600
//! file (`config/shippin.rs`), and never logged.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::config::shippin::Provisioning;

const MAX_ANSWER: usize = 256 * 1024;

pub struct Nomen {
    api: String,
    organization: String,
    token: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Nomen {
    /// Never prints the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Nomen({})", self.api)
    }
}

/// What Nomen answered for an email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The user to link: found with that email verified, or made now.
    User { id: String, made: bool },
    /// A user has that email, unverified at Nomen: not linked without them.
    Unverified,
}

impl Nomen {
    /// From the config, its token read now: one that is missing stops the start.
    pub fn from_config(config: &Provisioning) -> Result<Self, String> {
        let token = match (&config.token_env, &config.token_file) {
            (Some(variable), _) => std::env::var(variable)
                .map_err(|_| format!("shippin.provisioning: {variable} is not set"))?,
            (None, Some(path)) => {
                crate::auth::read_private(path, "shippin.provisioning.token_file")
                    .map_err(|error| error.message)?
            }
            (None, None) => return Err("shippin.provisioning has no token".into()),
        };
        let token = token.trim().to_string();
        if token.is_empty() || token.contains(char::is_whitespace) {
            return Err("shippin.provisioning: the token is empty or not one token".into());
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("devagotchi-hub")
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            api: config.api.trim_end_matches('/').to_string(),
            organization: config.organization_id.clone(),
            token,
            http,
        })
    }

    /// The Nomen user with `email`, made when there is none.
    pub async fn find_or_make(&self, email: &str, name: &str) -> Result<Found, String> {
        if let Some(found) = self.find(email).await? {
            return Ok(found);
        }
        let (given, family) = names(name, email);
        let made: Made = self
            .call(
                "/v2/users/new",
                json!({
                    "organizationId": self.organization,
                    "username": email,
                    "human": {
                        "profile": { "givenName": given, "familyName": family },
                        "email": { "email": email, "isVerified": true },
                    },
                }),
            )
            .await?;
        if made.id.is_empty() {
            return Err("CreateUser answered no id".into());
        }
        Ok(Found::User {
            id: made.id,
            made: true,
        })
    }

    async fn find(&self, email: &str) -> Result<Option<Found>, String> {
        let listed: Listed = self
            .call(
                "/v2/users",
                json!({
                    "queries": [
                        { "emailQuery": {
                            "emailAddress": email,
                            "method": "TEXT_QUERY_METHOD_EQUALS_IGNORE_CASE" } },
                        { "organizationIdQuery": { "organizationId": self.organization } },
                    ],
                }),
            )
            .await?;
        let mut unverified = false;
        for user in listed.result {
            let Some(human) = user.human else { continue };
            let Some(address) = human.email else { continue };
            if !address.email.eq_ignore_ascii_case(email) || user.user_id.is_empty() {
                continue;
            }
            if address.is_verified {
                return Ok(Some(Found::User {
                    id: user.user_id,
                    made: false,
                }));
            }
            unverified = true;
        }
        Ok(unverified.then_some(Found::Unverified))
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, String> {
        let mut response = self
            .http
            .post(format!("{}{path}", self.api))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("{path}: {}", error.without_url()))?;
        let status = response.status();
        let mut answer = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("{path}: {}", error.without_url()))?
        {
            if answer.len() + chunk.len() > MAX_ANSWER {
                return Err(format!("{path}: the answer is too large"));
            }
            answer.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(format!("{path} answered {status}"));
        }
        serde_json::from_slice(&answer).map_err(|_| format!("{path}: not the JSON expected"))
    }
}

/// A given and a family name for Nomen, which needs both: the account's name split at its
/// first space, else the email's local part, used for both when there is one word.
fn names(name: &str, email: &str) -> (String, String) {
    let clip = |text: &str| text.chars().take(200).collect::<String>();
    let name = name.trim();
    let name = if name.is_empty() {
        email.split('@').next().unwrap_or(email)
    } else {
        name
    };
    match name.split_once(' ') {
        Some((given, family)) if !family.trim().is_empty() => (clip(given), clip(family.trim())),
        _ => (clip(name), clip(name)),
    }
}

#[derive(Deserialize)]
struct Made {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize)]
struct Listed {
    #[serde(default)]
    result: Vec<User>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    human: Option<Human>,
}

#[derive(Deserialize)]
struct Human {
    #[serde(default)]
    email: Option<Email>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Email {
    #[serde(default)]
    email: String,
    #[serde(default)]
    is_verified: bool,
}

#[cfg(test)]
mod tests {
    use super::names;

    #[test]
    fn nomen_gets_a_given_and_a_family_name() {
        assert_eq!(
            names("Ada Lovelace", "a@x.org"),
            ("Ada".into(), "Lovelace".into())
        );
        assert_eq!(names("ada", "a@x.org"), ("ada".into(), "ada".into()));
        assert_eq!(names("  ", "ada.l@x.org"), ("ada.l".into(), "ada.l".into()));
    }
}
