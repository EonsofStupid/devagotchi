//! The browser routes: the landing page and claiming a new hub (`/claim`), sign in, the
//! account page, sign out, `/api/v1/me` (which also takes a bearer token, so scripts and AI
//! clients see the same view), `/approvals`, and the public pages the AI directories link to
//! (`/docs`, `/privacy`, `/terms`, `/support`).

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

use super::{
    Exchange, SessionToken, WebLogin, approvals, claim, machines, notify, oauth,
    pages::{self, Viewer},
    passkeys, public,
};
use crate::{
    AppState,
    auth::{AuthError, Subject},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/account", get(account))
        .route("/signed-out", get(signed_out))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .route("/owner", get(passkeys::owner_page))
        .route("/claim", get(claim::page).post(claim::claim))
        .route(
            "/passkey/register/options",
            post(passkeys::register_options),
        )
        .route("/passkey/register", post(passkeys::register))
        .route("/passkey/sign-in/options", post(passkeys::sign_in_options))
        .route("/passkey/sign-in", post(passkeys::sign_in))
        .route("/assets/passkey.js", get(passkeys::script))
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
        .route("/api/v1/me", get(me))
        .route("/api/v1/machines", post(machines::add_json))
        .route("/approvals", get(approvals::list))
        .route("/approvals/decide", post(approvals::decide))
        .route("/approvals/test", post(approvals::test))
        .route("/machines/add", post(machines::add))
        .route("/machines/revoke", post(machines::revoke))
        .route("/push/subscribe", post(notify::subscribe))
        .route("/sw.js", get(notify::service_worker))
        .route("/manifest.webmanifest", get(notify::manifest))
        .route("/assets/push.js", get(notify::script))
        .route("/assets/icon.svg", get(notify::icon))
        .route("/docs", get(public::docs))
        .route("/privacy", get(public::privacy))
        .route("/terms", get(public::terms))
        .route("/support", get(public::support))
        .route("/assets/hub.css", get(css))
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

/// Signed in, `/` is your machines; signed out, it says what Devagotchi is and offers
/// one Sign in, instead of sending a stranger straight to an identity provider.
/// `/`: the landing page. A hub nobody has claimed offers the claim; one with an owner
/// says where AIs connect and offers sign-in; a signed-in person goes to their account.
async fn root(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let public_url = state.hub.config.public_url.clone();
    if let Some(hub) = &state.passkeys {
        return match hub.claimed().await {
            Ok(false) => pages::home(pages::Front::Unclaimed, &public_url),
            Ok(true) if signed_in(&state, &headers).await => see_other("/account", &[]),
            Ok(true) => pages::home(pages::Front::Passkeys, &public_url),
            Err(()) => unavailable(),
        };
    }
    if signed_in(&state, &headers).await {
        see_other("/account", &[])
    } else if state.web.is_some() {
        pages::home(pages::Front::Shippin, &public_url)
    } else {
        see_other("/docs", &[])
    }
}

async fn css() -> Response {
    let mut response = pages::CSS.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
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
            &format!("Shippin ended the sign-in ({error})."),
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
                "Your Shippin account signed in, but this hub does not accept that sign-in. Ask the hub's owner.",
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
            let machines = state.hub.machine_status(&subject.id).await;
            pages::account(
                &Viewer {
                    subject: &subject.id,
                    name: subject.name.as_deref(),
                    owner: state.hub.config.is_owner(&subject.id),
                    hub_account: state.passkeys.is_some(),
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
    let text = if state.passkeys.is_some() {
        "You are signed out of this Devagotchi hub. Your passkey stays on your device for next time."
    } else {
        "You are signed out of this Devagotchi hub. Your Shippin account stays signed in at Shippin."
    };
    pages::message(
        StatusCode::OK,
        "You are signed out",
        text,
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
    let machines = state.hub.machine_status(&subject.id).await;
    let mut response = axum::Json(serde_json::json!({
        "subject": subject.id,
        "via": subject.via,
        "name": subject.name,
        "owner": state.hub.config.is_owner(&subject.id),
        "machines": machines,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
