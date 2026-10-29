//! "Add a machine" and "Revoke" on the Machines page (increment 29a). A signed-in person
//! names a machine and gets a one-time code for it, shown once in this response and never
//! put in a URL; the machine that enrolls with it belongs to that account alone. Forms
//! only, from this hub's own pages (`same_origin`), like the approvals page.

use axum::{
    Form,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use axum::{Json, http::header, response::IntoResponse};
use serde::Deserialize;

use crate::auth::AuthError;

use super::{
    approvals::same_origin,
    pages::{self, escape},
    routes::{Who, not_set_up, see_other, unavailable, who},
};
use crate::{AppState, devices};

#[derive(Deserialize)]
pub struct Named {
    name: String,
}

/// The signed-in person behind a form post from this hub's own page, or the page to show.
async fn person(state: &AppState, headers: &HeaderMap) -> Result<String, Box<Response>> {
    let Some(web) = state.web.clone() else {
        return Err(Box::new(not_set_up()));
    };
    if !same_origin(headers, &state.hub.config.public_url) {
        return Err(Box::new(pages::message(
            StatusCode::FORBIDDEN,
            "Not from this hub",
            "Use the Machines page itself.",
            Some(("/account", "Your machines")),
        )));
    }
    match who(state, &web, headers).await {
        Who::Person(subject) => Ok(subject.id),
        Who::SignedOut => Err(Box::new(see_other("/auth/login?return_to=/account", &[]))),
        Who::Unavailable => Err(Box::new(unavailable())),
    }
}

pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<Named>,
) -> Response {
    let owner = match person(&state, &headers).await {
        Ok(owner) => owner,
        Err(page) => return *page,
    };
    let name = form.name.trim().to_ascii_lowercase();
    match state.hub.links.issue_owned_code(&owner, &name) {
        Ok(code) => code_page(
            &state.hub.config.public_url,
            &name,
            &code,
            state.hub.downloads.is_some(),
        ),
        Err(error) => pages::message(
            match error.code.as_str() {
                "device_store" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::CONFLICT,
            },
            "No code issued",
            &error.message,
            Some(("/account", "Back to your machines")),
        ),
    }
}

pub async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<Named>,
) -> Response {
    let owner = match person(&state, &headers).await {
        Ok(owner) => owner,
        Err(page) => return *page,
    };
    match state.hub.links.revoke_owned(&owner, &form.name) {
        Ok(_) => see_other("/account", &[]),
        Err(error) => pages::message(
            StatusCode::CONFLICT,
            "Not revoked",
            &error.message,
            Some(("/account", "Back to your machines")),
        ),
    }
}

/// The code, once, with what to run on the machine. The page is never cached. With a
/// downloads directory the hub hands out its own build: one line per OS (`/join`).
fn code_page(hub: &str, name: &str, code: &str, joinable: bool) -> Response {
    let until = pages::utc(devices::now_secs() + devices::CODE_TTL_SECS);
    let hub = hub.trim_end_matches('/');
    let (hub, name, code) = (escape(hub), escape(name), escape(code));
    let how = if joinable {
        format!(
            "<p>Paste one line on that machine. It installs Devagotchi, gives the machine its own \
key, joins it as <strong>{name}</strong> and starts it, now and after every restart. The line \
works once, until {until}; this page is the only place it is shown.</p>\
<h2>Linux</h2><pre class=\"what\">curl -fsSL {hub}/join/{code} | sudo sh</pre>\
<h2>Windows</h2><p class=\"meta\">In PowerShell (no administrator needed):</p>\
<pre class=\"what\">irm {hub}/join/{code}.ps1 | iex</pre>"
        )
    } else {
        format!(
            "<p>This code adds one machine as <strong>{name}</strong>. It works once, until {until}, and \
this page is the only place it is shown.</p><pre class=\"what\">{code}</pre>\
<h2>Linux</h2><p class=\"meta\">With the Devagotchi installer on that machine:</p>\
<pre class=\"what\">echo {code} | sudo ./install-devagotchi.sh --hub {hub} --device {name} --enroll --exec-policy account</pre>\
<h2>Windows</h2><p class=\"meta\">In PowerShell, next to the installer:</p>\
<pre class=\"what\">.\\install-devagotchi.ps1 -Autostart -ExecPolicy account -Hub {hub} -Device {name} -EnrollCode {code}</pre>"
        )
    };
    pages::html(
        StatusCode::OK,
        "Add a machine",
        &format!(
            "<h1>Add {name}</h1>{how}\
<p class=\"note\">When it joins you get a notification on the phones you turned on, and it \
shows up under your machines. Only your account reaches it.</p>\
<p><a class=\"button\" href=\"/account\">Your machines</a></p>"
        ),
    )
}

fn json_reply(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// `POST /api/v1/machines` `{ "name": "..." }` with `Authorization: Bearer <token>`: the
/// same one-time code "Add a machine" issues, for an app the person is signed in to (the
/// devagotchi.com Connect page). Bearer only, never the hub's cookie, so another site cannot
/// make a signed-in browser issue codes. The code is in this response once and nowhere else.
pub async fn add_json(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(form): Json<Named>,
) -> Response {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim);
    let Some(token) = bearer else {
        return json_reply(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "signed_out" }),
        );
    };
    let owner = match state.verifier.verify(token).await {
        Ok(subject) => subject.id,
        Err(AuthError::Invalid(why)) => {
            crate::log_refusal(why);
            return json_reply(
                StatusCode::UNAUTHORIZED,
                serde_json::json!({ "error": "invalid_token" }),
            );
        }
        Err(AuthError::Unavailable(_)) => {
            return json_reply(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": "issuer_unavailable" }),
            );
        }
    };
    let name = form.name.trim().to_ascii_lowercase();
    let code = match state.hub.links.issue_owned_code(&owner, &name) {
        Ok(code) => code,
        Err(error) => {
            let status = match error.code.as_str() {
                "device_store" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::CONFLICT,
            };
            return json_reply(
                status,
                serde_json::json!({ "error": error.code, "message": error.message }),
            );
        }
    };
    let hub = state.hub.config.public_url.trim_end_matches('/');
    let commands = state.hub.downloads.is_some().then(|| {
        serde_json::json!({
            "linux": format!("curl -fsSL {hub}/join/{code} | sudo sh"),
            "windows": format!("irm {hub}/join/{code}.ps1 | iex"),
        })
    });
    json_reply(
        StatusCode::CREATED,
        serde_json::json!({
            "name": name,
            "code": code,
            "expires_at": devices::now_secs() + devices::CODE_TTL_SECS,
            "commands": commands,
        }),
    )
}
