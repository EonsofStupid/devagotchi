//! The sign-in page of a hosted hub (spec first-connect, A2): Continue with Google (and any
//! other configured provider), or an email and a password (`accounts/passwords.rs`). After
//! a sign-in or sign-up here (or with a provider) the browser carries `dvg_fresh`, the
//! SHA-256 of where it returns to, for five minutes: the authorize request it returns to
//! then gives the AI app its code with no Connect page (`oauth::authorize`).
//!
//! - `POST /auth/password` signs in, `POST /auth/password/new` signs up.
//! - `GET|POST /auth/password/forgot`: the email sign-in link, when the hub sends mail;
//!   it signs in to the account page, whose Set a password (`routes.rs`) sets a new one.

use std::sync::Arc;

use axum::{
    Form, Router,
    extract::{Extensions, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
    routing::post,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{HubSignIn, encode, pages, routes::see_other, safe_return_to};
use crate::AppState;

const FRESH: &str = "dvg_fresh";
const FRESH_SECS: u64 = 300;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    In,
    Up,
}

/// The `dvg_fresh` cookie for a sign-in that returns to `return_to`.
pub(super) fn fresh_cookie(hub: &HubSignIn, return_to: &str) -> String {
    hub.cookie(FRESH, &hex::encode(Sha256::digest(return_to.as_bytes())), FRESH_SECS)
}

/// Whether this request carries a fresh sign-in for `return_to`; and the cookie that ends it.
pub(super) fn fresh(hub: &HubSignIn, headers: &HeaderMap, return_to: &str) -> Option<String> {
    let want = hex::encode(Sha256::digest(return_to.as_bytes()));
    (super::cookie_value(headers, FRESH)? == want).then(|| hub.cookie(FRESH, "", 0))
}

/// The page: the providers' buttons, "or", then one email and password form.
pub fn page(hub: &HubSignIn, return_to: &str, tab: Tab, notice: Option<&str>) -> Response {
    let site = hub.site();
    let words = &site.messages.sign_in;
    let return_to = safe_return_to(return_to);
    let back = encode(return_to);
    let e = pages::escape;
    let buttons = hub.providers.buttons(&site, return_to, false);
    let ways = if buttons.is_empty() {
        String::new()
    } else {
        format!("<p class=\"ways\">{buttons}</p><p class=\"meta\">{}</p>", e(&words.or))
    };
    let (action, submit, complete) = match tab {
        Tab::In => ("/auth/password", &words.submit_in, "current-password"),
        Tab::Up => ("/auth/password/new", &words.submit_up, "new-password"),
    };
    let notice = notice.map(|text| format!("<p class=\"notice\" role=\"alert\">{}</p>", e(text)));
    let body = format!(
        "<h1>{title}</h1>{ways}\
<p class=\"tabs\"><a href=\"/auth/login?tab=in&amp;return_to={back}\">{tab_in}</a> \
<a href=\"/auth/login?tab=up&amp;return_to={back}\">{tab_up}</a></p>{notice}\
<form method=\"post\" action=\"{action}\"><input type=\"hidden\" name=\"return_to\" value=\"{to}\">\
<label>{email} <input type=\"email\" name=\"email\" required autocomplete=\"email\" maxlength=\"254\"></label>\
<label>{password} <input type=\"password\" name=\"password\" required autocomplete=\"{complete}\"></label>\
<p class=\"actions\"><button class=\"button primary\" type=\"submit\">{submit}</button></p></form>\
<p><a href=\"/auth/password/forgot?return_to={back}\">{forgot}</a></p>",
        title = e(&words.title),
        tab_in = e(&words.tab_in),
        tab_up = e(&words.tab_up),
        notice = notice.unwrap_or_default(),
        to = e(return_to),
        email = e(&words.email),
        password = e(&words.password),
        submit = e(submit),
        forgot = e(&words.forgot),
    );
    let mut response = pages::html(StatusCode::OK, &words.title, &body);
    // Its forms post here: the browser sends that POST's true Origin only under `same-origin`.
    let policy = HeaderValue::from_static("same-origin");
    response.headers_mut().insert(header::REFERRER_POLICY, policy);
    response
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/password", post(sign_in))
        .route("/auth/password/new", post(sign_up))
        .route("/auth/password/forgot", axum::routing::get(forgot).post(forgot_sent))
}

#[derive(Deserialize)]
pub struct Typed {
    #[serde(default)]
    email: String,
    password: String,
    #[serde(default)]
    return_to: Option<String>,
}

async fn sign_in(state: State<AppState>, headers: HeaderMap, form: Form<Typed>) -> Response {
    answer(state, headers, form, Tab::In).await
}

async fn sign_up(state: State<AppState>, headers: HeaderMap, form: Form<Typed>) -> Response {
    answer(state, headers, form, Tab::Up).await
}

async fn answer(State(state): State<AppState>, headers: HeaderMap, Form(form): Form<Typed>, tab: Tab) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    let return_to = safe_return_to(form.return_to.as_deref().unwrap_or("/account")).to_string();
    if !super::approvals::same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(StatusCode::FORBIDDEN, "Not from this hub", "Sign in on this hub's own page.", None);
    }
    let accounts = hub.accounts();
    let done = match tab {
        Tab::In => accounts.sign_in_password(&form.email, &form.password).await,
        Tab::Up => accounts.sign_up_password(&form.email, &form.password).await,
    };
    let words = hub.site().messages.sign_in.clone();
    let (status, notice) = match done {
        Ok(account) => {
            let current = hub.who(&headers).await.ok().flatten();
            let fresh = [fresh_cookie(&hub, &return_to)];
            return super::session::open_and_return(&hub, current.as_ref(), &account, &return_to, &fresh).await;
        }
        Err(error) => match error.code.as_str() {
            "slow_down" => (StatusCode::TOO_MANY_REQUESTS, words.slow_down),
            "email_taken" => (StatusCode::BAD_REQUEST, words.taken),
            "password_short" => (StatusCode::BAD_REQUEST, words.short),
            "password_wrong" | "invalid_email" => (StatusCode::BAD_REQUEST, words.wrong),
            "account_disabled" => (StatusCode::FORBIDDEN, error.message),
            _ => {
                eprintln!("devagotchi-hub: account store: {}", error.message);
                return super::routes::unavailable();
            }
        },
    };
    let mut response = page(&hub, &return_to, tab, Some(&notice));
    *response.status_mut() = status;
    response
}

