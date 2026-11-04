//! Sign in with Google, GitHub or an OpenID Connect provider (Nomen is one), on a hub with
//! its own accounts: the OAuth 2.0 authorization code flow with PKCE
//! (RFC 7636), the hub a confidential client of the provider. What the provider proves
//! becomes a linked sign-in of an account (`accounts/identities.rs`), so every way in
//! reaches the same account.
//!
//! - `GET /auth/provider/<name>` sends the browser to the provider. What the callback must
//!   see again (`state`, the PKCE verifier, the OpenID nonce, where to return, whether this
//!   links to the signed-in account) rides in a short-lived `__Host-` cookie under the
//!   store's HMAC: nothing is stored until the sign-in comes back, and any hub instance can
//!   finish it.
//! - `GET /auth/provider/<name>/callback` checks that cookie and `state`, trades the code
//!   for the provider's tokens (never followed through a redirect), and reads who it is: the
//!   ID token's `sub`, `email` and `email_verified` (checked: `iss`, `aud`, `exp`, `nonce`;
//!   received straight from the token endpoint over TLS, so its signature may be left to TLS,
//!   OpenID Connect Core §3.1.3.7), or GitHub's user id and its primary verified email.
//! - Then: linking (from the account page) adds it to the signed-in account; otherwise it
//!   signs in to the account it reaches, links to a signed-in account nobody secured yet,
//!   or (on a hosted hub) makes one.

use std::{sync::Arc, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{HubSignIn, cookie_value_up_to, encode, pages, routes::see_other, safe_return_to};
use crate::{
    AppState,
    accounts::identities::Proven,
    config::{HubConfig, ProviderConfig, ProviderKind},
    devices::now_secs,
};

/// How long a started sign-in waits for the provider to send the browser back.
const SIGN_IN_SECS: u64 = 600;
const MAX_ANSWER: usize = 256 * 1024;
const GITHUB: (&str, &str, &str) = (
    "https://github.com/login/oauth/authorize",
    "https://github.com/login/oauth/access_token",
    "https://api.github.com",
);

/// One provider as the hub uses it: its config, its client id and secret as read from the
/// environment or the secret file, and its endpoints once discovered.
pub struct Provider {
    pub config: ProviderConfig,
    client_id: String,
    secret: Option<String>,
    endpoints: tokio::sync::OnceCell<Endpoints>,
}

#[derive(Debug, Clone)]
struct Endpoints {
    authorize: String,
    token: String,
    /// Client authentication at the token endpoint: HTTP Basic, unless only the form's is
    /// offered.
    basic: bool,
}

/// Every sign-in provider the config names, and the email link if it has a mail server.
pub struct Providers {
    pub list: Vec<Provider>,
    pub email: Option<super::email::Mailer>,
    http: reqwest::Client,
}

impl std::fmt::Debug for Provider {
    /// Never prints the secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Provider({})", self.config.name())
    }
}

impl Providers {
    /// No providers and no email link: passkeys only.
    pub fn none() -> Self {
        Self {
            list: Vec::new(),
            email: None,
            http: reqwest::Client::new(),
        }
    }

