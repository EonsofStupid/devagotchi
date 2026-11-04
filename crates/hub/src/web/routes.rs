//! The browser routes: the landing page and claiming a new hub (`/claim`), sign in, the
//! account page, the operator page (`/operator`) and its analytics (`/operator/analytics`), sign out, `/api/v1/me` (which also takes a bearer token, so scripts and AI
//! clients see the same view), `/approvals`, a workflow's link (`/go/<token>`), the public
//! pages the AI directories link to (`/docs`, `/privacy`, `/terms`, `/support`), and what
//! search engines and AI agents read (`/llms.txt`, `/llms-full.txt`, `/robots.txt`,
//! `/sitemap.xml`).

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

use super::{
    Exchange, SessionToken, WebLogin, analytics, approvals, claim, device, discovery, email, machines,
    notify, oauth, offers, operator,
    pages::{self, Viewer},
    passkeys, providers, public, workflow,
};
use crate::{
    AppState,
    auth::{AuthError, Subject},
};

/// Every browser route. `hub_authorization` mounts the routes of the hub's own
/// authorization server for AI clients (32b) and the browser claim of a new hub (16c):
/// `/claim`, `/.well-known/oauth-authorization-server` and `/oauth/*`. It is false only in
/// `auth.mode devagotchi`, whose own authorization server (`crate::oauth`) serves `/claim`
/// and the metadata itself: axum refuses the same path twice when the router is built.
pub fn router(hub_authorization: bool) -> Router<AppState> {
    let router = Router::new()
        .route("/", get(root))
        .route("/account", get(account))
        .route("/signed-out", get(signed_out))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .route("/owner", get(passkeys::owner_page))
        .route(
            "/passkey/register/options",
            post(passkeys::register_options),
        )
        .route("/passkey/register", post(passkeys::register))
        .route("/passkey/sign-in/options", post(passkeys::sign_in_options))
        .route("/passkey/sign-in", post(passkeys::sign_in))
        .route("/assets/passkey.js", get(passkeys::script))
        .route("/auth/provider/{name}", get(providers::start))
        .route("/auth/provider/{name}/callback", get(providers::callback))
        .route("/auth/email", get(email::page).post(email::ask))
        .route("/auth/email/verify", post(email::verify))
        .route("/api/v1/me", get(me))
        .route("/api/v1/machines", post(machines::add_json))
        .route("/operator", get(operator::page))
        .route("/operator/admit", post(operator::admit))
        .route("/operator/plan", post(operator::plan))
        .route("/operator/analytics", get(analytics::page))
        .route("/operator/analytics.csv", get(analytics::csv))
        .route("/approvals", get(approvals::list))
        .route("/approvals/decide", post(approvals::decide))
        .route("/approvals/test", post(approvals::test))
        .route("/machines/add", post(machines::add))
        .route("/machines/revoke", post(machines::revoke))
        .route("/push/subscribe", post(notify::subscribe))
        .route("/sw.js", get(notify::service_worker))
        .route("/manifest.webmanifest", get(notify::manifest))
        .route("/assets/push.js", get(notify::script))
        .route("/docs", get(public::docs))
        .route("/privacy", get(public::privacy))
        .route("/terms", get(public::terms))
        .route("/support", get(public::support))
        // What search engines and AI agents read (`discovery.rs`).
        .route("/llms.txt", get(discovery::llms))
        .route("/llms-full.txt", get(discovery::llms_full))
        .route("/robots.txt", get(discovery::robots))
        .route("/sitemap.xml", get(discovery::sitemap))
        // A workflow's link (`workflow.rs`).
        .route("/go/{token}", get(workflow::page))
        .route("/go/{token}/status", get(workflow::status))
        .route("/go/{token}/command", post(workflow::command))
        .route("/go/{token}/phone", post(workflow::phone))
        .route("/go/{token}/keep", post(workflow::keep))
        .route("/go/{token}/client", post(workflow::client))
        .route("/assets/workflow.js", get(workflow::script))
        // The one-tap link that links an account to the site's network (`offers.rs`).
        .route("/offers/{token}", get(offers::show).post(offers::go))
        .route("/assets/hub.css", get(css))
        // The site's own files: its logo, icon, pictures, video and fonts.
        .route("/assets/{file}", get(super::media::asset));
    if !hub_authorization {
        return router;
    }
    router
        .route("/claim", get(claim::page).post(claim::claim))
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::metadata).options(oauth::preflight),
        )
        .route(
            "/oauth/register",
            post(oauth::register).options(oauth::preflight),
        )
        .route("/oauth/authorize", get(oauth::authorize).post(oauth::decide))
        .route("/oauth/token", post(oauth::token).options(oauth::preflight))
        .route("/oauth/revoke", post(oauth::revoke).options(oauth::preflight))
        .route("/oauth/device", post(device::start).options(oauth::preflight))
        .route("/device", get(device::page).post(device::decide))
}

