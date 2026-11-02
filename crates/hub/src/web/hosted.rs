//! The way into a hosted hub (`tenancy.mode multi`): nobody claims it, and nobody fills in a
//! form. The first time someone connects an AI client (`/oauth/authorize`) or the CLI
//! (`/device`) with no session, the page they already have to see, Connect or Approve, is
//! also where their account and its own tenant are made, with one click
//! (`accounts/tenants.rs`). Their AI is working within seconds.
//!
//! Securing it never blocks that: the account page, every page's bar, the device page's
//! answer and `whoami` (`secure_url`) offer it afterwards, and say plainly that until then
//! the account lives in that browser and the apps it approved, and nothing can recover it.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::ConnectInfo,
    http::{Extensions, HeaderMap, StatusCode},
    response::Response,
};

use super::{HubSignIn, pages};
use crate::{AppState, accounts::sessions::SESSION_MAX_SECS, auth::Subject};

/// The address a request came from, as `tenancy.trusted_proxies` lets the hub believe it.
pub(super) fn client_address(
    state: &AppState,
    extensions: &Extensions,
    headers: &HeaderMap,
) -> IpAddr {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok());
    state.hub.config.tenancy.client_address(peer, forwarded)
}

/// Make the account and tenant of someone connecting `client` with no session, and open
/// the session this browser keeps it by: who they are now, and the session's cookie. Or
/// the page to show instead (too many new accounts from their address, or the hub full).
pub(super) async fn start(
    hub: &Arc<HubSignIn>,
    address: IpAddr,
    client: &str,
) -> Result<(Subject, String), Response> {
    let accounts = hub.accounts().clone();
    let client = client.to_string();
    let made = tokio::spawn(async move {
        let (account, token) = accounts.start_unsecured(address, &client).await?;
        Ok::<_, devagotchi_contracts::BridgeError>((accounts.subject(&account, "passkey"), token))
    })
    .await;
    match made {
        Ok(Ok((subject, token))) => {
            eprintln!(
                "devagotchi-hub: made account {} in tenant {} on first connect",
                subject.id,
                subject.tenant.as_deref().unwrap_or("?")
            );
            Ok((
                subject,
                hub.cookie(hub.session_cookie(), &token, SESSION_MAX_SECS),
            ))
        }
        Ok(Err(error)) if error.code == "rate_limited" => Err(pages::message(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many new accounts",
            "Too many accounts were made from your network in the last hour. Try again later, or sign in to the account you made.",
            Some(("/auth/login?return_to=/account", "Sign in")),
        )),
        Ok(Err(error)) if error.code == "capacity" => Err(pages::message(
            StatusCode::SERVICE_UNAVAILABLE,
            "Not making new accounts right now",
            "This hub is full for the moment. Try again later.",
            None,
        )),
        Ok(Err(error)) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            Err(super::routes::unavailable())
        }
        Err(_) => Err(super::routes::unavailable()),
    }
}

/// What the Connect or Approve page says to someone with no account yet.
pub(super) fn new_account_note(sign_in: &str) -> String {
    format!(
        "<p class=\"note\">You have no account here yet: pressing it makes yours, with a space of its own \
and nothing to fill in. Secure it afterwards with a passkey on your account page. Until you do, it \
lives in this browser and the apps you connect, and nothing can recover it.</p>\
<p class=\"meta\">Have an account already? <a href=\"{}\">Sign in</a> first.</p>",
        pages::escape(sign_in)
    )
}

/// The offer to secure an account nobody has secured yet, on the account page and the
/// device page's answer: never in the way, always there.
pub(super) fn secure_offer(lapses_at: u64, with_button: bool) -> String {
    let button = if with_button {
        "<p><button class=\"button primary\" id=\"passkey-add\" disabled>Make a passkey</button></p>\
<p class=\"meta\" id=\"passkey-status\" role=\"status\"></p>\
<noscript><p>Making a passkey needs JavaScript for this page.</p></noscript>"
            .to_string()
    } else {
        "<p><a class=\"button primary\" href=\"/account#secure\">Secure my account</a></p>"
            .to_string()
    };
    format!(
        "<section class=\"secure\" id=\"secure\" aria-labelledby=\"secure-title\">\
<h2 id=\"secure-title\">Secure your account</h2>\
<p>Your account is not secured yet. It lives in this browser and the AI apps you connected; if you \
lose them, nothing can recover it. Unused, it ends on {until}.</p>{button}\
<p class=\"meta\">A passkey is your phone's or computer's fingerprint, face or PIN. Signing in with \
Google, GitHub, an email link or Shippin is coming to this page.</p></section>",
        until = pages::utc(lapses_at),
    )
}

/// `/claim` on a hosted hub: there is nothing to claim.
pub(super) fn nothing_to_claim() -> Response {
    pages::message(
        StatusCode::NOT_FOUND,
        "Nothing to claim here",
        "This is a hosted hub: nobody claims it. Your account is made the first time your AI connects to it.",
        Some(("/", "Get started")),
    )
}
