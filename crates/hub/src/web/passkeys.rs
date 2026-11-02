//! Passkey sign-in on the hub's own pages (`auth.mode hub`, D25 §14): the owner's one-time
//! link makes their first passkey, and `/auth/login` signs in with one. Nothing here talks
//! to another service.
//!
//! - Challenges are the hub's own, signed and stateless until used (`accounts.rs`): asking
//!   for one stores nothing, so a flood of unanswered sign-ins blocks nobody.
//! - Each challenge also rides in a short-lived `__Host-` cookie that only this site can
//!   set, and the answer is checked against the cookie, so another site cannot sign a
//!   browser in to an account of its choosing (login CSRF).
//! - The JSON endpoints answer only this hub's own pages: the `Origin` must be the hub's,
//!   and `Sec-Fetch-Site`, when sent, same-origin. Both are checked before the body is
//!   parsed, and the body must be `application/json`.
//! - The pages run one script from the hub itself (`/assets/passkey.js`), under a strict
//!   Content-Security-Policy.
//! - The owner's link is `/owner#<token>`: the token never reaches a request line or a log.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::json;

use super::{cookie_value, pages, safe_return_to};
use crate::{
    AppState,
    accounts::{
        Accounts, Ceremony, LinkPurpose,
        passkey::{self, Assertion, Registration, Relying},
        sessions::SESSION_MAX_SECS,
    },
    auth::Subject,
    config::HubConfig,
};

/// The hub's own sign-in: its accounts, and who its passkeys belong to.
pub struct HubSignIn {
    accounts: Arc<Accounts>,
    config: Arc<HubConfig>,
    rp_id: String,
    origin: String,
    secure: bool,
    /// Seen claimed: the owner has a passkey (`claimed`).
    claimed: AtomicBool,
    /// Sign in with Google, GitHub, OpenID Connect providers and the email link.
    pub(super) providers: super::providers::Providers,
}

/// `scheme://host[:port]` of the hub's public URL, as a browser writes an origin.
pub(crate) fn origin_of(public_url: &str) -> Option<String> {
    let uri = public_url.parse::<axum::http::Uri>().ok()?;
    let scheme = uri.scheme_str()?;
    let host = uri.host()?.to_ascii_lowercase();
    let port = match (scheme, uri.port_u16()) {
        ("https", Some(443)) | ("http", Some(80)) | (_, None) => String::new(),
        (_, Some(port)) => format!(":{port}"),
    };
    Some(format!("{scheme}://{host}{port}"))
}

impl HubSignIn {
    /// Whether the store holds the hub's queries to one tenant's rows (`/healthz`).
    pub fn tenant_rls(&self) -> bool {
        self.accounts.store().enforced()
    }

    /// Passkey sign-in for a hub in `auth.mode hub`; `None` in any other mode.
    pub fn new(config: Arc<HubConfig>, accounts: Arc<Accounts>) -> Option<Self> {
        let rp_id = config.rp_id()?;
        let origin = origin_of(&config.public_url)?;
        Some(Self {
            secure: config.public_url.starts_with("https://"),
            accounts,
            config,
            rp_id,
            origin,
            claimed: AtomicBool::new(false),
            providers: super::providers::Providers::none(),
        })
    }

    /// This sign-in with the config's providers and email link (`Providers::from_config`).
    pub fn with_providers(mut self, providers: super::providers::Providers) -> Self {
        self.providers = providers;
        self
    }

    /// The other ways to secure the signed-in account this hub offers, as links and a form
    /// that add them to it: each provider, and the email link.
    pub(super) fn linking_ways(&self) -> String {
        let buttons = self.providers.buttons("/account", true);
        let mut ways = String::new();
        if !buttons.is_empty() {
            ways.push_str(&format!("<p class=\"ways\">Or add a sign-in you already have: {buttons}</p>"));
        }
        if self.providers.email.is_some() {
            ways.push_str(&super::email::form("/account", true));
        }
        ways
    }

    /// Whether the hub is on https (its cookies `__Host-` and `Secure`).
    pub(super) fn secure(&self) -> bool {
        self.secure
    }

    /// The hub's public URL, without a trailing slash.
    pub(super) fn public_url(&self) -> &str {
        self.config.public_url.trim_end_matches('/')
    }