#[derive(Deserialize)]
pub(super) struct LoginQuery {
    pub(super) return_to: Option<String>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Who the browser's session belongs to: a hub account's session, or (in Nomen mode) a
/// session whose token is checked against Nomen like any bearer token.
pub(super) enum Who {
    Person(Subject),
    SignedOut,
    Unavailable,
    /// This hub has no browser sign-in.
    NotSetUp,
}

pub(super) fn see_other(location: &str, cookies: &[String]) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(location) {
        headers.insert(header::LOCATION, value);
    }
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            headers.append(header::SET_COOKIE, value);
        }
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) fn not_set_up() -> Response {
    pages::message(
        StatusCode::NOT_FOUND,
        "Browser sign-in is not set up",
        "This hub has no web sign-in configured. AI clients still connect at /mcp.",
        None,
    )
}

pub(super) fn unavailable() -> Response {
    pages::message(
        StatusCode::SERVICE_UNAVAILABLE,
        "Sign-in is unavailable",
        "The hub could not check your sign-in just now. Your access has not changed; try again in a minute.",
        Some(("/account", "Try again")),
    )
}

pub(super) async fn who(state: &AppState, headers: &HeaderMap) -> Who {
    if let Some(hub) = &state.passkeys {
        return match hub.who(headers).await {
            Ok(Some(subject)) => Who::Person(subject),
            Ok(None) => Who::SignedOut,
            Err(()) => Who::Unavailable,
        };
    }
    let Some(web) = &state.web else {
        return Who::NotSetUp;
    };
    nomen_who(state, web, headers).await
}

/// Whether this request carries a live session, without asking any other service.
pub(super) async fn signed_in(state: &AppState, headers: &HeaderMap) -> bool {
    match (&state.passkeys, &state.web) {
        (Some(hub), _) => hub.signed_in(headers).await,
        (None, Some(web)) => web.signed_in(headers),
        (None, None) => false,
    }
}

async fn nomen_who(state: &AppState, web: &WebLogin, headers: &HeaderMap) -> Who {
    let token = match web.token(headers).await {
        SessionToken::Token(token) => token,
        SessionToken::None => return Who::SignedOut,
        SessionToken::Unavailable => return Who::Unavailable,
    };
    match state.verifier.verify(&token).await {
        Ok(subject) => Who::Person(subject),
        Err(AuthError::Invalid(why)) => {
            crate::log_refusal(why);
            web.end_session(headers);
            Who::SignedOut
        }
        Err(AuthError::Unavailable(why)) => {
            crate::log_unavailable(&why);
            Who::Unavailable
        }
    }
}

/// `/`: the landing page. A hub that can be claimed offers only the claim; otherwise it says
/// what the site's product is and where AIs connect, and offers one Sign in instead of
/// sending a stranger straight to an identity provider. Signed in, it is your account.
async fn root(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let public_url = state.hub.config.public_url.clone();
    if let Some(hub) = &state.passkeys
        && state.hub.config.multi_tenant()
    {
        // A hosted hub is never claimed: its page gets people started.
        return if hub.signed_in(&headers).await {
            see_other("/account", &[])
        } else {
            pages::home(pages::Front::Hosted, &public_url)
        };
    }
    if let Some(hub) = &state.passkeys {
        return match hub.claimable().await {
            Ok(true) => pages::home(pages::Front::Unclaimed, &public_url),
            Ok(false) if signed_in(&state, &headers).await => see_other("/account", &[]),
            Ok(false) => pages::home(pages::Front::Passkeys, &public_url),
            Err(()) => unavailable(),
        };
    }
    if signed_in(&state, &headers).await {
        see_other("/account", &[])
    } else if state.web.is_some() {
        pages::home(pages::Front::Nomen, &public_url)
    } else {
        see_other("/docs", &[])
    }
}