    /// The config's providers with their secrets read now: from the environment, or a 0600
    /// file. A secret that cannot be read stops the hub at start, not a sign-in later.
    pub fn from_config(config: &HubConfig) -> Result<Self, String> {
        let (providers, email) = config.sign_ins();
        let mut list = Vec::new();
        for provider in providers {
            let what = format!("auth.providers {}", provider.name());
            let client_id = match (&provider.client_id, &provider.client_id_env) {
                (Some(id), _) => id.clone(),
                (None, Some(variable)) => {
                    std::env::var(variable).map_err(|_| format!("{what}: {variable} is not set"))?
                }
                (None, None) => return Err(format!("{what}: no client id")),
            };
            let secret = match (&provider.client_secret_env, &provider.client_secret_file) {
                (Some(variable), _) => Some(
                    std::env::var(variable)
                        .map_err(|_| format!("{what}: {variable} is not set"))?,
                ),
                (None, Some(path)) => Some(
                    crate::auth::read_private(path, &format!("{what} client_secret_file"))
                        .map_err(|error| error.message)?
                        .trim()
                        .to_string(),
                ),
                (None, None) => None,
            };
            list.push(Provider {
                config: provider.clone(),
                client_id: client_id.trim().to_string(),
                secret,
                endpoints: tokio::sync::OnceCell::new(),
            });
        }
        let email = email.map(super::email::Mailer::from_config).transpose()?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("devagotchi-hub")
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { list, email, http })
    }

    pub fn get(&self, name: &str) -> Option<&Provider> {
        self.list.iter().find(|p| p.config.name() == name)
    }

    /// The buttons a sign-in page offers: each provider's path and label, in the site's
    /// order (`sign_in.order`) and with its label (`sign_in.labels`) over the config's.
    pub fn buttons(&self, site: &crate::site::Site, return_to: &str, link: bool) -> String {
        let mut ordered: Vec<&Provider> = self.list.iter().collect();
        ordered.sort_by_key(|provider| site.sign_in_rank(provider.config.name()));
        ordered
            .into_iter()
            .map(|provider| {
                format!(
                    "<a class=\"button\" href=\"/auth/provider/{}?return_to={}{}\">Continue with {}</a> ",
                    pages::escape(provider.config.name()),
                    pages::escape(&encode(return_to)),
                    if link { "&amp;link=1" } else { "" },
                    pages::escape(site.label(provider.config.name(), &provider.config.label()))
                )
            })
            .collect()
    }

    async fn endpoints(&self, provider: &Provider) -> Result<Endpoints, String> {
        provider
            .endpoints
            .get_or_try_init(|| async {
                match provider.config.kind {
                    ProviderKind::Github => Ok(Endpoints {
                        authorize: provider
                            .config
                            .authorize_url
                            .clone()
                            .unwrap_or_else(|| GITHUB.0.into()),
                        token: provider
                            .config
                            .token_url
                            .clone()
                            .unwrap_or_else(|| GITHUB.1.into()),
                        basic: false,
                    }),
                    ProviderKind::Google | ProviderKind::Oidc => {
                        let issuer = provider.config.issuer().unwrap_or_default();
                        #[derive(Deserialize)]
                        struct Discovery {
                            issuer: String,
                            authorization_endpoint: String,
                            token_endpoint: String,
                            #[serde(default)]
                            token_endpoint_auth_methods_supported: Option<Vec<String>>,
                        }
                        let found: Discovery = self
                            .json(
                                self.http
                                    .get(format!("{issuer}/.well-known/openid-configuration")),
                            )
                            .await?;
                        if found.issuer.trim_end_matches('/') != issuer {
                            return Err(format!("{issuer} names itself {}", found.issuer));
                        }
                        let basic =
                            found
                                .token_endpoint_auth_methods_supported
                                .is_none_or(|methods| {
                                    methods.iter().any(|m| m == "client_secret_basic")
                                });
                        Ok(Endpoints {
                            authorize: found.authorization_endpoint,
                            token: found.token_endpoint,
                            basic,
                        })
                    }
                }
            })
            .await
            .cloned()
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, String> {
        let mut response = request
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?;
        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| error.without_url().to_string())?
        {
            if body.len() + chunk.len() > MAX_ANSWER {
                return Err("the answer is too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(format!("answered {status}"));
        }
        serde_json::from_slice(&body).map_err(|_| "the answer is not the JSON expected".to_string())
    }
}

/// What the callback must see again, sealed in the sign-in cookie.
#[derive(Serialize, Deserialize)]
struct Started {
    /// The provider's name.
    p: String,
    /// `state`.
    s: String,
    /// The PKCE verifier.
    v: String,
    /// The OpenID nonce.
    n: String,
    /// Where to go after.
    r: String,
    /// Link to the signed-in account rather than sign in.
    l: bool,
    /// When it started.
    t: u64,
}

fn random() -> String {
    URL_SAFE_NO_PAD.encode(crate::link::random::<32>())
}

fn signin_cookie(hub: &HubSignIn) -> &'static str {
    if hub.secure() {
        "__Host-dg_signin"
    } else {
        "dg_signin"
    }
}

fn seal(hub: &HubSignIn, started: &Started) -> String {
    let json = serde_json::to_vec(started).unwrap_or_default();
    let tag = hub.accounts().mac("provider sign-in", &json);
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&json),
        URL_SAFE_NO_PAD.encode(tag)
    )
}

fn unseal(hub: &HubSignIn, cookie: &str) -> Option<Started> {
    let (json, tag) = cookie.split_once('.')?;
    let json = URL_SAFE_NO_PAD.decode(json).ok()?;
    let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
    if !hub.accounts().mac_ok("provider sign-in", &json, &tag) {
        return None;
    }
    let started: Started = serde_json::from_slice(&json).ok()?;
    (now_secs() < started.t.saturating_add(SIGN_IN_SECS)).then_some(started)
}