    /// The owner signed in another way than a passkey: every holder of the config knows them.
    pub(super) fn learn_owner_of(&self, account: &str) {
        self.learn_owner(account);
    }

    /// The accounts behind this sign-in, for the hub's own authorization server.
    pub(super) fn accounts(&self) -> &Arc<Accounts> {
        &self.accounts
    }

    /// Whether this hub can be claimed from the browser (`accounts/claim.rs`): a one-owner
    /// hub whose config names no owner and no operators, and no owner can sign in with a
    /// passkey yet. A config that names
    /// its owner has decided who that is: their passkey is made on the host with
    /// `devagotchi hub owner`, whatever the log says. `Err` when the store failed.
    pub(super) async fn claimable(&self) -> Result<bool, ()> {
        // A hosted hub is never claimed, and neither is one whose operators were set at
        // deploy (`HubConfig::claim_allowed`).
        if !self.config.claim_allowed() {
            return Ok(false);
        }
        self.claimed().await.map(|claimed| !claimed)
    }

    /// Whether the hub's owner can sign in with a passkey yet. Once they can, that does not
    /// change, so the store is asked only until then. `Err` when it failed.
    pub(super) async fn claimed(&self) -> Result<bool, ()> {
        if self.claimed.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let accounts = self.accounts.clone();
        match tokio::spawn(async move { accounts.claimed().await }).await {
            Ok(Ok(claimed)) => {
                if claimed {
                    self.claimed.store(true, Ordering::Relaxed);
                }
                Ok(claimed)
            }
            Ok(Err(error)) => {
                eprintln!("devagotchi-hub: account store: {}", error.message);
                Err(())
            }
            Err(_) => Err(()),
        }
    }

    fn relying(&self) -> Relying<'_> {
        Relying {
            rp_id: &self.rp_id,
            origin: &self.origin,
        }
    }

    pub fn session_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-dg_session"
        } else {
            "dg_session"
        }
    }

    fn challenge_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-dg_challenge"
        } else {
            "dg_challenge"
        }
    }

    /// A `Set-Cookie` value; `max_age` 0 deletes the cookie.
    pub fn cookie(&self, name: &str, value: &str, max_age: u64) -> String {
        let secure = if self.secure { "; Secure" } else { "" };
        format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
    }

    /// The account behind this request's session cookie. `Err` when the store failed.
    pub async fn who(&self, headers: &HeaderMap) -> Result<Option<Subject>, ()> {
        let Some(token) = cookie_value(headers, self.session_cookie()) else {
            return Ok(None);
        };
        let accounts = self.accounts.clone();
        match tokio::spawn(async move { accounts.session(&token).await }).await {
            Ok(Ok(Some(session))) => Ok(Some(self.accounts.subject(&session.account, "passkey"))),
            Ok(Ok(None)) => Ok(None),
            Ok(Err(error)) => {
                eprintln!("devagotchi-hub: account store: {}", error.message);
                Err(())
            }
            Err(_) => Err(()),
        }
    }

    /// Whether this request's cookie names a live session (for the top bar).
    pub async fn signed_in(&self, headers: &HeaderMap) -> bool {
        let Some(token) = cookie_value(headers, self.session_cookie()) else {
            return false;
        };
        let accounts = self.accounts.clone();
        matches!(
            tokio::spawn(async move { accounts.session(&token).await }).await,
            Ok(Ok(Some(_)))
        )
    }

    /// Sign out: the session ends on the hub, whatever the browser does with its cookie.
    pub async fn end_session(&self, headers: &HeaderMap) {
        let Some(token) = cookie_value(headers, self.session_cookie()) else {
            return;
        };
        let accounts = self.accounts.clone();
        if let Ok(Err(error)) =
            tokio::spawn(async move { accounts.end_session(&token).await }).await
        {
            eprintln!("devagotchi-hub: account store: {}", error.message);
        }
    }

    /// An owner link names the owner the config already knows, if it knows one: a store
    /// whose owner was made without the config's id is refused before its link is spent.
    pub(super) fn owner_mismatch(&self, account: &str) -> Option<Response> {
        let known = self.config.owner_id()?;
        (known != account).then(|| {
            eprintln!(
                "devagotchi-hub: an owner link names {account}, but the config's owner is {known}; run devagotchi hub owner with the config the hub runs with"
            );
            refuse(
                StatusCode::CONFLICT,
                "owner_mismatch",
                "This link is for another owner than this hub's. Make a new one on the hub's host with the config the hub runs with.",
            )
        })
    }

    /// The JSON endpoints answer only the hub's own pages.
    pub(super) fn from_own_page(&self, headers: &HeaderMap) -> bool {
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        if header("sec-fetch-site").is_some_and(|site| site != "same-origin") {
            return false;
        }
        header("origin") == Some(self.origin.as_str())
    }

    fn challenge_from(&self, headers: &HeaderMap) -> Option<Vec<u8>> {
        cookie_value(headers, self.challenge_cookie())
            .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
    }

    /// The options answer: the JSON body, with the challenge also set as a cookie.
    fn with_challenge(&self, challenge: &[u8], body: serde_json::Value) -> Response {
        let mut response = Json(body).into_response();
        let cookie = self.cookie(
            self.challenge_cookie(),
            &URL_SAFE_NO_PAD.encode(challenge),
            crate::accounts::CHALLENGE_TTL_SECS,
        );
        let headers = response.headers_mut();
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            headers.append(header::SET_COOKIE, value);
        }
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }

    /// Signed in: the session cookie set, the challenge cookie cleared, and where to go.
    fn signed_in_to(&self, token: &str, next: &str) -> Response {
        let mut response = Json(json!({ "next": next })).into_response();
        let headers = response.headers_mut();
        for cookie in [
            self.cookie(self.session_cookie(), token, SESSION_MAX_SECS),
            self.cookie(self.challenge_cookie(), "", 0),
        ] {
            if let Ok(value) = HeaderValue::from_str(&cookie) {
                headers.append(header::SET_COOKIE, value);
            }
        }
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }

    /// The owner made a passkey or signed in with one: every holder of the config now
    /// knows them, and the hub is claimed.
    fn learn_owner(&self, account: &str) {
        self.claimed.store(true, Ordering::Relaxed);
        if let Err(why) = self.config.learn_owner(account) {
            eprintln!("devagotchi-hub: {why}");
        }
    }
}

