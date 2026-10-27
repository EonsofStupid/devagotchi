//! `devagotchi hub connect-nomen`: the hub joins a Nomen with its owner's approval.
//!
//! The owner approves a device code (on any device; a phone will do) through the
//! `devagotchi-cli` app that Nomen's deploy made in the "Devagotchi" project. With that
//! approval, and nothing else, this command adds the hub's own two apps to the same project:
//! an API app whose key Nomen generates and hands back to this process only (it is written
//! here, owner-only, and never travels anywhere else), and a browser app for the hub's
//! pages. Then it points the hub's config at them and makes the person who approved the
//! hub's owner. No secret is typed, pasted or copied between machines.
//!
//! Running it again against the same project reuses the apps, adds a fresh key, and keeps
//! the clients already listed. A different person approving for the same project is
//! refused: the owner is changed only by editing the config.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use base64::Engine;
use devagotchi_contracts::{BridgeError, BridgeResult};
use serde_json::{Value, json};

use crate::{account::Issuer, config::HubConfig};

/// Nomen's management API, on top of the sign-in itself.
pub const API_SCOPE: &str =
    "openid profile email offline_access urn:zitadel:iam:org:project:id:zitadel:aud";
pub const API_APP: &str = "devagotchi-hub";
pub const WEB_APP: &str = "devagotchi-hub-web";
pub const KEY_FILE: &str = "nomen-key.json";

#[derive(Debug, PartialEq, Eq)]
pub struct Connect {
    pub state: PathBuf,
    pub config: PathBuf,
    pub issuer: String,
    pub client_id: String,
    pub project: String,
}

fn err(code: &str, message: impl Into<String>) -> BridgeError {
    BridgeError::new(code, message.into())
}