fn failed(status: StatusCode, title: &str, text: &str) -> Response {
    pages::message(
        status,
        title,
        text,
        Some(("/auth/login", "Sign in another way")),
    )
}

#[derive(Deserialize)]
pub struct StartQuery {
    return_to: Option<String>,
    link: Option<String>,
}

/// `GET /auth/provider/<name>`: off to the provider.
pub async fn start(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<StartQuery>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    let Some(provider) = hub.providers.get(&name) else {
        return failed(
            StatusCode::NOT_FOUND,
            "No such sign-in",
            "This hub does not sign in with that.",
        );
    };
    let endpoints = match hub.providers.endpoints(provider).await {
        Ok(endpoints) => endpoints,
        Err(why) => {
            eprintln!("devagotchi-hub: sign-in provider {name}: {why}");
            return failed(
                StatusCode::SERVICE_UNAVAILABLE,
                "That sign-in is unavailable",
                "The hub could not reach it just now. Try again in a minute, or sign in another way.",
            );
        }
    };
    let started = Started {
        p: name.clone(),
        s: random(),
        v: random(),
        n: random(),
        r: safe_return_to(query.return_to.as_deref().unwrap_or("/account")).to_string(),
        l: query.link.as_deref() == Some("1"),
        t: now_secs(),
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(started.v.as_bytes()));
    let scopes = provider
        .config
        .scopes
        .clone()
        .unwrap_or_else(|| match provider.config.kind {
            ProviderKind::Github => "read:user user:email".into(),
            _ => "openid email profile".into(),
        });
    let mut url = format!(
        "{}{}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={challenge}&code_challenge_method=S256",
        endpoints.authorize,
        if endpoints.authorize.contains('?') {
            '&'
        } else {
            '?'
        },
        encode(&provider.client_id),
        encode(&callback_uri(&hub, &name)),
        encode(&scopes),
        started.s,
    );
    if provider.config.kind != ProviderKind::Github {
        url.push_str(&format!("&nonce={}", started.n));
    }
    let cookie = hub.cookie(signin_cookie(&hub), &seal(&hub, &started), SIGN_IN_SECS);
    see_other(&url, &[cookie])
}

fn callback_uri(hub: &HubSignIn, name: &str) -> String {
    format!("{}/auth/provider/{name}/callback", hub.public_url())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// `GET /auth/provider/<name>/callback`: who the provider says it is, and on into the
/// account that reaches.
pub async fn callback(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    let Some(provider) = hub.providers.get(&name) else {
        return failed(
            StatusCode::NOT_FOUND,
            "No such sign-in",
            "This hub does not sign in with that.",
        );
    };
    let stale = || {
        failed(
            StatusCode::BAD_REQUEST,
            "This sign-in expired",
            "It expired, was already used, or was started in another browser. Start again.",
        )
    };
    if query.error.is_some() {
        return failed(
            StatusCode::BAD_REQUEST,
            "Sign-in did not finish",
            "The provider ended the sign-in.",
        );
    }
    let Some(started) =
        cookie_value_up_to(&headers, signin_cookie(&hub), 2048).and_then(|c| unseal(&hub, &c))
    else {
        return stale();
    };
    let (Some(code), Some(returned)) = (query.code, query.state) else {
        return stale();
    };
    if started.p != name || started.s != returned {
        return stale();
    }
    let proven = match prove(&hub, provider, &code, &started).await {
        Ok(proven) => proven,
        Err(why) => {
            eprintln!("devagotchi-hub: sign-in with {name} refused: {why}");
            return failed(
                StatusCode::BAD_REQUEST,
                "That sign-in was not accepted",
                "The provider's answer did not check out. Start again.",
            );
        }
    };
    // A sign-in (not a link from the account page) returns as a fresh one (`sign_in.rs`).
    let fresh: Vec<String> = (!started.l)
        .then(|| super::sign_in::fresh_cookie(&hub, &started.r))
        .into_iter()
        .collect();
    let mut response = finish(&hub, &headers, &proven, started.l, &started.r, &fresh).await;
    if let Ok(value) = axum::http::HeaderValue::from_str(&hub.cookie(signin_cookie(&hub), "", 0)) {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, value);
    }
    response
}

