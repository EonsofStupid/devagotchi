//! Device authorization (RFC 8628) on a hub with its own accounts: `devagotchi login`
//! signing a terminal in. The store side is `accounts/oauth/device.rs`.
//!
//! - `POST /oauth/device` (the metadata's `device_authorization_endpoint`): a client asks
//!   for a device code and a user code. The CLI's built-in client is `devagotchi-cli`; any
//!   client registered at `/oauth/register` may ask too.
//! - `GET /device`: the person, signed in with their passkey (or sent to sign in first and
//!   brought back), enters or follows the code, sees which app asks and as whom, and checks
//!   the code matches their terminal (RFC 8628 §5.4).
//! - `POST /device`: their answer, from this page alone (`Origin` and `Sec-Fetch-Site`, as
//!   the consent page). The terminal's next poll at `/oauth/token` gets its tokens.

use axum::{
    Form, Json,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use super::{
    encode,
    oauth::{allow_any_origin, oauth_error, resource_for, scope_for},
    pages,
    routes::see_other,
};
use crate::{AppState, accounts::oauth::normalize_user_code};

/// The CLI's own client id: `devagotchi login` needs no registration.
pub const CLI_CLIENT: &str = "devagotchi-cli";

/// A client's name as the person sees it: the CLI's, or a registered client's. `Ok(None)`
/// for a client this hub does not know.
pub(super) async fn client_name(state: &AppState, id: &str) -> Result<Option<String>, ()> {
    if id == CLI_CLIENT {
        // The terminal sign-in, by the site's name for it.
        return Ok(Some(format!("{} CLI", state.hub.config.site().name())));
    }
    let Some(hub) = state.passkeys.clone() else {
        return Ok(None);
    };
    match hub.accounts().client(id).await {
        Ok(found) => Ok(found.map(|client| client.name)),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            Err(())
        }
    }
}

#[derive(Deserialize)]
pub struct DeviceRequest {
    client_id: Option<String>,
    scope: Option<String>,
    resource: Option<String>,
}

/// RFC 8628 §3.1–3.2: a device code for a terminal, and the code its person types.
pub async fn start(State(state): State<AppState>, Form(asked): Form<DeviceRequest>) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return oauth_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no authorization server",
        );
    };
    let Some(client) = asked.client_id.as_deref() else {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "client_id is required",
        );
    };
    match client_name(&state, client).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return oauth_error(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "register at /oauth/register first, or use devagotchi-cli",
            );
        }
        Err(()) => return store_failed(),
    }
    let Some(resource) = resource_for(&state, asked.resource.as_deref()) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "this hub issues tokens for its own /mcp only",
        );
    };
    let scope = scope_for(asked.scope.as_deref());
    match hub.accounts().start_device(client, &scope, &resource).await {
        Ok(started) => {
            let base = state.hub.config.public_url.trim_end_matches('/');
            let mut response = Json(json!({
                "device_code": started.device_code,
                "user_code": started.user_code,
                "verification_uri": format!("{base}/device"),
                "verification_uri_complete": format!("{base}/device?code={}", started.user_code),
                "expires_in": started.expires_in,
                "interval": started.interval,
            }))
            .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            allow_any_origin(&mut response);
            response
        }
        Err(error) if error.code == "slow_down" => {
            oauth_error(StatusCode::TOO_MANY_REQUESTS, "slow_down", &error.message)
        }
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            store_failed()
        }
    }
}

fn store_failed() -> Response {
    oauth_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "temporarily_unavailable",
        "the hub could not reach its store; try again",
    )
}

#[derive(Deserialize)]
pub struct DeviceQuery {
    code: Option<String>,
}

fn not_valid() -> Response {
    pages::message(
        StatusCode::BAD_REQUEST,
        "This code is not valid",
        "It is mistyped, expired or already used. Run devagotchi login again for a new one.",
        Some(("/device", "Enter another code")),
    )
}

