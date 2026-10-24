//! `/approvals`: the requests agents are waiting on, one tap to approve or deny, laid out
//! for a phone. A signed-in person sees the pending approvals on every board they may use;
//! deciding names the version they saw, so a request that changed is shown again first.
//! Forms only, no script: the session cookie is `SameSite=Lax` and a decision must also
//! come from this hub's own pages (`Sec-Fetch-Site` / `Origin`). "Notify this phone" turns
//! on push for the browser it is tapped in (`notify.rs`).

use axum::{
    Form,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

use super::{
    pages::{self, escape},
    routes::{Who, not_set_up, see_other, unavailable, who},
};
use crate::{
    AppState,
    board::{
        Actor,
        approval::{ApprovalStatus, Verdict},
    },
};

#[derive(Deserialize)]
pub struct Shown {
    done: Option<String>,
}

#[derive(Deserialize)]
pub struct Decide {
    project: String,
    id: String,
    version: i64,
    verdict: Verdict,
    #[serde(default)]
    note: String,
}

/// A decision must come from this hub's own page, not another site's form.
pub(super) fn same_origin(headers: &HeaderMap, public_url: &str) -> bool {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(site) = header("sec-fetch-site") {
        return site == "same-origin";
    }
    match header("origin") {
        Some(origin) => public_url.trim_end_matches('/') == origin,
        None => true,
    }
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(shown): Query<Shown>,
) -> Response {
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    let subject = match who(&state, &web, &headers).await {
        Who::Person(subject) => subject,
        Who::SignedOut => return see_other("/auth/login?return_to=/approvals", &[]),
        Who::Unavailable => return unavailable(),
    };
    if !state.hub.config.is_human(&subject.id) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Only people approve",
            "This account is not marked as a person on this hub, so it cannot approve or deny.",
            Some(("/account", "Your account")),
        );
    }
    let projects: Vec<String> = state
        .hub
        .config
        .boards_for(&subject.id)
        .iter()
        .map(|b| b.project.clone())
        .collect();
    let boards = state.hub.boards.clone();
    let pending = tokio::task::spawn_blocking(move || {
        let mut all = Vec::new();
        for project in projects {
            if let Ok(board) = boards.open(&project) {
                for approval in board.pending_approvals().unwrap_or_default() {
                    all.push((project.clone(), approval));
                }
            }
        }
        all
    })
    .await
    .unwrap_or_default();
    let key = state.hub.push.as_ref().map(|p| p.public_key.clone());
    page(&pending, shown.done.as_deref(), key.as_deref())
}

pub async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<Decide>,
) -> Response {
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    if !same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not from this hub",
            "Decide from the approvals page itself.",
            Some(("/approvals", "Approvals")),
        );
    }
    let subject = match who(&state, &web, &headers).await {
        Who::Person(subject) => subject,
        Who::SignedOut => return see_other("/auth/login?return_to=/approvals", &[]),
        Who::Unavailable => return unavailable(),
    };
    let human = state.hub.config.is_human(&subject.id);
    let granted = state
        .hub
        .config
        .boards_for(&subject.id)
        .iter()
        .any(|b| b.project == form.project);
    if !human || !granted {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not yours to decide",
            "This account cannot decide approvals on that board.",
            Some(("/approvals", "Approvals")),
        );
    }
    let boards = state.hub.boards.clone();
    let id = subject.id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let board = boards.open(&form.project)?;
        let actor = Actor {
            id: &id,
            human: true,
            wip_limit: None,
        };
        board.decide_approval(&actor, &form.id, form.version, form.verdict, &form.note)
    })
    .await;
    match result {
        Ok(Ok(decided)) => see_other(&format!("/approvals?done={}", decided.id), &[]),
        Ok(Err(error)) => pages::message(
            StatusCode::CONFLICT,
            "Not decided",
            &error.message,
            Some(("/approvals", "Back to approvals")),
        ),
        Err(_) => pages::message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Not decided",
            "The board did not answer; nothing changed.",
            Some(("/approvals", "Back to approvals")),
        ),
    }
}

fn page(
    pending: &[(String, ApprovalStatus)],
    done: Option<&str>,
    push_key: Option<&str>,
) -> Response {
    let flash = done
        .map(|id| {
            format!(
                "<p class=\"note\">Decided {}. The agent continues on its own.</p>",
                escape(id)
            )
        })
        .unwrap_or_default();
    let cards: String = pending
        .iter()
        .map(|(project, a)| {
            format!(
                "<section class=\"card\" id=\"{p}-{i}\"><h2>{t}</h2><p class=\"meta\">{by} asks · {when} · {p}</p>\
<pre class=\"what\">{d}</pre><form method=\"post\" action=\"/approvals/decide\">\
<input type=\"hidden\" name=\"project\" value=\"{p}\"><input type=\"hidden\" name=\"id\" value=\"{i}\">\
<input type=\"hidden\" name=\"version\" value=\"{v}\">\
<label>Note (optional)<input type=\"text\" name=\"note\" maxlength=\"2000\"></label>\
<p class=\"actions\"><button class=\"approve\" name=\"verdict\" value=\"approved\">Approve</button> \
<button class=\"deny\" name=\"verdict\" value=\"denied\">Deny</button></p></form></section>",
                p = escape(project),
                i = escape(&a.id),
                t = escape(&a.title),
                by = escape(&a.requested_by),
                when = pages::utc((a.requested_at / 1000).max(0) as u64),
                d = escape(&a.description),
                v = a.version,
            )
        })
        .collect();
    let body = if pending.is_empty() {
        "<p class=\"empty\">Nothing is waiting for you.</p>".to_string()
    } else {
        cards
    };
    let notify = push_key
        .map(|key| {
            format!(
                "<p><button id=\"notify\" type=\"button\" data-key=\"{}\">Notify this phone</button> \
<span id=\"notify-status\" class=\"note\" role=\"status\"></span></p>",
                escape(key)
            )
        })
        .unwrap_or_default();
    pages::app(
        StatusCode::OK,
        "Approvals",
        &format!("<h1>Approvals</h1>{notify}{flash}{body}"),
    )
}
