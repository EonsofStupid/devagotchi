//! The account this machine signs in with: `devagotchi login`, `logout`, `whoami` (D22).
//!
//! Sign-in is OAuth device authorization (RFC 8628) against Nomen, Shippin's identity
//! provider: the terminal shows a short code and a URL, the person opens it anywhere — a
//! phone will do — and the tokens arrive here. No browser on this machine, no local port,
//! no secret in the binary: the CLI is a public client. The tokens rest in one owner-only
//! file under the config directory (`account.json`, 0600, written whole and renamed into
//! place); nothing else reads it but this binary, and the hub never sees it — a hub checks
//! the access token the usual way (`nomen`, introspection), so a stolen file is a stolen
//! token and nothing more, and `logout` revokes it.
//!
//! `login --local` records the other choice D22 allows: this machine runs without an
//! account. Until one of the two is on file, `devagotchi start` says so once per run.
//!
//! `client` is one MCP session with a hub as the account; `front` carries that into an
//! agent's stdio session (`devagotchi mcp --hub`); the board's remote face calls through it.
//!
//! Offline grace (D22, PROPOSED): the access token is refreshed when within five minutes
//! of expiry; when Nomen cannot be reached, the account on file stays "signed in (offline)"
//! for seven days past expiry, then "expired", and nothing here deletes it — only `logout`
//! does. What a consumer does in each state is the consumer's call (board sync, packet 15).

use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::config::valid_subject;

pub const USAGE: &str = "Usage:
  devagotchi login [--issuer URL] [--client-id ID]     sign in with your Shippin account
  devagotchi login --local                              run without an account on this machine
  devagotchi logout                                     revoke the tokens and forget the account
  devagotchi whoami [--json]                            who this machine is signed in as

login shows a code and a URL; open the URL on any device, enter the code, and the sign-in
lands here (OAuth device authorization, RFC 8628). Tokens are kept in account.json under
the config directory, readable by you alone. The hosted hub (devagotchi.com, shippin.cloud)
requires an account; a local or self-hosted run does not, and --local records that choice.

Defaults: issuer https://id.shippin.cloud, client id devagotchi-cli@shippin (the native
application the operator registers in Nomen with device authorization and refresh tokens).";

pub const DEFAULT_ISSUER: &str = "https://id.shippin.cloud";
pub const DEFAULT_CLIENT_ID: &str = "devagotchi-cli@shippin";
pub const FILE_NAME: &str = "account.json";
const SCOPE: &str = "openid profile email offline_access urn:nomen:iam:user:resourceowner";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Refresh when the access token is this close to expiry.
const REFRESH_AHEAD: Duration = Duration::from_secs(5 * 60);
/// Past expiry, the account on file still counts as signed in (offline) for this long.
pub const OFFLINE_GRACE: Duration = Duration::from_secs(7 * 24 * 3600);
/// A device code the person never confirms is given up on after this long at most.
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// What is on file: an account, or the choice to run without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stored {
    Local { chosen_at: u64 },
    Account(Account),
}

/// A signed-in account. The tokens are never printed; `Debug` shows the rest.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub issuer: String,
    pub client_id: String,
    /// The Nomen account ID (`sub`), which never changes; the hub grants by it.
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
    pub signed_in_at: u64,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("subject", &self.subject)
            .field("username", &self.username)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// The account's standing at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The access token is good for a while.
    Fresh,
    /// Within `REFRESH_AHEAD` of expiry, or past it: refresh before use.
    Refresh,
    /// Past expiry and no refresh succeeded; still the account on file, for now.
    Offline { grace_ends_at: u64 },
    /// Past the grace: sign in again.
    Expired,
}

impl Account {
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// An account as a test would have it: a token that is good until `expires_at`.
    #[cfg(test)]
    pub(crate) fn for_tests(issuer: &str, subject: &str, token: &str, expires_at: u64) -> Self {
        Self {
            issuer: issuer.into(),
            client_id: DEFAULT_CLIENT_ID.into(),
            subject: subject.into(),
            username: None,
            email: None,
            access_token: token.into(),
            refresh_token: None,
            expires_at,
            signed_in_at: unix_now(),
        }
    }

