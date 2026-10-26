//! The browser routes: sign in, the account page, sign out, `/api/v1/me` (which also
//! takes a bearer token, so scripts and AI clients see the same view), `/approvals`, and the public
//! pages the AI directories link to (`/docs`, `/privacy`, `/terms`, `/support`).

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

use super::{
    Exchange, SessionToken, WebLogin, approvals, machines, notify,
    pages::{self, Viewer},
    public,
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
        .route("/api/v1/me", get(me))
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
struct LoginQuery {
    return_to: Option<String>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Who the browser's session belongs to, checked against Nomen like any bearer token.
pub(super) enum Who {
    Person(Subject),
    SignedOut,
    Unavailable,
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
        "Shippin sign-in is unavailable",
        "The hub could not reach Shippin's sign-in service. Your access has not changed; try again in a minute.",
        Some(("/account", "Try again")),
    )
}

pub(super) async fn who(state: &AppState, web: &WebLogin, headers: &HeaderMap) -> Who {
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
async fn root(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match &state.web {
        Some(web) if web.signed_in(&headers) => see_other("/account", &[]),
        Some(_) => pages::home(),
        None => see_other("/docs", &[]),
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

async fn login(State(state): State<AppState>, Query(query): Query<LoginQuery>) -> Response {
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
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    match who(&state, &web, &headers).await {
        Who::Person(subject) => {
            let machines = state.hub.machine_status(&subject.id).await;
            pages::account(
                &Viewer {
                    subject: &subject.id,
                    name: subject.name.as_deref(),
                    owner: state.hub.config.is_owner(&subject.id),
                },
                &machines,
            )
        }
        Who::SignedOut => see_other("/auth/login?return_to=/account", &[]),
        Who::Unavailable => unavailable(),
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    web.end_session(&headers);
    see_other("/signed-out", &[web.cookie(web.session_cookie(), "", 0)])
}

async fn signed_out() -> Response {
    pages::message(
        StatusCode::OK,
        "You are signed out",
        "You are signed out of this Devagotchi hub. Your Shippin account stays signed in at Shippin.",
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
    let subject = match (bearer, state.web.clone()) {
        (Some(token), _) => match state.verifier.verify(token).await {
            Ok(subject) => subject,
            Err(AuthError::Invalid(why)) => {
                crate::log_refusal(why);
                return json_error(StatusCode::UNAUTHORIZED, "invalid_token");
            }
            Err(AuthError::Unavailable(_)) => {
                return json_error(StatusCode::SERVICE_UNAVAILABLE, "issuer_unavailable");
            }
        },
        (None, Some(web)) => match who(&state, &web, &headers).await {
            Who::Person(subject) => subject,
            Who::SignedOut => return json_error(StatusCode::UNAUTHORIZED, "signed_out"),
            Who::Unavailable => {
                return json_error(StatusCode::SERVICE_UNAVAILABLE, "issuer_unavailable");
            }
        },
        (None, None) => return json_error(StatusCode::UNAUTHORIZED, "signed_out"),
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