/// A page whose form posts back here: the browser sends that POST's true `Origin` only
/// under `same-origin` (under the pages' `no-referrer` it sends `Origin: null`).
fn with_form(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

/// The verification page: enter the code, or check the one followed and approve.
pub async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DeviceQuery>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    let typed = query
        .code
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let back = match typed {
        Some(code) => format!("/device?code={}", encode(code)),
        None => "/device".to_string(),
    };
    let sign_in = format!("/auth/login?return_to={}", encode(&back));
    // On a hosted hub, someone with no session gets the same page: Approve makes their
    // account (`hosted.rs`).
    let person = match hub.who(&headers).await {
        Ok(Some(person)) => Some(person),
        Ok(None) if state.hub.config.multi_tenant() => None,
        Ok(None) => return see_other(&sign_in, &[]),
        Err(()) => return super::routes::unavailable(),
    };
    let Some(typed) = typed else {
        return pages::html(
            StatusCode::OK,
            "Sign in a terminal",
            "<h1>Sign in a terminal</h1>\
<p>Enter the code your terminal shows after <code>devagotchi login</code>.</p>\
<form method=\"get\" action=\"/device\"><p><label>Code \
<input name=\"code\" autocomplete=\"off\" autocapitalize=\"characters\" spellcheck=\"false\" \
placeholder=\"XXXX-XXXX\" maxlength=\"16\" required></label> \
<button class=\"button primary\">Continue</button></p></form>",
        );
    };
    let Some(code) = normalize_user_code(typed) else {
        return not_valid();
    };
    let pending = match hub.accounts().pending_device(&code).await {
        Ok(Some(pending)) => pending,
        Ok(None) => return not_valid(),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let app = match client_name(&state, &pending.client).await {
        Ok(Some(name)) => name,
        Ok(None) => return not_valid(),
        Err(()) => return super::routes::unavailable(),
    };
    let shown = format!("{}-{}", &code[..4], &code[4..]);
    let body = format!(
        "<h1>Sign in {app}?</h1>\
<p><strong>{app}</strong> on another device asks to use this hub as <strong>{who}</strong>: every tool \
on the machines you reach, until you sign it out or it goes unused for {days} days.</p>\
<p>Continue only if your terminal shows <strong>{shown}</strong>. Never approve a code someone else gave you.</p>\
<form method=\"post\" action=\"/device\"><input type=\"hidden\" name=\"code\" value=\"{shown}\">\
<p><button class=\"button primary\" name=\"decision\" value=\"approve\">Approve</button> \
<button class=\"button\" name=\"decision\" value=\"deny\">Cancel</button></p></form>{new}",
        app = pages::escape(&app),
        who = match &person {
            Some(person) => pages::escape(person.name.as_deref().unwrap_or(&person.id)),
            None => format!(
                "your new {} account",
                pages::escape(state.hub.config.site().name())
            ),
        },
        new = if person.is_none() {
            super::hosted::new_account_note(&sign_in)
        } else {
            String::new()
        },
        days = state
            .hub
            .config
            .token_config()
            .refresh_secs
            .div_ceil(86_400),
    );
    with_form(pages::html(StatusCode::OK, "Sign in a terminal", &body))
}

#[derive(Deserialize)]
pub struct DeviceDecision {
    code: String,
    decision: String,
}

/// The person's answer, from the verification page only. On a hosted hub, Approve from
/// someone with no session makes their account first and signs this browser in to it.
pub async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    Form(form): Form<DeviceDecision>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !hub.from_own_page(&headers) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not from this hub's page",
            "Approve a terminal on this hub's own page.",
            None,
        );
    }
    let approve = form.decision == "approve";
    let mut cookies = Vec::new();
    let person = match hub.who(&headers).await {
        Ok(Some(person)) => person,
        Ok(None) if state.hub.config.multi_tenant() && approve => {
            // Only a code that waits makes an account.
            let waiting = match normalize_user_code(&form.code) {
                Some(code) => hub.accounts().pending_device(&code).await,
                None => Ok(None),
            };
            let pending = match waiting {
                Ok(Some(pending)) => pending,
                Ok(None) => return not_valid(),
                Err(error) => {
                    eprintln!("devagotchi-hub: account store: {}", error.message);
                    return super::routes::unavailable();
                }
            };
            let app = client_name(&state, &pending.client)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "a terminal".to_string());
            let address = super::hosted::client_address(&state, &extensions, &headers);
            match super::hosted::start(&hub, address, &app).await {
                Ok((person, cookie)) => {
                    cookies.push(cookie);
                    person
                }
                Err(page) => return page,
            }
        }
        Ok(None) if state.hub.config.multi_tenant() => {
            return pages::message(
                StatusCode::OK,
                "Cancelled",
                "Your terminal was not signed in.",
                None,
            );
        }
        Ok(None) => {
            let back = format!("/device?code={}", encode(&form.code));
            return see_other(&format!("/auth/login?return_to={}", encode(&back)), &[]);
        }
        Err(()) => return super::routes::unavailable(),
    };
    match hub
        .accounts()
        .decide_device(&form.code, &person.id, approve)
        .await
    {
        Ok(true) if approve => {
            if let Some(tenant) = &person.tenant {
                let _ = hub
                    .accounts()
                    .record(tenant, &person.id, "terminal.connected", "devagotchi login")
                    .await;
            }
            let offer = person
                .provisional_until
                .map(|until| super::hosted::secure_offer(until, false, ""))
                .unwrap_or_default();
            let mut page = pages::html(
                StatusCode::OK,
                "Your terminal is signed in",
                &format!(
                    "<h1>Your terminal is signed in</h1><p>Go back to it: it finishes signing in \
within a few seconds.</p>{offer}"
                ),
            );
            for cookie in &cookies {
                if let Ok(value) = HeaderValue::from_str(cookie) {
                    page.headers_mut().append(header::SET_COOKIE, value);
                }
            }
            page
        }
        Err(error) if error.code == "client_limit" => pages::message(
            StatusCode::FORBIDDEN,
            "Secure your account to sign in more terminals",
            &error.message,
            Some(("/account#secure", "Secure my account")),
        ),
        Err(error) if error.code == "plan_limit" => pages::message(
            StatusCode::FORBIDDEN,
            &pages::site().messages.access.limit_title,
            &error.message,
            Some(("/account", "Your account")),
        ),
        Ok(true) => pages::message(
            StatusCode::OK,
            "Cancelled",
            "Your terminal was not signed in.",
            None,
        ),
        Ok(false) => not_valid(),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            super::routes::unavailable()
        }
    }
}
