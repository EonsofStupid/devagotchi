//! A hosted hub's sign-in page (spec first-connect, A2): Continue with Google (and any other
//! provider), or email and password (`POST /auth/password`, `/auth/password/new`). A sign-in
//! sets `dvg_fresh`, the SHA-256 of where it returns to, for five minutes: that authorize
//! request then gives the AI app its code with no Connect page (`oauth::authorize`). Forgot
//! password (`/auth/password/forgot`) mails the email sign-in link to the account page.

use axum::{
    Form, Router,
    extract::State,
    http::{Extensions, HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{HubSignIn, encode, pages, safe_return_to};
use crate::AppState;

const FRESH: &str = "dvg_fresh";
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab { In, Up }

fn digest(return_to: &str) -> String { hex::encode(Sha256::digest(return_to.as_bytes())) }

/// The `dvg_fresh` cookie for a sign-in that returns to `return_to` (five minutes).
pub(super) fn fresh_cookie(hub: &HubSignIn, return_to: &str) -> String {
    hub.cookie(FRESH, &digest(return_to), 300)
}

/// When this request carries a fresh sign-in for `return_to`: the cookie that ends it.
pub(super) fn fresh(hub: &HubSignIn, headers: &HeaderMap, return_to: &str) -> Option<String> {
    (super::cookie_value(headers, FRESH)? == digest(return_to)).then(|| hub.cookie(FRESH, "", 0))
}

/// A page whose forms post here: the browser sends a POST's true Origin only under `same-origin`.
fn own_page(title: &str, body: &str) -> Response {
    let mut response = pages::html(StatusCode::OK, title, body);
    response.headers_mut().insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    response
}

/// The page: the providers' buttons, "or", then one email and password form.
pub fn page(hub: &HubSignIn, return_to: &str, tab: Tab, notice: Option<&str>) -> Response {
    let (site, e) = (hub.site(), pages::escape);
    let words = &site.messages.sign_in;
    let return_to = safe_return_to(return_to);
    let back = encode(return_to);
    let buttons = hub.providers.buttons(&site, return_to, false);
    let or = format!("<p class=\"ways\">{buttons}</p><p class=\"meta\">{}</p>", e(&words.or));
    let ways = if buttons.is_empty() { String::new() } else { or };
    let (action, submit, complete) = match tab {
        Tab::In => ("/auth/password", &words.submit_in, "current-password"),
        Tab::Up => ("/auth/password/new", &words.submit_up, "new-password"),
    };
    let notice = notice.map(|text| format!("<p class=\"notice\" role=\"alert\">{}</p>", e(text)));
    let body = format!(
        "<h1>{title}</h1>{ways}<p class=\"tabs\"><a href=\"/auth/login?tab=in&amp;return_to={back}\">{tab_in}</a> \
<a href=\"/auth/login?tab=up&amp;return_to={back}\">{tab_up}</a></p>{notice}\
<form method=\"post\" action=\"{action}\"><input type=\"hidden\" name=\"return_to\" value=\"{to}\">\
<label>{email} <input type=\"email\" name=\"email\" required autocomplete=\"email\" maxlength=\"254\"></label>\
<label>{password} <input type=\"password\" name=\"password\" required autocomplete=\"{complete}\"></label>\
<p class=\"actions\"><button class=\"button primary\" type=\"submit\">{submit}</button></p></form>\
<p><a href=\"/auth/password/forgot?return_to={back}\">{forgot}</a></p>",
        title = e(&words.title), tab_in = e(&words.tab_in), tab_up = e(&words.tab_up),
        notice = notice.unwrap_or_default(), to = e(return_to), email = e(&words.email),
        password = e(&words.password), submit = e(submit), forgot = e(&words.forgot),
    );
    let mut response = own_page(&words.title, &body);
    (super::session::form_may_return(&mut response, return_to), response).1
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/password", post(|s: State<AppState>, h: HeaderMap, f: Form<Typed>| answer(s, h, f, Tab::In)))
        .route("/auth/password/new", post(|s: State<AppState>, h: HeaderMap, f: Form<Typed>| answer(s, h, f, Tab::Up)))
        .route("/auth/password/forgot", get(forgot).post(forgot_sent))
}

#[derive(Deserialize)]
pub struct Typed { #[serde(default)] email: String, password: String, #[serde(default)] return_to: Option<String> }

async fn answer(State(state): State<AppState>, headers: HeaderMap, Form(form): Form<Typed>, tab: Tab) -> Response {
    let Some(hub) = state.passkeys.clone() else { return super::routes::not_set_up() };
    if !super::approvals::same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(StatusCode::FORBIDDEN, "Not from this hub", "Sign in on this hub's own page.", None);
    }
    let return_to = safe_return_to(form.return_to.as_deref().unwrap_or("/account")).to_string();
    let done = match tab {
        Tab::In => hub.accounts().sign_in_password(&form.email, &form.password).await,
        Tab::Up => hub.accounts().sign_up_password(&form.email, &form.password).await,
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
            _ => return { eprintln!("devagotchi-hub: account store: {}", error.message); super::routes::unavailable() },
        },
    };
    let mut response = page(&hub, &return_to, tab, Some(&notice));
    *response.status_mut() = status;
    response
}

/// The hub's sign-in when it sends mail; else the page saying a reset isn't available.
fn mailing(state: &AppState) -> Result<std::sync::Arc<HubSignIn>, Response> {
    let hub = state.passkeys.clone().ok_or_else(super::routes::not_set_up)?;
    if hub.providers.email.is_some() {
        return Ok(hub);
    }
    let words = &hub.site().messages.sign_in;
    Err(pages::message(StatusCode::OK, &words.forgot, &words.reset_unavailable, Some(("/auth/login", &words.title))))
}

/// `GET /auth/password/forgot`: ask for the email sign-in link, or say why not.
async fn forgot(State(state): State<AppState>) -> Response {
    let hub = match mailing(&state) { Ok(hub) => hub, Err(page) => return page };
    let (words, e) = (&hub.site().messages.sign_in, pages::escape);
    let body = format!(
        "<h1>{}</h1><form method=\"post\" action=\"/auth/password/forgot\"><label>{} <input type=\"email\" \
name=\"email\" required autocomplete=\"email\" maxlength=\"254\"></label>\
<p class=\"actions\"><button class=\"button primary\" type=\"submit\">{}</button></p></form>",
        e(&words.forgot), e(&words.email), e(&words.submit_in)
    );
    own_page(&words.forgot, &body)
}

#[derive(Deserialize)]
pub struct Forgot { email: String }

/// `POST /auth/password/forgot`: the email link (`email::ask`) to the account page.
async fn forgot_sent(state: State<AppState>, headers: HeaderMap, extensions: Extensions, Form(form): Form<Forgot>) -> Response {
    let hub = match mailing(&state) { Ok(hub) => hub, Err(page) => return page };
    let ask = super::email::Ask { email: form.email, return_to: Some("/account".into()), link: None };
    let sent = super::email::ask(state, headers, extensions, Form(ask)).await;
    let words = &hub.site().messages.sign_in;
    match sent.status() {
        StatusCode::OK => pages::message(StatusCode::OK, &words.forgot, &words.reset_sent, None),
        _ => sent,
    }
}