pub(super) fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    let mut response = (status, Json(json!({ "error": code, "message": message }))).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) fn not_own_page() -> Response {
    refuse(
        StatusCode::FORBIDDEN,
        "not_same_origin",
        "Use this hub's own sign-in page.",
    )
}

fn expired() -> Response {
    refuse(
        StatusCode::BAD_REQUEST,
        "expired",
        "This sign-in timed out or was already used. Start again.",
    )
}

pub(super) fn store_failed(message: &str) -> Response {
    eprintln!("devagotchi-hub: account store: {message}");
    refuse(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "The hub could not finish that just now. Try again in a minute.",
    )
}

/// The ceremonies take JSON only, as the hub's script sends it.
fn json_only(headers: &HeaderMap) -> Result<(), Response> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if json {
        Ok(())
    } else {
        Err(refuse(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "not_json",
            "Use this hub's own sign-in page.",
        ))
    }
}

/// The request body as `T`, parsed only after the origin check has passed.
pub(super) fn body<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    raw: &[u8],
) -> Result<T, Response> {
    json_only(headers)?;
    serde_json::from_slice(raw).map_err(|_| {
        refuse(
            StatusCode::BAD_REQUEST,
            "malformed",
            "The passkey's answer was garbled.",
        )
    })
}

fn bytes(field: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(field.trim_end_matches('=')).ok()
}

/// The page for a script: the hub's one passkey script, nothing else.
pub(super) fn scripted(title: &str, body: &str) -> Response {
    pages::scripted(StatusCode::OK, title, body, "/assets/passkey.js")
}