    pub fn standing(&self, now: u64) -> Standing {
        if now + REFRESH_AHEAD.as_secs() < self.expires_at {
            Standing::Fresh
        } else if now < self.expires_at {
            Standing::Refresh
        } else if now < self.expires_at + OFFLINE_GRACE.as_secs() {
            Standing::Offline {
                grace_ends_at: self.expires_at + OFFLINE_GRACE.as_secs(),
            }
        } else {
            Standing::Expired
        }
    }

    /// `username` when Nomen gave one, else the account ID.
    pub fn display_name(&self) -> &str {
        self.username.as_deref().unwrap_or(&self.subject)
    }
}

/// Read what is on file; `None` when nothing is. A file this binary cannot read is an
/// error naming the file, never its contents.
pub fn read(path: &Path) -> Result<Option<Stored>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| {
            format!(
                "{} is not an account file; `devagotchi logout` removes it",
                path.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Write the file whole, owner-only, and rename it into place.
pub fn write(path: &Path, stored: &Stored) -> Result<(), String> {
    let text = serde_json::to_string_pretty(stored).map_err(|e| e.to_string())? + "\n";
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
    }
    file.write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn remove(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The endpoints of one issuer, in Nomen's layout.
#[derive(Clone)]
pub struct Issuer {
    issuer: String,
    client_id: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize, Default)]
struct OAuthError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

/// Why a sign-in or refresh did not produce an account.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The person declined, the code expired, or Nomen refused the client or the grant.
    Refused(String),
    /// Nomen could not be reached or did not answer as an issuer does.
    Unavailable(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Refused(why) => write!(f, "refused: {why}"),
            Refusal::Unavailable(why) => write!(f, "{why}"),
        }
    }
}