#[derive(Deserialize)]
pub struct Back {
    #[serde(default)]
    return_to: Option<String>,
}

fn reset_unavailable(hub: &HubSignIn) -> Response {
    let words = &hub.site().messages.sign_in;
    pages::message(StatusCode::OK, &words.forgot, &words.reset_unavailable, Some(("/auth/login", &words.title)))
}

/// `GET /auth/password/forgot`: an email sign-in link to the account page, or why not.
async fn forgot(State(state): State<AppState>, Query(_back): Query<Back>) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if hub.providers.email.is_none() {
        return reset_unavailable(&hub);
    }
    let words = &hub.site().messages.sign_in;
    let body = format!(
        "<h1>{}</h1><form method=\"post\" action=\"/auth/password/forgot\"><label>{} \
<input type=\"email\" name=\"email\" required autocomplete=\"email\" maxlength=\"254\"></label>\
<p class=\"actions\"><button class=\"button primary\" type=\"submit\">{}</button></p></form>",
        pages::escape(&words.forgot),
        pages::escape(&words.email),
        pages::escape(&words.submit_in)
    );
    let mut response = pages::html(StatusCode::OK, &words.forgot, &body);
    let policy = HeaderValue::from_static("same-origin");
    response.headers_mut().insert(header::REFERRER_POLICY, policy);
    response
}

#[derive(Deserialize)]
pub struct Forgot {
    email: String,
}

/// `POST /auth/password/forgot`: the email link (`email::ask`) to the account page.
async fn forgot_sent(state: State<AppState>, headers: HeaderMap, extensions: Extensions, Form(form): Form<Forgot>) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if hub.providers.email.is_none() {
        return reset_unavailable(&hub);
    }
    let ask = super::email::Ask { email: form.email, return_to: Some("/account".into()), link: None };
    let sent = super::email::ask(state, headers, extensions, Form(ask)).await;
    if sent.status() != StatusCode::OK {
        return sent;
    }
    let words = &hub.site().messages.sign_in;
    pages::message(StatusCode::OK, &words.forgot, &words.reset_sent, None)
}