/// `/auth/login` in hub mode: the passkey button and the script that runs its prompt, then
/// every other way in this hub offers (`providers.rs`, `email.rs`).
pub fn login_page(hub: &HubSignIn, return_to: &str) -> Response {
    let return_to = safe_return_to(return_to);
    let others = hub.providers.buttons(return_to, false);
    let others = if others.is_empty() {
        String::new()
    } else {
        format!("<p class=\"ways\">{others}</p>")
    };
    let email = if hub.providers.email.is_some() {
        super::email::form(return_to, false)
    } else {
        String::new()
    };
    let mut response = scripted(
        "Sign in",
        &format!(
            "<h1>Sign in</h1>\
<p>Use the passkey you made for this hub: your phone's or computer's fingerprint, face or PIN.</p>\
<p><button class=\"button primary\" id=\"passkey-sign-in\" data-return-to=\"{}\" disabled>Sign in with a passkey</button></p>\
<p class=\"meta\" id=\"passkey-status\" role=\"status\"></p>\
<noscript><p>Signing in with a passkey needs JavaScript for this page.</p></noscript>{others}{email}",
            pages::escape(return_to)
        ),
    );
    // Its email form posts here: the browser sends that POST's true Origin only under
    // `same-origin`.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

/// `/owner#<link>`: the owner's one-time link carries its token after `#`, which a browser
/// never sends, so it stays out of every proxy's and server's request log. The script reads
/// it, takes it out of the address bar, and checks it with the hub when the button is used.
pub async fn owner_page(State(state): State<AppState>) -> Response {
    if state.passkeys.is_none() {
        return super::routes::not_set_up();
    }
    scripted(
        "Make your passkey",
        "<h1>Make your passkey</h1>\
<p>This makes the passkey that signs the hub's owner in to this hub. Use the phone or computer \
you want to sign in with; your passkey manager can share it with your other devices.</p>\
<p><button class=\"button primary\" id=\"passkey-register\" disabled>Make a passkey</button></p>\
<p class=\"meta\" id=\"passkey-status\" role=\"status\"></p>\
<p class=\"meta\">Any other session of this account ends when you do. The link works once, for 15 minutes; \
on the hub's host, <code>devagotchi hub owner</code> prints a new one.</p>\
<noscript><p>Making a passkey needs JavaScript for this page.</p></noscript>",
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkIn {
    /// An owner link; absent to secure the account this browser is signed in to.
    #[serde(default)]
    link: Option<String>,
}

pub async fn register_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: axum::body::Bytes,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !hub.from_own_page(&headers) {
        return not_own_page();
    }
    let input: LinkIn = match body(&headers, &raw) {
        Ok(input) => input,
        Err(refused) => return refused,
    };
    match input.link.filter(|link| !link.is_empty()) {
        Some(link) => registration(hub, link, false).await,
        None => securing(hub, &headers).await,
    }
}

/// The account this browser is signed in to, when nobody has secured it yet: the only
/// account a session alone may add a passkey to (anything else takes a link from the hub's
/// host), with the session's token. The page to show otherwise.
async fn unsecured_session(
    hub: &Arc<HubSignIn>,
    headers: &HeaderMap,
) -> Result<(crate::auth::Subject, String), Response> {
    let refused = || {
        refuse(
            StatusCode::FORBIDDEN,
            "not_unsecured",
            "Only an account that is not secured yet adds its first passkey here.",
        )
    };
    let Some(token) = cookie_value(headers, hub.session_cookie()) else {
        return Err(expired());
    };
    match hub.who(headers).await {
        Ok(Some(subject)) if subject.provisional_until.is_some() => Ok((subject, token)),
        Ok(Some(_)) => Err(refused()),
        Ok(None) => Err(expired()),
        Err(()) => Err(store_failed("a session could not be checked")),
    }
}

/// The options to make the first passkey of the account this browser is signed in to.
async fn securing(hub: Arc<HubSignIn>, headers: &HeaderMap) -> Response {
    let (subject, _) = match unsecured_session(&hub, headers).await {
        Ok(found) => found,
        Err(page) => return page,
    };
    let challenge = hub.accounts.challenge(Ceremony::Register, Some(&subject.id));
    let name = subject.name.clone().unwrap_or_else(|| subject.id.clone());
    hub.with_challenge(
        &challenge,
        json!({
            "challenge": URL_SAFE_NO_PAD.encode(&challenge),
            "rp": { "id": hub.rp_id, "name": "Devagotchi" },
            "user": {
                "id": URL_SAFE_NO_PAD.encode(subject.id.as_bytes()),
                "name": name,
                "displayName": name,
            },
            "pubKeyCredParams": [
                { "type": "public-key", "alg": -8 },
                { "type": "public-key", "alg": -7 },
                { "type": "public-key", "alg": -257 },
            ],
            "timeout": crate::accounts::CHALLENGE_TTL_SECS * 1000,
            "attestation": "none",
            "authenticatorSelection": {
                "residentKey": "required",
                "requireResidentKey": true,
                "userVerification": "required",
            },
            "excludeCredentials": [],
        }),
    )
}

/// The options to make a passkey with the owner link `link`: who it is for, the passkeys it
/// must not make again, and a register challenge (also set as the challenge cookie). With
/// `hand_back`, the answer carries `link` too, for the page that just got it by claiming
/// the hub (`claim.rs`).
pub(super) async fn registration(hub: Arc<HubSignIn>, link: String, hand_back: bool) -> Response {
    let accounts = hub.accounts.clone();
    let checked = link.clone();
    let found = tokio::spawn(async move {
        let (account, replaces) = accounts
            .check_link_replacing(LinkPurpose::Owner, &checked)
            .await?;
        let holder = accounts.get(&account).await?;
        // A replacing link removes the account's passkeys, so none is excluded: the device
        // that holds the old one may make the new one in its place.
        let existing = if replaces {
            Vec::new()
        } else {
            accounts.passkeys_of(&account).await?
        };
        let challenge = accounts.challenge(Ceremony::Register, Some(&account));
        Ok::<_, devagotchi_contracts::BridgeError>((holder, existing, challenge))
    })
    .await;
    let (account, existing, challenge) = match found {
        Ok(Ok((Some(account), existing, challenge))) => (account, existing, challenge),
        Ok(Err(error)) if error.code != "invalid_link" => return store_failed(&error.message),
        _ => return expired(),
    };
    if let Some(refused) = hub.owner_mismatch(&account.id) {
        return refused;
    }
    let exclude: Vec<_> = existing
        .iter()
        .map(|p| json!({ "type": "public-key", "id": URL_SAFE_NO_PAD.encode(&p.id) }))
        .collect();
    let mut options = json!({
        "challenge": URL_SAFE_NO_PAD.encode(&challenge),
        "rp": { "id": hub.rp_id, "name": "Devagotchi" },
        "user": {
            "id": URL_SAFE_NO_PAD.encode(account.id.as_bytes()),
            "name": account.name,
            "displayName": account.name,
        },
        // Ed25519 first, then P-256 (nearly every passkey), then RSA (older Windows Hello).
        "pubKeyCredParams": [
            { "type": "public-key", "alg": -8 },
            { "type": "public-key", "alg": -7 },
            { "type": "public-key", "alg": -257 },
        ],
        "timeout": crate::accounts::CHALLENGE_TTL_SECS * 1000,
        "attestation": "none",
        "authenticatorSelection": {
            "residentKey": "required",
            "requireResidentKey": true,
            "userVerification": "required",
        },
        "excludeCredentials": exclude,
    });
    if hand_back {
        options["link"] = link.into();
    }
    hub.with_challenge(&challenge, options)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterIn {
    #[serde(default)]
    link: Option<String>,
    id: String,
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    authenticator_data: String,
    public_key: String,
    public_key_algorithm: i64,
}

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: axum::body::Bytes,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !hub.from_own_page(&headers) {
        return not_own_page();
    }
    let input: RegisterIn = match body(&headers, &raw) {
        Ok(input) => input,
        Err(refused) => return refused,
    };
    let Some(challenge) = hub.challenge_from(&headers) else {
        return expired();
    };
    let (Some(id), Some(client_data_json), Some(authenticator_data), Some(public_key)) = (
        bytes(&input.id),
        bytes(&input.client_data_json),
        bytes(&input.authenticator_data),
        bytes(&input.public_key),
    ) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "malformed",
            "The passkey's answer was garbled.",
        );
    };
    let Some(link) = input.link.clone().filter(|link| !link.is_empty()) else {
        return secure_with(hub, &headers, &challenge, &input).await;
    };
    let worker = hub.clone();
    let done = tokio::spawn(async move {
        let accounts = &worker.accounts;
        let account = accounts.check_link(LinkPurpose::Owner, &link).await?;
        if worker
            .config
            .owner_id()
            .is_some_and(|known| known != account)
        {
            return Err(devagotchi_contracts::BridgeError::new(
                "owner_mismatch",
                account,
            ));
        }
        let credential = passkey::verify_registration(
            worker.relying(),
            &challenge,
            Registration {
                credential_id: &id,
                client_data_json: &client_data_json,
                authenticator_data: &authenticator_data,
                public_key: &public_key,
                alg: input.public_key_algorithm,
            },
        )
        .map_err(|refusal| {
            eprintln!("devagotchi-hub: refused a passkey registration: {refusal}");
            devagotchi_contracts::BridgeError::new("refused", refusal.0)
        })?;
        let label = format!("Passkey made {}", pages::utc(crate::devices::now_secs()));
        // The passkey, the link spent and the session it signs in to: all or nothing.
        let token = accounts
            .register_with_link(
                &account,
                &credential,
                &label,
                LinkPurpose::Owner,
                &link,
            )
            .await?;
        Ok::<_, devagotchi_contracts::BridgeError>((account, token))
    })
    .await;
    match done {
        Ok(Ok((account, token))) => {
            // An owner link only ever names the owner.
            hub.learn_owner(&account);
            hub.signed_in_to(&token, "/account")
        }
        Ok(Err(error)) => match error.code.as_str() {
            "invalid_link" | "invalid_ceremony" => expired(),
            "owner_mismatch" => hub
                .owner_mismatch(&error.message)
                .unwrap_or_else(|| store_failed("the owner changed during a registration")),
            "refused" => refuse(
                StatusCode::BAD_REQUEST,
                "refused",
                "That passkey was not accepted here. Try again, or another device.",
            ),
            "passkey_exists" => refuse(
                StatusCode::CONFLICT,
                "passkey_exists",
                "That passkey is already registered on this hub.",
            ),
            "account_disabled" => refuse(
                StatusCode::FORBIDDEN,
                "account_disabled",
                "This account is disabled.",
            ),
            _ => store_failed(&error.message),
        },
        Err(_) => store_failed("a passkey registration did not complete"),
    }
}

