//! `/app/machines/<name>`: one machine — its status, build and when it was last seen,
//! **Update now** (`POST /app/machines/<name>/update`: what the `machine_update` tool does,
//! from this hub's own page only), and the last operations the hub routed to it
//! (`Links::recent_jobs`: when, the command's first 80 characters, state, exit code).

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

use super::{home, person, words};
use crate::{
    AppState,
    hub::MachineUpdate,
    site::Ctx,
    web::{
        approvals::same_origin,
        pages::{self, Tab, escape},
        routes::see_other,
    },
};

/// What the last Update now came to, carried back to the page after it.
#[derive(Deserialize, Default)]
pub struct Shown {
    state: Option<String>,
    detail: Option<String>,
}

pub async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(shown): Query<Shown>,
) -> Response {
    let here = format!("/app/machines/{}", super::super::encode(&name));
    let subject = match person(&state, &headers, &here).await {
        Ok(subject) => subject,
        Err(response) => return response,
    };
    let site = words();
    let copy = &site.messages.app;
    let machines = state.hub.machine_status(&subject).await;
    let Some(machine) = machines.into_iter().find(|m| m.name == name) else {
        return pages::app_page(
            StatusCode::NOT_FOUND,
            &copy.machine_unknown,
            &format!(
                "<h1>{}</h1><p class=\"note\">{}</p>",
                escape(&copy.machine_unknown),
                escape(&copy.machine_unknown_text)
            ),
            Tab::Machines,
        );
    };
    let device = state.hub.link_device(&subject, &machine.name);
    let seen = if machine.status == "online" {
        escape(&copy.seen_now)
    } else {
        device
            .as_deref()
            .and_then(|device| state.hub.links.last_seen(device))
            .map(pages::utc)
            .unwrap_or_else(|| escape(&copy.seen_never))
    };
    let build = match &machine.build {
        Some(build) => escape(build),
        None => escape(&copy.machine_build_unknown),
    };
    let updating = if machine.update.as_deref() == Some("updating") {
        escape(&copy.updating)
    } else {
        String::new()
    };
    let result = shown
        .state
        .map(|done| {
            let line = Ctx::new(&site)
                .with("state", done)
                .inline(&copy.update_state);
            let detail = shown
                .detail
                .map(|detail| format!(" · {}", escape(&detail)))
                .unwrap_or_default();
            format!("<p class=\"note\" id=\"update-result\" role=\"status\">{line}{detail}</p>")
        })
        .unwrap_or_default();
    let jobs = device
        .as_deref()
        .map(|device| state.hub.links.recent_jobs(device))
        .unwrap_or_default();
    let jobs = if jobs.is_empty() {
        format!("<p class=\"empty\">{}</p>", escape(&copy.jobs_empty))
    } else {
        let rows: String = jobs
            .iter()
            .map(|job| {
                format!(
                    "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
                    pages::utc(job.at),
                    escape(&job.command),
                    escape(&job.state),
                    job.exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_default(),
                )
            })
            .collect();
        format!(
            "<table class=\"jobs\"><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr></thead>\
<tbody>{rows}</tbody></table>",
            escape(&copy.job_when),
            escape(&copy.job_command),
            escape(&copy.job_state),
            escape(&copy.job_exit),
        )
    };
    let n = escape(&machine.name);
    let body = format!(
        "<h1>{n}</h1>{result}<section class=\"card\" id=\"machine\"><dl class=\"facts\">\
<dt>{}</dt><dd><span class=\"status {}\">{}</span> <span class=\"meta\">{updating}</span></dd>\
<dt>{}</dt><dd id=\"build\">{build}</dd><dt>{}</dt><dd id=\"last-seen\">{seen}</dd></dl>\
<form method=\"post\" action=\"/app/machines/{}/update\"><p class=\"actions\">\
<button class=\"approve\" type=\"submit\" id=\"update-now\">{}</button></p></form></section>\
<h2>{}</h2>{jobs}",
        escape(&copy.machine_status),
        machine.status,
        escape(&home::status_word(machine.status)),
        escape(&copy.machine_build),
        escape(&copy.last_seen),
        super::super::encode(&machine.name),
        escape(&copy.update_now),
        escape(&copy.jobs_title),
    );
    let mut response = pages::app_page(StatusCode::OK, &machine.name, &body, Tab::Machines);
    // Its form posts here: the browser sends a POST's true Origin only under `same-origin`.
    response.headers_mut().insert(
        axum::http::header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("same-origin"),
    );
    response
}

/// `POST /app/machines/<name>/update`: the machine updates to the hub's build, as
/// `machine_update` does it (`Hub::update_machine`, audited there); then its page, with
/// what it came to.
pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let here = format!("/app/machines/{}", super::super::encode(&name));
    let site = words();
    let copy = &site.messages.app;
    if !same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(
            StatusCode::FORBIDDEN,
            &copy.update_now,
            &copy.not_from_here,
            None,
        );
    }
    let subject = match person(&state, &headers, &here).await {
        Ok(subject) => subject,
        Err(response) => return response,
    };
    // A tenant waiting to be let in changes nothing (`tenant_access.rs`).
    if let Some(refused) = state.hub.waiting_refusal(&subject) {
        return pages::message(
            StatusCode::FORBIDDEN,
            &copy.update_now,
            &refused.message,
            None,
        );
    }
    let asked = MachineUpdate {
        machine: name.clone(),
        force: false,
        wait_seconds: None,
    };
    let (done, detail) = match state.hub.update_machine(&subject, asked).await {
        Ok(updated) => (updated.state, updated.detail),
        Err(refused) => ("failed".to_string(), Some(refused.0.message)),
    };
    let mut to = format!("{here}?state={}", super::super::encode(&done));
    if let Some(detail) = detail {
        to.push_str(&format!("&detail={}", super::super::encode(&detail)));
    }
    see_other(&to, &[])
}