/// The site's stylesheet. Revalidated on every use (a `304` while it is the same), so a
/// site read again restyles every page at once.
async fn css(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let site = state.hub.config.site();
    let etag = site.stylesheet_etag();
    let current = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|tag| tag.trim() == etag));
    let mut response = if current {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        site.stylesheet().to_string().into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LoginQuery>,
) -> Response {
    if state.passkeys.is_some() {
        return passkeys::login(State(state), headers, Query(query)).await;
    }
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    let (login_state, url) = web.begin(query.return_to.as_deref().unwrap_or("/account"));
    let cookie = web.cookie(web.login_cookie(), &login_state, super::LOGIN_TTL.as_secs());
    see_other(&url, &[cookie])
}

async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    let start_again = Some(("/auth/login", "Sign in again"));
    if let Some(error) = &query.error {
        // OAuth error codes are `[a-z_]`; anything else in the URL is dropped, not shown.
        let error: String = error
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect();
        return pages::message(
            StatusCode::BAD_REQUEST,
            "Sign-in did not finish",
            &format!(
                "{} ended the sign-in ({error}).",
                state.hub.config.site().label("nomen", "Nomen")
            ),
            start_again,
        );
    }
    let (Some(code), Some(returned)) = (query.code, query.state) else {
        return pages::message(
            StatusCode::BAD_REQUEST,
            "Sign-in did not finish",
            "Nomen sent no code back.",
            start_again,
        );
    };
    // The state must be the one this browser started with, and it works once.
    let stale = pages::message(
        StatusCode::BAD_REQUEST,
        "This sign-in expired",
        "This sign-in link expired, was already used, or was started in another browser.",
        start_again,
    );
    if web.login_state(&headers).as_deref() != Some(returned.as_str()) {
        return stale;
    }
    let Some(pending) = web.take(&returned) else {
        return stale;
    };
    let tokens = match web.exchange(&code, &pending).await {
        Ok(tokens) => tokens,
        Err(Exchange::Refused) => return stale,
        Err(Exchange::Unavailable(why)) => {
            crate::log_unavailable(&why);
            return unavailable();
        }
    };
    match state.verifier.verify(&tokens.access_token).await {
        Ok(_) => {}
        Err(AuthError::Invalid(why)) => {
            crate::log_refusal(why);
            return pages::message(
                StatusCode::FORBIDDEN,
                "This sign-in is not accepted here",
                &format!(
                    "Your {} account signed in, but this hub does not accept that sign-in. Ask the hub's owner.",
                    state.hub.config.site().label("nomen", "Nomen")
                ),
                None,
            );
        }
        Err(AuthError::Unavailable(why)) => {
            crate::log_unavailable(&why);
            return unavailable();
        }
    }
    let (id, lasts) = match web.open_session(tokens) {
        Ok(opened) => opened,
        Err(error) => {
            eprintln!("devagotchi-hub: session store: {}", error.message);
            return unavailable();
        }
    };
    see_other(
        &pending.return_to,
        &[
            web.cookie(web.session_cookie(), &id, lasts.as_secs()),
            web.cookie(web.login_cookie(), "", 0),
        ],
    )
}

