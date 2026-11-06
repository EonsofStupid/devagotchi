//! `/offers/<token>`: the one-tap link `offers_accept` hands an AI when the hub cannot link
//! the account to the network by itself (`crate::offers`). The link is the credential, like a
//! workflow's: random, kept only as its SHA-256, for one tenant, short-lived, and replaced by
//! a later yes.
//!
//! - A GET shows what the button does and spends nothing (a chat app's preview, a mail
//!   scanner). Once the account is linked it says so, and the AI's `offers` says linked.
//! - The button is a POST from this page (`same_origin`). For an account nobody secured yet
//!   it opens a session of the account in this browser (on the tenant's chain); a secured one
//!   signs in first and comes back. Then on to the network's sign-in
//!   (`/auth/provider/<shippin.provider>?link=1`), which links it to the account the way the
//!   account page does, and back here.
//!
//! Every word is the site's (`site.json` `offers.link`).

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};

use super::{
    approvals::same_origin,
    pages::{self, escape},
    routes::{see_other, unavailable},
};
use crate::{AppState, accounts::sessions::SESSION_MAX_SECS, devices::now_secs, site::Ctx};

fn path(token: &str) -> String {
    format!("/offers/{token}")
}

/// A page of the site's link words: `text` (inline markup) and, while it works, the button.
fn page(status: StatusCode, text: &str, button: Option<&str>) -> Response {
    let site = pages::site();
    let ctx = Ctx::new(&site);
    let Some(offers) = site.info.offers() else {
        return not_found();
    };
    let link = &offers.link;
    let title = ctx.plain(&link.title);
    let form = button
        .map(|action| {
            format!(
                "<form method=\"post\" action=\"{}\"><p><button class=\"button primary\" \
type=\"submit\">{}</button></p></form>",
                escape(action),
                ctx.inline(&link.button)
            )
        })
        .unwrap_or_default();
    let mut response = pages::html(
        status,
        &title,
        &format!(
            "<h1>{}</h1><p>{}</p>{form}",
            ctx.inline(&link.title),
            ctx.inline(text)
        ),
    );
    // Its form posts here: the browser sends a POST's true Origin only under `same-origin`.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

fn not_found() -> Response {
    pages::message(
        StatusCode::NOT_FOUND,
        "Not found",
        "There is no such page.",
        None,
    )
}

fn gone() -> Response {
    let site = pages::site();
    match site.info.offers() {
        Some(offers) => page(StatusCode::NOT_FOUND, &offers.link.gone, None),
        None => not_found(),
    }
}

/// `GET /offers/<token>`: what the button does, or that it is done.
pub async fn show(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let Ok(engine) = state.hub.offers_engine() else {
        return not_found();
    };
    let now = now_secs();
    let row = match engine.accounts().offer_by_link(&token).await {
        Ok(Some(row)) => row,
        Ok(None) => return gone(),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return unavailable();
        }
    };
    // Linked since (a sign-in through this page, or any other way): done.
    let row = match engine.settle(row, now).await {
        Ok(row) => row,
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return unavailable();
        }
    };
    let site = pages::site();
    let Some(offers) = site.info.offers() else {
        return not_found();
    };
    if row.linked() {
        return page(StatusCode::OK, &offers.link.done, None);
    }
    if !row.live(now) {
        return gone();
    }
    page(StatusCode::OK, &offers.link.text, Some(&path(&token)))
}

/// `POST /offers/<token>`: this browser on the account, then on to the network's sign-in.
pub async fn go(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    let (Ok(engine), Some(hub)) = (state.hub.offers_engine(), &state.passkeys) else {
        return not_found();
    };
    if !same_origin(&headers, &state.hub.config.public_url) {
        return see_other(&path(&token), &[]);
    }
    let now = now_secs();
    let row = match engine.accounts().offer_by_link(&token).await {
        Ok(Some(row)) => row,
        Ok(None) => return gone(),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return unavailable();
        }
    };
    if row.linked() {
        return see_other(&path(&token), &[]);
    }
    if !row.live(now) || !engine.can_link() {
        return gone();
    }
    let sign_in = format!(
        "/auth/provider/{}?return_to={}&link=1",
        super::encode(engine.provider()),
        super::encode(&path(&token))
    );
    // Signed in to the account already: straight on.
    if let Ok(Some(subject)) = hub.who(&headers).await
        && subject.id == row.account
    {
        return see_other(&sign_in, &[]);
    }
    match engine.accounts().offer_session(&token, now).await {
        Ok(session) => see_other(
            &sign_in,
            &[hub.cookie(hub.session_cookie(), &session, SESSION_MAX_SECS)],
        ),
        // A secured account signs in first, then comes back here.
        Err(error) if error.code == "secured" => see_other(
            &format!("/auth/login?return_to={}", super::encode(&path(&token))),
            &[],
        ),
        Err(error) if matches!(error.code.as_str(), "expired" | "account_disabled") => gone(),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            unavailable()
        }
    }
}
