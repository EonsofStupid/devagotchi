//! The phone control app (`docs/spec-phone-app.md`): server-rendered pages in one fixed
//! frame (`pages::app_page`: the bar on top, the tabs at the bottom, the page scrolling in
//! between), installable from the hub's manifest. Every word is the site's
//! (`content/messages.json` `app`). The one script, `/assets/app.js`, shows the install
//! banner, refreshes on a pull and registers the service worker, which keeps the frame and
//! the stylesheet so the app opens offline on `/app/offline`.

mod home;

use std::sync::Arc;

use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
};

use super::{
    pages::{self, escape},
    routes::{Who, not_set_up, see_other, unavailable, who},
};
use crate::{AppState, auth::Subject, board::Board, board::mcp::Boards, site::Site};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/app", get(home::home))
        .route("/app/machines", get(home::machines))
        .route("/app/boards", get(home::boards))
        .route("/app/offline", get(offline))
        .route("/assets/app.js", get(script))
}

/// The person signed in to this browser, or the page that answers instead: signed out,
/// the sign-in page that comes back to `return_to`.
pub(super) async fn person(
    state: &AppState,
    headers: &HeaderMap,
    return_to: &str,
) -> Result<Subject, Response> {
    match who(state, headers).await {
        Who::Person(subject) => Ok(subject),
        Who::SignedOut => Err(see_other(
            &format!("/auth/login?return_to={}", super::encode(return_to)),
            &[],
        )),
        Who::Unavailable => Err(unavailable()),
        Who::NotSetUp => Err(not_set_up()),
    }
}

/// The boards the person may work on, by project, as `whoami` lists them: the account's
/// own (or its org's) first, then the shared ones it holds a grant for.
pub(super) fn board_names(state: &AppState, subject: &Subject) -> Vec<String> {
    let mut names = state.hub.boards.account_boards(&Boards::owner_of(subject));
    for entry in state.hub.config.boards_for(&subject.id) {
        if !names.contains(&entry.project) {
            names.push(entry.project.clone());
        }
    }
    names
}

/// The board `project` names for this person, as the board tools open it: the account's
/// own first, then a shared one by grant. None when it is not theirs.
pub(super) fn open_board(state: &AppState, subject: &Subject, project: &str) -> Option<Arc<Board>> {
    let boards = &state.hub.boards;
    if let Ok(Some(board)) = boards.open_account(&Boards::owner_of(subject), project) {
        return Some(board);
    }
    let granted = state
        .hub
        .config
        .boards_for(&subject.id)
        .iter()
        .any(|entry| entry.project == project);
    if granted {
        boards.open(project).ok()
    } else {
        None
    }
}

/// The site's words for the app.
pub(super) fn words() -> Arc<Site> {
    pages::site()
}

/// A card: a heading and what is under it, or the line that says it is empty.
pub(super) fn card(id: &str, title: &str, rows: &[String], empty: &str) -> String {
    let inner = if rows.is_empty() {
        format!("<p class=\"empty\">{}</p>", escape(empty))
    } else {
        format!("<ul class=\"rows\">{}</ul>", rows.concat())
    };
    format!(
        "<section class=\"card\" id=\"{id}\"><h2>{}</h2>{inner}</section>",
        escape(title)
    )
}

/// `/app/offline`: what the service worker shows when the hub cannot be reached.
async fn offline() -> Response {
    let site = words();
    let words = &site.messages.app;
    pages::app_page(
        StatusCode::OK,
        &words.offline_title,
        &format!(
            "<h1>{}</h1><p class=\"note\">{}</p>",
            escape(&words.offline_title),
            escape(&words.offline_text)
        ),
        pages::Tab::Home,
    )
}

/// `/assets/app.js`: the app's one script.
async fn script() -> Response {
    super::notify::asset(
        include_str!("../assets/app.js").to_string(),
        "text/javascript; charset=utf-8",
        "no-cache",
    )
}

#[cfg(test)]
mod tests;
