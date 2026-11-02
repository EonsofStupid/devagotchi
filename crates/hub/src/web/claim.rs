//! `/claim`: a new hub claimed from a browser (16c). Whoever has the claim code the hub
//! printed to its log names the owner and makes the owner's passkey on one page; the code
//! and why it is a code are in `accounts/claim.rs`.
//!
//! - The page takes the code from `/claim#<code>` (the link the log prints), which a browser
//!   never sends, so it stays out of every request line and log; or it is typed in.
//! - `POST /claim` answers only this hub's own page (the same origin checks as the passkey
//!   endpoints), and answers a right code with the passkey's registration options and the
//!   owner link they are for, so the browser asks for the passkey straight away.
//! - Nothing a claim makes signs anyone in: the session comes with the passkey.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

use super::{pages, passkeys};
use crate::{AppState, audit::AuditEntry};

/// The name an owner gets when the claimant leaves it empty.
const DEFAULT_NAME: &str = "Owner";

/// `GET /claim`: the claim form on a hub nobody owns yet; a pointer to sign-in on one that
/// has an owner.
pub async fn page(State(state): State<AppState>) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    match hub.claimed().await {
        Ok(false) => passkeys::scripted("Claim this hub", FORM),
        Ok(true) => pages::message(
            StatusCode::OK,
            "This hub has an owner",
            "Its owner claimed it already. If that is you, sign in with your passkey.",
            Some(("/auth/login?return_to=/account", "Sign in")),
        ),
        Err(()) => super::routes::unavailable(),
    }
}

const FORM: &str = "<h1>Claim this hub</h1>\
<p>This hub has no owner yet. Whoever claims it owns it: they reach every machine on it and \
approve every AI that connects to it.</p>\
<p>Claiming takes the code the hub printed in its log when it started. With Docker Compose, \
on the hub's host: <code>docker compose logs hub</code>. Opening the link printed with it \
fills the code in here.</p>\
<form id=\"claim-form\">\
<label>Claim code <input id=\"claim-code\" type=\"text\" required autocomplete=\"off\" \
autocapitalize=\"characters\" spellcheck=\"false\" maxlength=\"64\" \
placeholder=\"XXXX-XXXX-XXXX-XXXX-XXXX-XXXX\"></label>\
<label>Your name <input id=\"claim-name\" type=\"text\" maxlength=\"64\" autocomplete=\"name\" \
placeholder=\"Owner\"></label>\
<p class=\"actions\"><button class=\"button primary\" id=\"passkey-claim\" type=\"submit\" disabled>\
Claim this hub and make my passkey</button></p></form>\
<p class=\"meta\" id=\"passkey-status\" role=\"status\"></p>\
<p class=\"meta\">Your passkey is how you sign in from now on: your phone's or computer's \
fingerprint, face or PIN, and your passkey manager can share it with your other devices. \
The code stops working once you have made it. If the code has expired, restarting the hub \
prints a new one.</p>\
<noscript><p>Making a passkey needs JavaScript for this page.</p></noscript>";

#[derive(Deserialize)]
pub struct ClaimIn {
    code: String,
    #[serde(default)]
    name: Option<String>,
}

/// `POST /claim`: a right code makes the owner (once) and answers with the registration
/// options for their passkey and the owner link they are for.
pub async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: axum::body::Bytes,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !hub.from_own_page(&headers) {
        return passkeys::not_own_page();
    }
    let input: ClaimIn = match passkeys::body(&headers, &raw) {
        Ok(input) => input,
        Err(refused) => return refused,
    };
    let name = input
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_NAME.to_string());
    let configured = state.hub.config.owner_id().map(str::to_string);
    let accounts = hub.accounts().clone();
    let done = tokio::spawn(async move {
        accounts
            .claim(&input.code, configured.as_deref(), &name)
            .await
    })
    .await;
    match done {
        Ok(Ok((owner, link))) => {
            // A claim is on the audit chain, or it is refused: the link it made is never
            // handed out, and the next claim supersedes it.
            let recorded = state.hub.audit.record(&AuditEntry {
                subject: &owner.id,
                via: "claim",
                tool: "account.claim",
                broker: "",
                operation_id: "",
                outcome: "ok",
            });
            if let Err(error) = recorded {
                return passkeys::store_failed(&error.message);
            }
            eprintln!(
                "devagotchi-hub: claimed by {} ({}); making their passkey",
                owner.name, owner.id
            );
            passkeys::registration(hub, link, true).await
        }
        Ok(Err(error)) => match error.code.as_str() {
            "wrong_code" => passkeys::refuse(
                StatusCode::FORBIDDEN,
                "wrong_code",
                "That is not this hub's claim code. Use the newest one in the hub's log: it prints a new one each time it starts.",
            ),
            "claimed" => passkeys::refuse(
                StatusCode::CONFLICT,
                "claimed",
                "This hub already has an owner. If that is you, sign in with your passkey.",
            ),
            "invalid_account" => passkeys::refuse(
                StatusCode::BAD_REQUEST,
                "invalid_name",
                "A name is 1 to 64 characters on one line.",
            ),
            "owner_mismatch" => {
                eprintln!(
                    "devagotchi-hub: a claim was refused: {}; the config's owner must be the owner the hub's accounts hold",
                    error.message
                );
                passkeys::refuse(
                    StatusCode::CONFLICT,
                    "owner_mismatch",
                    "This hub's config names another owner than its accounts hold. Fix the config's owner on the hub's host.",
                )
            }
            _ => passkeys::store_failed(&error.message),
        },
        Err(_) => passkeys::store_failed("a claim did not complete"),
    }
}