async fn account(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match who(&state, &headers).await {
        Who::Person(subject) => {
            let machines = state.hub.machine_status(&subject).await;
            // A tenant's own history, on a hub with its own accounts: its chain only.
            let events = match (&state.passkeys, &subject.tenant) {
                (Some(hub), Some(tenant)) => {
                    hub.accounts().events(tenant, 12).await.unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let ways = state
                .passkeys
                .as_ref()
                .map(|hub| hub.linking_ways())
                .unwrap_or_default();
            let linked = match &state.passkeys {
                Some(hub) => hub.accounts().identities_of(&subject.id).await.unwrap_or_default(),
                None => Vec::new(),
            };
            // Early access and plans, on a hosted hub.
            let access = subject
                .access
                .as_ref()
                .filter(|_| state.hub.config.multi_tenant());
            let plan = state.hub.held_plan(&subject);
            pages::account(
                &Viewer {
                    subject: &subject.id,
                    name: subject.name.as_deref(),
                    owner: state.hub.config.is_owner(&subject.id),
                    hub_account: state.passkeys.is_some(),
                    unsecured_until: subject
                        .provisional_until
                        .filter(|_| state.passkeys.is_some()),
                    events: &events,
                    ways: &ways,
                    linked: &linked,
                    access,
                    plan: plan.as_ref().map(|plan| plan.name.as_str()),
                    // The hosted hub's operator: the way to the operator page.
                    operator: state.passkeys.is_some()
                        && state.hub.config.multi_tenant()
                        && state.hub.config.is_owner(&subject.id),
                },
                &machines,
            )
        }
        Who::SignedOut => see_other("/auth/login?return_to=/account", &[]),
        Who::Unavailable => unavailable(),
        Who::NotSetUp => not_set_up(),
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // Another site cannot sign you out, here or in your browser.
    if !super::approvals::same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Sign out from this hub's own page",
            "That sign-out came from another site, so nothing was changed.",
            Some(("/account", "Your account")),
        );
    }
    if let Some(hub) = state.passkeys.clone() {
        hub.end_session(&headers).await;
        return see_other("/signed-out", &[hub.cookie(hub.session_cookie(), "", 0)]);
    }
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    web.end_session(&headers);
    see_other("/signed-out", &[web.cookie(web.session_cookie(), "", 0)])
}

async fn signed_out(State(state): State<AppState>) -> Response {
    let site = state.hub.config.site();
    let text = if state.passkeys.is_some() {
        format!(
            "You are signed out of this {} hub. Your passkey stays on your device for next time.",
            site.name()
        )
    } else {
        let provider = site.label("nomen", "Nomen");
        format!(
            "You are signed out of this {} hub. Your {provider} account stays signed in at {provider}.",
            site.name()
        )
    };
    pages::message(
        StatusCode::OK,
        "You are signed out",
        &text,
        Some(("/auth/login", "Sign in again")),
    )
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let json_error = |status: StatusCode, code: &str| {
        let mut response =
            (status, axum::Json(serde_json::json!({ "error": code }))).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    };
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim);
    let subject = match bearer {
        Some(token) => match state.verifier.verify(token).await {
            Ok(subject) => subject,
            Err(AuthError::Invalid(why)) => {
                crate::log_refusal(why);
                return json_error(StatusCode::UNAUTHORIZED, "invalid_token");
            }
            Err(AuthError::Unavailable(_)) => {
                return json_error(StatusCode::SERVICE_UNAVAILABLE, "issuer_unavailable");
            }
        },
        None => match who(&state, &headers).await {
            Who::Person(subject) => subject,
            Who::SignedOut | Who::NotSetUp => {
                return json_error(StatusCode::UNAUTHORIZED, "signed_out");
            }
            Who::Unavailable => {
                return json_error(StatusCode::SERVICE_UNAVAILABLE, "issuer_unavailable");
            }
        },
    };
    let machines = state.hub.machine_status(&subject).await;
    let access = subject.access.as_ref().map(|access| {
        serde_json::json!({
            "state": if access.waiting { "waiting" } else { "admitted" },
            "waiting_since": access.waiting_since.filter(|_| access.waiting),
            "plan": state.hub.held_plan(&subject).map(|plan| plan.id),
        })
    });
    let mut response = axum::Json(serde_json::json!({
        "subject": subject.id,
        "via": subject.via,
        "name": subject.name,
        "owner": state.hub.config.is_owner(&subject.id),
        "access": access,
        "machines": machines,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