impl Issuer {
    pub fn new(issuer: &str, client_id: &str) -> Result<Self, String> {
        let issuer = issuer.trim_end_matches('/');
        if !(issuer.starts_with("https://") || issuer.starts_with("http://127.0.0.1")) {
            return Err("issuer must be an https:// URL".into());
        }
        if client_id.trim().is_empty() {
            return Err("client id must not be empty".into());
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            issuer: issuer.to_string(),
            client_id: client_id.to_string(),
            http,
        })
    }

    /// Sign in: show the code, wait for the person, land the tokens. `tell` gets the
    /// lines a person reads (the code and the URL) as they are known.
    pub async fn login(&self, tell: &mut dyn Write) -> Result<Account, Refusal> {
        self.login_with_scope(SCOPE, tell).await
    }

    /// `login` asking for `scope` instead of the CLI's own: `hub connect-nomen` asks for
    /// Nomen's management API too, with the same code-on-any-device flow.
    pub async fn login_with_scope(
        &self,
        scope: &str,
        tell: &mut dyn Write,
    ) -> Result<Account, Refusal> {
        let started = unix_now();
        let auth: DeviceAuthorization = self
            .post(
                "/oauth/v2/device_authorization",
                &[("client_id", self.client_id.as_str()), ("scope", scope)],
            )
            .await?;
        let _ = writeln!(
            tell,
            "To sign in, open {} and enter the code {}",
            auth.verification_uri, auth.user_code
        );
        if let Some(complete) = &auth.verification_uri_complete {
            let _ = writeln!(tell, "  (or open {complete})");
        }
        let _ = writeln!(tell, "Waiting for you to confirm there…");
        let mut interval = Duration::from_secs(auth.interval.unwrap_or(5).max(1));
        let deadline = Duration::from_secs(auth.expires_in).min(MAX_WAIT);
        loop {
            tokio::time::sleep(interval).await;
            if Duration::from_secs(unix_now().saturating_sub(started)) > deadline {
                return Err(Refusal::Refused(
                    "the code expired before it was confirmed; run `devagotchi login` again".into(),
                ));
            }
            let answer: Result<TokenAnswer, Refusal> = self
                .post(
                    "/oauth/v2/token",
                    &[
                        ("grant_type", DEVICE_GRANT),
                        ("device_code", auth.device_code.as_str()),
                        ("client_id", self.client_id.as_str()),
                    ],
                )
                .await;
            match answer {
                Ok(tokens) => return self.account_from(tokens, unix_now()).await,
                Err(Refusal::Refused(code)) if code == "authorization_pending" => {}
                Err(Refusal::Refused(code)) if code == "slow_down" => {
                    interval += Duration::from_secs(5);
                }
                Err(Refusal::Refused(code)) if code == "access_denied" => {
                    return Err(Refusal::Refused("you declined the sign-in".into()));
                }
                Err(Refusal::Refused(code)) if code == "expired_token" => {
                    return Err(Refusal::Refused(
                        "the code expired before it was confirmed; run `devagotchi login` again"
                            .into(),
                    ));
                }
                Err(other) => return Err(other),
            }
        }
    }

    /// A fresh access token from the refresh token; the account keeps its identity.
    pub async fn refresh(&self, account: &Account) -> Result<Account, Refusal> {
        let Some(refresh_token) = account.refresh_token.as_deref() else {
            return Err(Refusal::Refused(
                "no refresh token on file; run `devagotchi login` again".into(),
            ));
        };
        let tokens: TokenAnswer = self
            .post(
                "/oauth/v2/token",
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                    ("client_id", self.client_id.as_str()),
                    ("scope", SCOPE),
                ],
            )
            .await?;
        let now = unix_now();
        Ok(Account {
            access_token: tokens.access_token,
            refresh_token: tokens
                .refresh_token
                .or_else(|| account.refresh_token.clone()),
            expires_at: now + tokens.expires_in.unwrap_or(3600),
            ..account.clone()
        })
    }

    /// Tell Nomen the tokens are done with (RFC 7009); best effort, the file goes either way.
    pub async fn revoke(&self, account: &Account) -> Result<(), Refusal> {
        for token in [
            Some(account.access_token.as_str()),
            account.refresh_token.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let response = self
                .http
                .post(format!("{}/oauth/v2/revoke", self.issuer))
                .form(&[("token", token), ("client_id", self.client_id.as_str())])
                .send()
                .await
                .map_err(|error| Refusal::Unavailable(format!("Nomen revoke: {error}")))?;
            if !response.status().is_success() {
                return Err(Refusal::Unavailable(format!(
                    "Nomen revoke answered {}",
                    response.status()
                )));
            }
        }
        Ok(())
    }

    async fn account_from(&self, tokens: TokenAnswer, now: u64) -> Result<Account, Refusal> {
        let response = self
            .http
            .get(format!("{}/oidc/v1/userinfo", self.issuer))
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(|error| Refusal::Unavailable(format!("Nomen userinfo: {error}")))?;
        if !response.status().is_success() {
            return Err(Refusal::Unavailable(format!(
                "Nomen userinfo answered {}",
                response.status()
            )));
        }
        let info: UserInfo = read_json(response).await?;
        valid_subject(&info.sub)
            .map_err(|why| Refusal::Unavailable(format!("Nomen's account id {why}")))?;
        Ok(Account {
            issuer: self.issuer.clone(),
            client_id: self.client_id.clone(),
            subject: info.sub,
            username: info.preferred_username.filter(|u| !u.is_empty()),
            email: info.email.filter(|e| !e.is_empty()),
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: now + tokens.expires_in.unwrap_or(3600),
            signed_in_at: now,
        })
    }

    /// A form POST to the issuer; an OAuth error body becomes `Refused(error code)`.
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        form: &[(&str, &str)],
    ) -> Result<T, Refusal> {
        let response = self
            .http
            .post(format!("{}{path}", self.issuer))
            .form(form)
            .send()
            .await
            .map_err(|error| Refusal::Unavailable(format!("Nomen {path}: {error}")))?;
        let status = response.status();
        if status.is_success() {
            return read_json(response).await;
        }
        if status.is_client_error() {
            let error: OAuthError = read_json(response).await.unwrap_or_default();
            if error.error.is_empty() {
                return Err(Refusal::Unavailable(format!(
                    "Nomen {path} answered {status}"
                )));
            }
            return Err(Refusal::Refused(match error.error_description {
                Some(why)
                    if !matches!(
                        error.error.as_str(),
                        "authorization_pending" | "slow_down" | "access_denied" | "expired_token"
                    ) =>
                {
                    format!("{} ({why})", error.error)
                }
                _ => error.error,
            }));
        }
        Err(Refusal::Unavailable(format!(
            "Nomen {path} answered {status}"
        )))
    }
}