/// The first passkey of the account this browser is signed in to, nobody having secured it
/// yet: stored on the session's authority, checked again in the same transaction. The
/// account is secured from then on; the session goes on.
async fn secure_with(
    hub: Arc<HubSignIn>,
    headers: &HeaderMap,
    challenge: &[u8],
    input: &RegisterIn,
) -> Response {
    let (subject, session) = match unsecured_session(&hub, headers).await {
        Ok(found) => found,
        Err(page) => return page,
    };
    let (Some(id), Some(client_data_json), Some(authenticator_data), Some(public_key)) = (
        bytes(&input.id),
        bytes(&input.client_data_json),
        bytes(&input.authenticator_data),
        bytes(&input.public_key),
    ) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "malformed",
            "The passkey's answer was garbled.",
        );
    };
    let credential = match passkey::verify_registration(
        hub.relying(),
        challenge,
        Registration {
            credential_id: &id,
            client_data_json: &client_data_json,
            authenticator_data: &authenticator_data,
            public_key: &public_key,
            alg: input.public_key_algorithm,
        },
    ) {
        Ok(credential) => credential,
        Err(refusal) => {
            eprintln!("devagotchi-hub: refused a passkey registration: {refusal}");
            return refuse(
                StatusCode::BAD_REQUEST,
                "refused",
                "That passkey was not accepted here. Try again, or another device.",
            );
        }
    };
    let accounts = hub.accounts.clone();
    let account = subject.id.clone();
    let kept = session.clone();
    let done = tokio::spawn(async move {
        let label = format!("Passkey made {}", pages::utc(crate::devices::now_secs()));
        accounts
            .add_passkey_signed_in(&account, &credential, &label, &kept)
            .await
    })
    .await;
    match done {
        Ok(Ok(())) => hub.signed_in_to(&session, "/account"),
        Ok(Err(error)) => match error.code.as_str() {
            "invalid_link" | "invalid_ceremony" => expired(),
            "passkey_exists" => refuse(
                StatusCode::CONFLICT,
                "passkey_exists",
                "That passkey is already registered on this hub.",
            ),
            "account_disabled" => refuse(
                StatusCode::FORBIDDEN,
                "account_disabled",
                "This account is disabled.",
            ),
            _ => store_failed(&error.message),
        },
        Err(_) => store_failed("a passkey registration did not complete"),
    }
}

