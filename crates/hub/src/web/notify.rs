//! Turning a phone into an approval notifier: the service worker, the one page script,
//! the app manifest and icon, and `POST /push/subscribe`, where a signed-in person's
//! browser leaves the address and public keys the hub pushes to (`crate::push`).

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;

use super::{
    approvals::same_origin,
    routes::{Who, who},
};
use crate::AppState;

fn asset(body: &'static str, content_type: &'static str, cache: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub async fn service_worker() -> Response {
    asset(
        include_str!("assets/sw.js"),
        "text/javascript; charset=utf-8",
        "no-cache",
    )
}

pub async fn script() -> Response {
    asset(
        include_str!("assets/push.js"),
        "text/javascript; charset=utf-8",
        "no-cache",
    )
}

pub async fn manifest() -> Response {
    asset(
        include_str!("assets/manifest.webmanifest"),
        "application/manifest+json",
        "public, max-age=3600",
    )
}

pub async fn icon() -> Response {
    asset(
        include_str!("assets/icon.svg"),
        "image/svg+xml",
        "public, max-age=86400",
    )
}

#[derive(Deserialize)]
pub struct Keys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
pub struct Subscription {
    endpoint: String,
    keys: Keys,
}

fn plain(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

pub async fn subscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(sub): Json<Subscription>,
) -> Response {
    let Some(push) = state.hub.push.clone() else {
        return plain(StatusCode::NOT_FOUND, "push_not_set_up");
    };
    if !same_origin(&headers, &state.hub.config.public_url) {
        return plain(StatusCode::FORBIDDEN, "not_same_origin");
    }
    let subject = match who(&state, &headers).await {
        Who::Person(subject) => subject,
        Who::SignedOut => return plain(StatusCode::UNAUTHORIZED, "signed_out"),
        Who::NotSetUp => return plain(StatusCode::NOT_FOUND, "push_not_set_up"),
        Who::Unavailable => return plain(StatusCode::SERVICE_UNAVAILABLE, "issuer_unavailable"),
    };
    if !state.hub.config.is_human(&subject.id) {
        return plain(StatusCode::FORBIDDEN, "not_a_person");
    }
    let key_ok = URL_SAFE_NO_PAD
        .decode(sub.keys.p256dh.trim_end_matches('='))
        .is_ok_and(|k| k.len() == 65);
    let auth_ok = URL_SAFE_NO_PAD
        .decode(sub.keys.auth.trim_end_matches('='))
        .is_ok_and(|a| a.len() == 16);
    let endpoint_ok = sub.endpoint.len() <= 2048
        && reqwest::Url::parse(&sub.endpoint).is_ok_and(|u| u.scheme() == "https" || cfg!(test));
    if !key_ok || !auth_ok || !endpoint_ok {
        return plain(StatusCode::BAD_REQUEST, "bad_subscription");
    }
    let devices = push.devices();
    match devices.add_push(
        &subject.id,
        &sub.endpoint,
        sub.keys.p256dh.trim_end_matches('='),
        sub.keys.auth.trim_end_matches('='),
    ) {
        Ok(()) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(_) => plain(StatusCode::INTERNAL_SERVER_ERROR, "store"),
    }
}