/// Trade the code for the provider's tokens and read who it proves.
async fn prove(
    hub: &HubSignIn,
    provider: &Provider,
    code: &str,
    started: &Started,
) -> Result<Proven, String> {
    let providers = &hub.providers;
    let endpoints = providers.endpoints(provider).await?;
    let redirect = callback_uri(hub, provider.config.name());
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", started.v.as_str()),
        ("client_id", provider.client_id.as_str()),
    ];
    let mut request = providers
        .http
        .post(&endpoints.token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(secret) = &provider.secret {
        if endpoints.basic {
            request = request.basic_auth(encode(&provider.client_id), Some(encode(secret)));
        } else {
            form.push(("client_secret", secret.as_str()));
        }
    }
    #[derive(Deserialize)]
    struct Tokens {
        #[serde(default)]
        access_token: Option<String>,
        #[serde(default)]
        id_token: Option<String>,
    }
    let tokens: Tokens = providers
        .json(request.form(&form))
        .await
        .map_err(|why| format!("token endpoint {why}"))?;
    match provider.config.kind {
        ProviderKind::Github => {
            let access = tokens.access_token.ok_or("no access token")?;
            github_user(providers, provider, &access).await
        }
        ProviderKind::Google | ProviderKind::Oidc => {
            let id_token = tokens.id_token.ok_or("no ID token")?;
            id_token_claims(provider, &id_token, &started.n)
        }
    }
}

/// The ID token's claims, checked: from this issuer, for this client, unexpired, for this
/// sign-in's nonce.
fn id_token_claims(provider: &Provider, id_token: &str, nonce: &str) -> Result<Proven, String> {
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or("the ID token is not a JWT")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| "the ID token's payload is not base64url")?;
    let claims: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "the ID token's payload is not JSON")?;
    let issuer = provider.config.issuer().unwrap_or_default();
    if claims["iss"].as_str().map(|iss| iss.trim_end_matches('/')) != Some(issuer) {
        return Err("the ID token is from another issuer".into());
    }
    let audience_ok = match &claims["aud"] {
        serde_json::Value::String(aud) => *aud == provider.client_id,
        serde_json::Value::Array(auds) => {
            auds.iter().any(|a| a.as_str() == Some(&provider.client_id))
        }
        _ => false,
    };
    if !audience_ok {
        return Err("the ID token is for another client".into());
    }
    if claims["exp"]
        .as_u64()
        .is_none_or(|exp| exp + 60 < now_secs())
    {
        return Err("the ID token has expired".into());
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err("the ID token is for another sign-in".into());
    }
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("the ID token names nobody")?;
    let verified = matches!(&claims["email_verified"], serde_json::Value::Bool(true))
        || claims["email_verified"].as_str() == Some("true");
    Ok(Proven {
        provider: provider.config.name().to_string(),
        subject: subject.to_string(),
        verified_email: claims["email"]
            .as_str()
            .filter(|_| verified)
            .map(str::to_string),
        name: claims["name"].as_str().map(str::to_string),
    })
}

/// GitHub's user id, and its primary email when GitHub verified it.
async fn github_user(
    providers: &Providers,
    provider: &Provider,
    access: &str,
) -> Result<Proven, String> {
    let api = provider
        .config
        .api_url
        .clone()
        .unwrap_or_else(|| GITHUB.2.into());
    let api = api.trim_end_matches('/');
    let get = |path: &str| {
        providers
            .http
            .get(format!("{api}{path}"))
            .bearer_auth(access)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
    };
    #[derive(Deserialize)]
    struct User {
        id: u64,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        login: Option<String>,
    }
    #[derive(Deserialize)]
    struct Email {
        email: String,
        #[serde(default)]
        primary: bool,
        #[serde(default)]
        verified: bool,
    }
    let user: User = providers
        .json(get("/user"))
        .await
        .map_err(|why| format!("GitHub user {why}"))?;
    let emails: Vec<Email> = providers
        .json(get("/user/emails"))
        .await
        .unwrap_or_default();
    Ok(Proven {
        provider: provider.config.name().to_string(),
        subject: user.id.to_string(),
        verified_email: emails
            .into_iter()
            .find(|e| e.primary && e.verified)
            .map(|e| e.email),
        name: user.name.or(user.login),
    })
}