pub async fn sign_in_options(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !hub.from_own_page(&headers) {
        return not_own_page();
    }
    if let Err(refused) = json_only(&headers) {
        return refused;
    }
    let challenge = hub.accounts.challenge(Ceremony::SignIn, None);
    hub.with_challenge(
        &challenge,
        json!({
            "challenge": URL_SAFE_NO_PAD.encode(&challenge),
            "rpId": hub.rp_id,
            "timeout": crate::accounts::CHALLENGE_TTL_SECS * 1000,
            "userVerification": "required",
            "allowCredentials": [],
        }),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInIn {
    id: String,
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    authenticator_data: String,
    signature: String,
    #[serde(default)]
    user_handle: Option<String>,
    #[serde(default)]
    return_to: Option<String>,
}

pub async fn sign_in(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: axum::body::Bytes,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !hub.from_own_page(&headers) {
        return not_own_page();
    }
    let input: SignInIn = match body(&headers, &raw) {
        Ok(input) => input,
        Err(refused) => return refused,
    };
    let Some(challenge) = hub.challenge_from(&headers) else {
        return expired();
    };
    let user_handle = match input.user_handle.as_deref().map(bytes) {
        None => None,
        Some(Some(handle)) => Some(handle),
        Some(None) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "malformed",
                "The passkey's answer was garbled.",
            );
        }
    };
    let (Some(id), Some(client_data_json), Some(authenticator_data), Some(signature)) = (
        bytes(&input.id),
        bytes(&input.client_data_json),
        bytes(&input.authenticator_data),
        bytes(&input.signature),
    ) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "malformed",
            "The passkey's answer was garbled.",
        );
    };
    let worker = hub.clone();
    let done = tokio::spawn(async move {
        let accounts = &worker.accounts;
        let Some(stored) = accounts.passkey(&id).await? else {
            return Err(devagotchi_contracts::BridgeError::new(
                "invalid_passkey",
                "unknown passkey",
            ));
        };
        let asserted = passkey::check_sign_in(
            worker.relying(),
            &challenge,
            &stored,
            Assertion {
                credential_id: &id,
                client_data_json: &client_data_json,
                authenticator_data: &authenticator_data,
                signature: &signature,
            },
            user_handle.as_deref(),
        )
        .map_err(|refusal| {
            eprintln!("devagotchi-hub: refused a passkey sign-in: {refusal}");
            devagotchi_contracts::BridgeError::new("refused", refusal.0)
        })?;
        let account = accounts.sign_in(&asserted).await?;
        let token = accounts
            .open_session(&account.id, Some(asserted.id()))
            .await?;
        Ok((account, token))
    })
    .await;
    match done {
        Ok(Ok((account, token))) => {
            if account.owner {
                hub.learn_owner(&account.id);
            }
            let next = safe_return_to(input.return_to.as_deref().unwrap_or("/account"));
            hub.signed_in_to(&token, next)
        }
        Ok(Err(error)) => match error.code.as_str() {
            "invalid_ceremony" => expired(),
            "invalid_passkey" | "refused" | "signature_counter" | "user_handle" => refuse(
                StatusCode::BAD_REQUEST,
                "refused",
                "That passkey did not sign you in here. Use the passkey you made for this hub.",
            ),
            "account_disabled" => refuse(
                StatusCode::FORBIDDEN,
                "account_disabled",
                "This account is disabled.",
            ),
            _ => store_failed(&error.message),
        },
        Err(_) => store_failed("a sign-in did not complete"),
    }
}

pub async fn script() -> Response {
    let mut response = include_str!("assets/passkey.js").into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// `GET /auth/login` when the hub signs people in itself.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<super::routes::LoginQuery>,
) -> Response {
    let return_to = query.return_to.as_deref().unwrap_or("/account");
    match &state.passkeys {
        Some(hub) if hub.signed_in(&headers).await => {
            super::routes::see_other(safe_return_to(return_to), &[])
        }
        Some(hub) => login_page(hub, return_to),
        None => super::routes::not_set_up(),
    }
}
