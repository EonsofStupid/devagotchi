//! `/approvals`: the requests agents are waiting on, one tap to approve or deny, laid out
//! for a phone. A signed-in person sees the pending approvals on every board they may use;
//! deciding names the version they saw, so a request that changed is shown again first.
//! Forms only, no script: the session cookie is `SameSite=Lax` and a decision must also
//! come from this hub's own pages (`Sec-Fetch-Site` / `Origin`). "Notify this phone" turns
//! on push for the browser it is tapped in (`notify.rs`); "Send me a test approval" then
//! puts a harmless approval on the person's first board and pushes it to their phones
//! only, so the whole loop is checked with one tap and no AI.

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
    test: Option<String>,
}

const TEST_TITLE: &str = "Test: tap Approve";
/// The board actor a test approval is asked by, shown as "Devagotchi asks".
const TEST_ASKER: &str = "devagotchi";

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
    let flash = Flash {
        done: shown.done.as_deref(),
        test: shown.test.as_deref(),
    };
    let can_test = !state.hub.config.boards_for(&subject.id).is_empty();
    page(&pending, &flash, key.as_deref(), &subject.id, can_test)
}

/// "Send me a test approval": a harmless approval on the person's first board, pushed to
/// their own phones only.
pub async fn test(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(web) = state.web.clone() else {
        return not_set_up();
    };
    if !same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not from this hub",
            "Send the test from the approvals page itself.",
            Some(("/approvals", "Approvals")),
        );
    }
    let subject = match who(&state, &web, &headers).await {
        Who::Person(subject) => subject,
        Who::SignedOut => return see_other("/auth/login?return_to=/approvals", &[]),
        Who::Unavailable => return unavailable(),
    };
    let Some(push) = state.hub.push.clone() else {
        return see_other("/approvals", &[]);
    };
    let project = state
        .hub
        .config
        .boards_for(&subject.id)
        .first()
        .map(|b| b.project.clone());
    let (true, Some(project)) = (state.hub.config.is_human(&subject.id), project) else {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not yours to test",
            "This account has no board it decides approvals on.",
            Some(("/account", "Your account")),
        );
    };
    let phones = push
        .devices()
        .push_for(&subject.id)
        .map(|p| p.len())
        .unwrap_or(0);
    if phones == 0 {
        return see_other("/approvals?test=no-phone", &[]);
    }
    let boards = state.hub.boards.clone();
    let on = project.clone();
    let made = tokio::task::spawn_blocking(move || {
        boards.open(&on)?.request_approval(
            &Actor::agent(TEST_ASKER),
            TEST_TITLE,
            "Nothing runs. This checks that your phone gets the notification and that your answer comes back.",
            "You tapped Send me a test approval.",
            "Nothing to undo.",
        )
    })
    .await;
    match made {
        Ok(Ok(approval)) => {
            state
                .hub
                .announce_approval_to(vec![subject.id.clone()], &project, &approval);
            see_other("/approvals?test=sent", &[])
        }
        _ => pages::message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Test not sent",
            "The board did not answer; nothing was sent.",
            Some(("/approvals", "Back to approvals")),
        ),
    }
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

struct Flash<'a> {
    done: Option<&'a str>,
    test: Option<&'a str>,
}

/// Who asked, as the person reads it: their own account asking is their AI.
fn asker(requested_by: &str, viewer: &str) -> String {
    if requested_by == viewer {
        "Your AI asks".into()
    } else if requested_by == TEST_ASKER {
        "Devagotchi asks".into()
    } else {
        format!("{} asks", escape(requested_by))
    }
}

fn page(
    pending: &[(String, ApprovalStatus)],
    flash: &Flash<'_>,
    push_key: Option<&str>,
    viewer: &str,
    can_test: bool,
) -> Response {
    let decided = flash
        .done
        .map(|id| {
            format!(
                "<p class=\"note\">Decided {}. The agent continues on its own.</p>",
                escape(id)
            )
        })
        .unwrap_or_default();
    let tested = match flash.test {
        Some("sent") => {
            "<p class=\"note\">Test sent. Your phone should buzz within a few seconds: approve it from the notification.</p>"
        }
        Some("no-phone") => {
            "<p class=\"note\">Tap Notify this phone first, then send the test.</p>"
        }
        _ => "",
    };
    let flash = format!("{decided}{tested}");
    let cards: String = pending
        .iter()
        .map(|(project, a)| {
            format!(
                "<section class=\"card\" id=\"{p}-{i}\"><h2>{t}</h2><p class=\"meta\">{by} · {when} · {p}</p>\
<pre class=\"what\">{d}</pre><form method=\"post\" action=\"/approvals/decide\">\
<input type=\"hidden\" name=\"project\" value=\"{p}\"><input type=\"hidden\" name=\"id\" value=\"{i}\">\
<input type=\"hidden\" name=\"version\" value=\"{v}\">\
<label>Note (optional)<input type=\"text\" name=\"note\" maxlength=\"2000\"></label>\
<p class=\"actions\"><button class=\"approve\" name=\"verdict\" value=\"approved\">Approve</button> \
<button class=\"deny\" name=\"verdict\" value=\"denied\">Deny</button></p></form></section>",
                p = escape(project),
                i = escape(&a.id),
                t = escape(&a.title),
                by = asker(&a.requested_by, viewer),
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
<span id=\"notify-status\" class=\"note\" role=\"status\"></span></p>{}",
                escape(key),
                if can_test {
                    "<form method=\"post\" action=\"/approvals/test\"><p><button class=\"test\" type=\"submit\">\
Send me a test approval</button></p></form>"
                } else {
                    ""
                }
            )
        })
        .unwrap_or_default();
    pages::app(
        StatusCode::OK,
        "Approvals",
        &format!("<h1>Approvals</h1>{notify}{flash}{body}"),
    )
}