/// Into the account a proven sign-in reaches: linked to the signed-in account when that was
/// asked (or when nobody secured it yet), else signed in. Shared with the email link.
/// `extra_cookies` go with the answer that signs in (the fresh sign-in's, `sign_in.rs`).
pub(super) async fn finish(
    hub: &Arc<HubSignIn>,
    headers: &HeaderMap,
    proven: &Proven,
    link: bool,
    return_to: &str,
    extra_cookies: &[String],
) -> Response {
    let current = hub.who(headers).await.ok().flatten();
    let accounts = hub.accounts().clone();
    // A new Google sign-in whose address already has a password account, with nobody
    // signed in: ask whether to link it there (spec first-connect, A2).
    if !link && current.is_none() && proven.provider == "google" {
        match accounts.password_account_for(proven).await {
            Ok(Some(_)) => return link_page(hub, proven, return_to, None),
            Ok(None) => {}
            Err(error) => {
                eprintln!("devagotchi-hub: account store: {}", error.message);
                return super::routes::unavailable();
            }
        }
    }
    let done = match (&current, link) {
        (Some(current), true) => accounts.link_identity(&current.id, proven).await,
        (None, true) => {
            return failed(
                StatusCode::BAD_REQUEST,
                "Sign in first",
                "Adding a sign-in to an account takes being signed in to it, in this browser.",
            );
        }
        (current, false) => {
            accounts
                .sign_in_identity_from(proven, current.as_ref().map(|c| c.id.as_str()))
                .await
        }
    };
    let account = match done {
        Ok(account) => account,
        Err(error) => {
            let (status, title, text) = match error.code.as_str() {
                "no_account" => (
                    StatusCode::FORBIDDEN,
                    "No account here for that sign-in",
                    "This hub has no account for it. Ask its owner, or sign in another way.",
                ),
                "identity_taken" => (
                    StatusCode::CONFLICT,
                    "That sign-in is someone else's here",
                    "It already reaches another account on this hub, so it was not added to yours.",
                ),
                "operator_identity" => (
                    StatusCode::CONFLICT,
                    "That is the operator's sign-in",
                    "It signs in as this hub's operator. Sign out, then sign in with it.",
                ),
                "account_disabled" => (
                    StatusCode::FORBIDDEN,
                    "This account is disabled",
                    "Ask the hub's owner.",
                ),
                _ => {
                    eprintln!("devagotchi-hub: account store: {}", error.message);
                    return super::routes::unavailable();
                }
            };
            return failed(status, title, text);
        }
    };
    super::session::open_and_return(hub, current.as_ref(), &account, return_to, extra_cookies).await
}

/// How long the link page waits for its answer.
const LINK_SECS: u64 = 300;

/// A proven sign-in waiting on the link page, sealed in its cookie.
#[derive(Serialize, Deserialize)]
struct Waiting {
    p: String,
    s: String,
    e: Option<String>,
    n: Option<String>,
    /// Where to go after.
    r: String,
    t: u64,
}

fn link_cookie(hub: &HubSignIn) -> &'static str {
    if hub.secure() { "__Host-dg_link" } else { "dg_link" }
}

/// "<email> already has an account. Link Google to it?": Link (with that account's
/// password), or Keep separate. `notice` is why it is shown again (a wrong password).
fn link_page(hub: &Arc<HubSignIn>, proven: &Proven, return_to: &str, notice: Option<&str>) -> Response {
    let waiting = Waiting {
        p: proven.provider.clone(),
        s: proven.subject.clone(),
        e: proven.verified_email.clone(),
        n: proven.name.clone(),
        r: return_to.to_string(),
        t: now_secs(),
    };
    let json = serde_json::to_vec(&waiting).unwrap_or_default();
    let tag = hub.accounts().mac("link sign-in", &json);
    let sealed = format!("{}.{}", URL_SAFE_NO_PAD.encode(&json), URL_SAFE_NO_PAD.encode(tag));
    let words = &hub.site().messages.sign_in;
    let email = proven.verified_email.as_deref().unwrap_or_default();
    let title = words.link_title.replace("{email}", email);
    let e = pages::escape;
    let notice = notice.map(|text| format!("<p class=\"notice\" role=\"alert\">{}</p>", e(text)));
    let body = format!(
        "<h1>{title}</h1>{notice}<p>{explain}</p><form method=\"post\" action=\"/auth/provider/link\">\
<input type=\"hidden\" name=\"decision\" value=\"link\">\
<label>{password} for {email} <input type=\"password\" name=\"password\" required autocomplete=\"current-password\"></label>\
<p class=\"actions\"><button class=\"button primary\" type=\"submit\">{link}</button></p></form>\
<form method=\"post\" action=\"/auth/provider/link\"><input type=\"hidden\" name=\"decision\" value=\"separate\">\
<p class=\"actions\"><button class=\"button\" type=\"submit\">{separate}</button></p></form>\
<p><a href=\"/auth/password/forgot\">{forgot}</a></p>",
        title = e(&title),
        notice = notice.unwrap_or_default(),
        explain = e(&words.link_password),
        password = e(&words.password),
        email = e(email),
        link = e(&words.link),
        separate = e(&words.keep_separate),
        forgot = e(&words.forgot),
    );
    let mut response = pages::html(StatusCode::OK, &title, &body);
    super::session::form_may_return(&mut response, return_to);
    let headers = response.headers_mut();
    headers.insert(axum::http::header::REFERRER_POLICY, axum::http::HeaderValue::from_static("same-origin"));
    if let Ok(value) = axum::http::HeaderValue::from_str(&hub.cookie(link_cookie(hub), &sealed, LINK_SECS)) {
        headers.append(axum::http::header::SET_COOKIE, value);
    }
    response
}