async fn read_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, Refusal> {
    let body = response
        .bytes()
        .await
        .map_err(|error| Refusal::Unavailable(format!("Nomen: {error}")))?;
    if body.len() > MAX_ANSWER_BYTES {
        return Err(Refusal::Unavailable("Nomen's answer is too large".into()));
    }
    serde_json::from_slice(&body)
        .map_err(|_| Refusal::Unavailable("Nomen's answer is not the JSON an issuer sends".into()))
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// `devagotchi login|logout|whoami`, with the account file's path resolved by the caller.
pub fn cli(verb: &str, args: Vec<std::ffi::OsString>, path: PathBuf) -> std::process::ExitCode {
    use std::process::ExitCode;

    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("devagotchi {verb}: runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match verb {
        "login" => runtime.block_on(login(&args, &path)),
        "logout" => runtime.block_on(logout(&args, &path)),
        "whoami" => runtime.block_on(whoami(&args, &path)),
        _ => Err((64, format!("unknown command {verb}"))),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("devagotchi {verb}: {message}");
            if code == 64 {
                eprintln!("\n{USAGE}");
            }
            ExitCode::from(code)
        }
    }
}

type Outcome = Result<(), (u8, String)>;

fn usage(message: impl Into<String>) -> (u8, String) {
    (64, message.into())
}

async fn login(args: &[String], path: &Path) -> Outcome {
    let mut issuer = DEFAULT_ISSUER.to_string();
    let mut client_id = DEFAULT_CLIENT_ID.to_string();
    let mut local = false;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--local" => local = true,
            "--issuer" => {
                issuer = iter
                    .next()
                    .ok_or_else(|| usage("--issuer needs a URL"))?
                    .clone()
            }
            "--client-id" => {
                client_id = iter
                    .next()
                    .ok_or_else(|| usage("--client-id needs a value"))?
                    .clone();
            }
            other => return Err(usage(format!("unknown option {other}"))),
        }
    }
    if local {
        if let Some(Stored::Account(account)) = read(path).map_err(|e| (1, e))? {
            return Err((
                1,
                format!(
                    "signed in as {}; `devagotchi logout` first, then `login --local`",
                    account.display_name()
                ),
            ));
        }
        write(
            path,
            &Stored::Local {
                chosen_at: unix_now(),
            },
        )
        .map_err(|e| (1, e))?;
        eprintln!(
            "devagotchi: this machine runs without an account ({}). `devagotchi login` changes that.",
            path.display()
        );
        return Ok(());
    }
    let nomen = Issuer::new(&issuer, &client_id).map_err(usage)?;
    let mut stderr = std::io::stderr();
    let account = nomen
        .login(&mut stderr)
        .await
        .map_err(|why| (1, why.to_string()))?;
    write(path, &Stored::Account(account.clone())).map_err(|e| (1, e))?;
    eprintln!(
        "devagotchi: signed in as {} at {} (account {}); tokens in {}",
        account.display_name(),
        account.issuer,
        account.subject,
        path.display()
    );
    Ok(())
}