pub async fn run(connect: &Connect, tell: &mut dyn Write) -> BridgeResult<()> {
    let text = std::fs::read_to_string(&connect.config)
        .map_err(|e| err("config", format!("{}: {e}", connect.config.display())))?;
    let mut doc: Value = serde_json::from_str(&text).map_err(|e| {
        err(
            "invalid_config",
            format!("{}: {e}", connect.config.display()),
        )
    })?;
    let public_url = doc["public_url"]
        .as_str()
        .filter(|u| u.starts_with("https://") || u.starts_with("http://127.0.0.1"))
        .ok_or_else(|| err("invalid_config", "public_url must be the hub's https URL"))?
        .trim_end_matches('/')
        .to_string();
    if !valid_id(&connect.project) || !valid_id(&connect.client_id) {
        return Err(err(
            "invalid_cli",
            "--project and --client-id are the numeric ids Nomen's deploy printed",
        ));
    }
    let same_project = doc["auth"]["audiences"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(&connect.project)));

    let issuer =
        Issuer::new(&connect.issuer, &connect.client_id).map_err(|e| err("invalid_cli", e))?;
    let _ = writeln!(
        tell,
        "Approve this hub as its owner, in Nomen at {}.",
        connect.issuer
    );
    let owner = issuer
        .login_with_scope(API_SCOPE, tell)
        .await
        .map_err(|e| err("nomen", e.to_string()))?;
    if same_project {
        if let Some(current) = doc["owner"].as_str().filter(|o| *o != owner.subject) {
            return Err(err(
                "not_owner",
                format!(
                    "this hub already belongs to {current} in project {}; {} approved. The owner changes only by editing {}",
                    connect.project,
                    owner.subject,
                    connect.config.display()
                ),
            ));
        }
    }
    let _ = writeln!(
        tell,
        "Approved by {} ({}).",
        owner.display_name(),
        owner.subject
    );

    let nomen = Management::new(&connect.issuer, owner.access_token())?;
    let project = connect.project.as_str();
    let api = match nomen.find_app(project, API_APP).await? {
        Some(app) => app,
        None => {
            let created = nomen
                .post(
                    &format!("/management/v1/projects/{project}/apps/api"),
                    &json!({ "name": API_APP, "authMethodType": "API_AUTH_METHOD_TYPE_PRIVATE_KEY_JWT" }),
                )
                .await?;
            App::created(&created)?
        }
    };
    let key = nomen
        .post(
            &format!("/management/v1/projects/{project}/apps/{}/keys", api.id),
            &json!({ "type": "KEY_TYPE_JSON" }),
        )
        .await?;
    let key = base64::engine::general_purpose::STANDARD
        .decode(key["keyDetails"].as_str().unwrap_or(""))
        .map_err(|_| err("nomen", "Nomen's key is not base64"))?;
    let parsed: Value =
        serde_json::from_slice(&key).map_err(|_| err("nomen", "Nomen's key is not JSON"))?;
    if parsed["clientId"].as_str() != Some(api.client_id.as_str()) {
        return Err(err("nomen", "Nomen's key is for another app"));
    }

    let callback = format!("{public_url}/auth/callback");
    let web = match nomen.find_app(project, WEB_APP).await? {
        Some(app) if app.redirects.iter().any(|r| *r == callback) => app,
        Some(_) => {
            return Err(err(
                "nomen",
                format!(
                    "{WEB_APP} exists in project {project} without the redirect {callback}; add it in the console or delete the app"
                ),
            ));
        }
        None => {
            let created = nomen
                .post(
                    &format!("/management/v1/projects/{project}/apps/oidc"),
                    &json!({
                        "name": WEB_APP,
                        "appType": "OIDC_APP_TYPE_USER_AGENT",
                        "authMethodType": "OIDC_AUTH_METHOD_TYPE_NONE",
                        "responseTypes": ["OIDC_RESPONSE_TYPE_CODE"],
                        "grantTypes": ["OIDC_GRANT_TYPE_AUTHORIZATION_CODE", "OIDC_GRANT_TYPE_REFRESH_TOKEN"],
                        "redirectUris": [callback],
                        "accessTokenType": "OIDC_TOKEN_TYPE_BEARER",
                    }),
                )
                .await?;
            App::created(&created)?
        }
    };

    let key_path = connect.state.join(KEY_FILE);
    write_private(&key_path, &key)?;
    let mut clients: Vec<String> = if same_project {
        doc["auth"]["clients"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    for client in [&connect.client_id, &web.client_id] {
        if !clients.contains(client) {
            clients.push(client.clone());
        }
    }
    doc["auth"] = json!({
        "mode": "nomen",
        "issuer": connect.issuer.trim_end_matches('/'),
        "audiences": [project],
        "clients": clients,
        "key_file": key_path,
    });
    doc["owner"] = json!(owner.subject);
    doc["web"] = json!({ "client_id": web.client_id });
    let new_text = serde_json::to_string_pretty(&doc).map_err(|e| err("config", e.to_string()))?;
    HubConfig::parse(&new_text)?;

    let backup = connect.config.with_extension(format!(
        "json.before-connect-nomen-{}",
        crate::account::unix_now()
    ));
    write_private(&backup, text.as_bytes())?;
    write_private(&connect.config, format!("{new_text}\n").as_bytes())?;
    let stale = doc["grants"].as_array().map_or(0, Vec::len);
    let _ = writeln!(
        tell,
        "Connected: project {project}, API app {} (key in {}), web app {}; owner {}.\nThe previous config is in {}.\n{stale} grant(s) name subjects from before; people and agents sign in again and get new ones.\nRestart the hub to use it: systemctl --user restart devagotchi-hub",
        api.client_id,
        key_path.display(),
        web.client_id,
        owner.subject,
        backup.display()
    );
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
}

/// Write `bytes` to `path` readable by the owner only, replacing it whole: a temporary
/// file in the same directory, made 0600 before anything is written, then renamed.
fn write_private(path: &Path, bytes: &[u8]) -> BridgeResult<()> {
    let dir = path.parent().ok_or_else(|| err("config", "no directory"))?;
    std::fs::create_dir_all(dir).map_err(|e| err("config", e.to_string()))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .map_err(|e| err("config", format!("{}: {e}", tmp.display())))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| err("config", e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| err("config", format!("{}: {e}", path.display())))
}

/// One app in a Nomen project, as the hub needs it.
struct App {
    id: String,
    client_id: String,
    redirects: Vec<String>,
}

impl App {
    fn created(answer: &Value) -> BridgeResult<Self> {
        Ok(Self {
            id: field(answer, "appId")?,
            client_id: field(answer, "clientId")?,
            redirects: Vec::new(),
        })
    }
}

fn field(value: &Value, name: &str) -> BridgeResult<String> {
    value[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| err("nomen", format!("Nomen's answer has no {name}")))
}

/// Nomen's management API as the owner who approved.
struct Management {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Management {
    fn new(issuer: &str, token: &str) -> BridgeResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| err("nomen", e.to_string()))?;
        Ok(Self {
            base: issuer.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        })
    }

    async fn post(&self, path: &str, body: &Value) -> BridgeResult<Value> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| err("nomen", format!("Nomen {path}: {e}")))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| err("nomen", format!("Nomen {path}: {e}")))?;
        if !status.is_success() {
            let why: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let message = why["message"].as_str().unwrap_or("");
            return Err(err(
                if status.as_u16() == 403 {
                    "not_owner"
                } else {
                    "nomen"
                },
                format!("Nomen {path} answered {status} {message}"),
            ));
        }
        serde_json::from_slice(&bytes).map_err(|_| err("nomen", format!("Nomen {path}: not JSON")))
    }

    async fn find_app(&self, project: &str, name: &str) -> BridgeResult<Option<App>> {
        let found = self
            .post(
                &format!("/management/v1/projects/{project}/apps/_search"),
                &json!({ "queries": [{ "nameQuery": { "name": name, "method": "TEXT_QUERY_METHOD_EQUALS" } }] }),
            )
            .await?;
        let Some(app) = found["result"].as_array().and_then(|r| r.first()) else {
            return Ok(None);
        };
        let config = if app["apiConfig"].is_object() {
            &app["apiConfig"]
        } else {
            &app["oidcConfig"]
        };
        Ok(Some(App {
            id: field(app, "id")?,
            client_id: field(config, "clientId")?,
            redirects: config["redirectUris"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        }))
    }
}

#[cfg(test)]
#[path = "connect/tests.rs"]
mod tests;
