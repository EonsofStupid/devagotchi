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
use serde::Deserialize;

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
        Ok(code) => code_page(&state.hub.config.public_url, &name, &code),
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

/// The code, once, with what to run on the machine. The page is never cached.
fn code_page(hub: &str, name: &str, code: &str) -> Response {
    let until = pages::utc(devices::now_secs() + devices::CODE_TTL_SECS);
    let (hub, name, code) = (escape(hub), escape(name), escape(code));
    pages::html(
        StatusCode::OK,
        "Add a machine",
        &format!(
            "<h1>Add {name}</h1>\
<p>This code adds one machine as <strong>{name}</strong>. It works once, until {until}, and \
this page is the only place it is shown.</p><pre class=\"what\">{code}</pre>\
<h2>Linux</h2><p class=\"meta\">With the Devagotchi installer on that machine:</p>\
<pre class=\"what\">echo {code} | sudo ./install-devagotchi.sh --hub {hub} --device {name} --enroll</pre>\
<h2>Windows</h2><p class=\"meta\">In PowerShell as administrator, next to the installer:</p>\
<pre class=\"what\">.\\install-devagotchi.ps1 -Hub {hub} -Device {name} -EnrollCode {code}</pre>\
<p class=\"note\">When it joins you get a notification on the phones you turned on, and it \
shows up under your machines. Only your account reaches it.</p>\
<p><a class=\"button\" href=\"/account\">Your machines</a></p>"
        ),
    )
}