async fn logout(args: &[String], path: &Path) -> Outcome {
    if !args.is_empty() {
        return Err(usage(format!("unknown option {}", args[0])));
    }
    match read(path).map_err(|e| (1, e))? {
        Some(Stored::Account(account)) => {
            match Issuer::new(&account.issuer, &account.client_id) {
                Ok(nomen) => match nomen.revoke(&account).await {
                    Ok(()) => eprintln!("devagotchi: tokens revoked at {}", account.issuer),
                    Err(why) => eprintln!(
                        "devagotchi: could not revoke the tokens at {} ({why}); they expire on their own",
                        account.issuer
                    ),
                },
                Err(why) => eprintln!("devagotchi: not revoking: {why}"),
            }
            remove(path).map_err(|e| (1, e))?;
            eprintln!("devagotchi: signed out ({} removed)", path.display());
        }
        Some(Stored::Local { .. }) => {
            remove(path).map_err(|e| (1, e))?;
            eprintln!("devagotchi: the local choice is forgotten; `devagotchi login` signs in");
        }
        None => eprintln!("devagotchi: not signed in"),
    }
    Ok(())
}

/// Whether an account (not `--local`, not past its offline grace) is on file.
pub fn signed_in(path: &Path) -> bool {
    matches!(read(path), Ok(Some(Stored::Account(account))) if account.standing(unix_now()) != Standing::Expired)
}

/// The account on file, refreshed when due and Nomen answers; otherwise as it stands.
/// What comes back is written to the file only when the refresh succeeded.
pub async fn current(path: &Path) -> Result<Option<Stored>, String> {
    let stored = read(path)?;
    let Some(Stored::Account(account)) = &stored else {
        return Ok(stored);
    };
    if matches!(
        account.standing(unix_now()),
        Standing::Fresh | Standing::Expired
    ) {
        return Ok(stored);
    }
    let Ok(nomen) = Issuer::new(&account.issuer, &account.client_id) else {
        return Ok(stored);
    };
    match nomen.refresh(account).await {
        Ok(refreshed) => {
            write(path, &Stored::Account(refreshed.clone()))?;
            Ok(Some(Stored::Account(refreshed)))
        }
        Err(_) => Ok(stored),
    }
}

async fn whoami(args: &[String], path: &Path) -> Outcome {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        [other, ..] => return Err(usage(format!("unknown option {other}"))),
    };
    let stored = current(path).await.map_err(|e| (1, e))?;
    let now = unix_now();
    if json {
        let value = match &stored {
            None => serde_json::json!({ "signed_in": false, "local": false }),
            Some(Stored::Local { chosen_at }) => {
                serde_json::json!({ "signed_in": false, "local": true, "chosen_at": chosen_at })
            }
            Some(Stored::Account(account)) => serde_json::json!({
                "signed_in": true,
                "local": false,
                "issuer": account.issuer,
                "subject": account.subject,
                "username": account.username,
                "email": account.email,
                "expires_at": account.expires_at,
                "standing": standing_name(account.standing(now)),
            }),
        };
        println!("{value}");
        return Ok(());
    }
    match stored {
        None => println!("not signed in (devagotchi login, or devagotchi login --local)"),
        Some(Stored::Local { .. }) => {
            println!("no account: this machine runs locally (devagotchi login signs in)")
        }
        Some(Stored::Account(account)) => {
            let standing = match account.standing(now) {
                Standing::Fresh => "signed in".to_string(),
                Standing::Refresh => "signed in (token due for refresh)".to_string(),
                Standing::Offline { grace_ends_at } => format!(
                    "signed in (offline: token expired, grace for {} more days)",
                    (grace_ends_at - now) / 86_400
                ),
                Standing::Expired => "signed in, but expired (devagotchi login again)".to_string(),
            };
            println!(
                "{standing}: {} at {} (account {})",
                account.display_name(),
                account.issuer,
                account.subject
            );
        }
    }
    Ok(())
}

fn standing_name(standing: Standing) -> &'static str {
    match standing {
        Standing::Fresh => "fresh",
        Standing::Refresh => "refresh",
        Standing::Offline { .. } => "offline",
        Standing::Expired => "expired",
    }
}

pub mod client;
pub mod front;

#[cfg(test)]
mod tests;