#[derive(Deserialize)]
pub struct LinkDecision {
    decision: String,
    /// The password account's password: Link only.
    #[serde(default)]
    password: String,
}

/// `POST /auth/provider/link`: the link page's answer. Link adds the sign-in to the password
/// account its address has; Keep separate signs in as any new sign-in does.
pub async fn link(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<LinkDecision>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !super::approvals::same_origin(&headers, &state.hub.config.public_url) {
        return failed(StatusCode::FORBIDDEN, "Not from this hub", "Answer on this hub's own page.");
    }
    let waiting = cookie_value_up_to(&headers, link_cookie(&hub), 2048).and_then(|cookie| {
        let (json, tag) = cookie.split_once('.')?;
        let (json, tag) = (URL_SAFE_NO_PAD.decode(json).ok()?, URL_SAFE_NO_PAD.decode(tag).ok()?);
        let ok = hub.accounts().mac_ok("link sign-in", &json, &tag);
        let waiting: Waiting = serde_json::from_slice(&json).ok().filter(|_| ok)?;
        (now_secs() < waiting.t.saturating_add(LINK_SECS)).then_some(waiting)
    });
    let Some(waiting) = waiting else {
        return failed(StatusCode::BAD_REQUEST, "This sign-in expired", "Start again.");
    };
    let proven = Proven { provider: waiting.p, subject: waiting.s, verified_email: waiting.e, name: waiting.n };
    let accounts = hub.accounts().clone();
    let done = match form.decision.as_str() {
        "link" => match accounts.password_account_for(&proven).await {
            // Linking takes the password: whoever signed up with this address first may not
            // be the person Google just proved it for.
            Ok(Some(account)) => {
                let email = proven.verified_email.as_deref().unwrap_or_default();
                match accounts.sign_in_password(email, &form.password).await {
                    Ok(holder) if holder.id == account => accounts.link_identity(&account, &proven).await,
                    Ok(_) => Err(devagotchi_contracts::BridgeError::new("password_wrong", "wrong")),
                    Err(error) => Err(error),
                }
            }
            Ok(None) => accounts.sign_in_identity_from(&proven, None).await,
            Err(error) => Err(error),
        },
        _ => accounts.sign_in_identity_from(&proven, None).await,
    };
    let words = hub.site().messages.sign_in.clone();
    let again = |status: StatusCode, notice: &str| {
        let mut page = link_page(&hub, &proven, &waiting.r, Some(notice));
        *page.status_mut() = status;
        page
    };
    let account = match done {
        Ok(account) => account,
        Err(error) if error.code == "password_wrong" => return again(StatusCode::BAD_REQUEST, &words.wrong),
        Err(error) if error.code == "slow_down" => return again(StatusCode::TOO_MANY_REQUESTS, &words.slow_down),
        Err(error) if error.code == "account_disabled" => {
            return failed(StatusCode::FORBIDDEN, "This account is disabled", "Ask the hub's owner.");
        }
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let cookies = [hub.cookie(link_cookie(&hub), "", 0), super::sign_in::fresh_cookie(&hub, &waiting.r)];
    super::session::open_and_return(&hub, None, &account, &waiting.r, &cookies).await
}
