//! What the ticket page's forms do (`ticket.rs`): `POST /app/tickets/<project>/<id>/<action>`
//! with `comment`, `assign`, `close`, `undo` or `hand`, each one board call as the person
//! (`Actor::human`), audited as the board tools audit theirs.

use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use devagotchi_contracts::BridgeResult;
use serde::Deserialize;

use super::{
    open_board, person,
    ticket::{UNDO_MS, now_ms, unknown},
    words,
};
use crate::{
    AppState,
    audit::AuditEntry,
    board::{Actor, Board, Patch, workflow::StateKind},
    web::{approvals::same_origin, encode, pages, routes::see_other},
};

#[derive(Deserialize)]
pub struct Act {
    #[serde(default)]
    version: i64,
    #[serde(default)]
    body: String,
    /// Undo: the state the ticket was closed from.
    #[serde(default)]
    from: String,
}

/// `POST /app/tickets/<project>/<id>/<action>`: comment, assign, close, undo or hand.
pub async fn act(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((project, id, action)): Path<(String, String, String)>,
    Form(act): Form<Act>,
) -> Response {
    let here = format!("/app/tickets/{}/{}", encode(&project), encode(&id));
    let site = words();
    let copy = &site.messages.app;
    if !same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(
            StatusCode::FORBIDDEN,
            &copy.ticket_unknown,
            &copy.not_from_here,
            None,
        );
    }
    let subject = match person(&state, &headers, &here).await {
        Ok(subject) => subject,
        Err(response) => return response,
    };
    if let Some(refused) = state.hub.waiting_refusal(&subject) {
        return see_other(
            &format!("{here}?done=no&why={}", encode(&refused.message)),
            &[],
        );
    }
    let Some(board) = open_board(&state, &subject, &project) else {
        return unknown();
    };
    let tool = match action.as_str() {
        "comment" => "board_note",
        "assign" | "close" | "undo" | "hand" => "board_update",
        _ => return unknown(),
    };
    let me = subject.id.clone();
    let ticket = id.clone();
    let done = tokio::task::spawn_blocking(move || {
        apply(&board, &Actor::human(&me), &ticket, &action, act)
    })
    .await
    .unwrap_or_else(|_| {
        Err(crate::board::fail(
            "board_store",
            "the board did not answer",
        ))
    });
    let outcome = match &done {
        Ok(_) => "ok".to_string(),
        Err(error) => error.code.clone(),
    };
    if let Err(error) = state.hub.audit.record(&AuditEntry {
        subject: &subject.id,
        via: subject.via,
        tool,
        broker: &project,
        operation_id: &id,
        outcome: &outcome,
    }) {
        return see_other(
            &format!("{here}?done=no&why={}", encode(&error.message)),
            &[],
        );
    }
    match done {
        Ok(next) => see_other(&format!("{here}?{next}"), &[]),
        Err(error) => see_other(
            &format!("{here}?done=no&why={}", encode(&error.message)),
            &[],
        ),
    }
}

/// One action on the board as the person; the query the ticket's page is shown with next.
fn apply(
    board: &Board,
    actor: &Actor<'_>,
    id: &str,
    action: &str,
    act: Act,
) -> BridgeResult<String> {
    let workflow = board.workflow()?;
    let update = |patch: Patch| board.update(actor, id, act.version, patch);
    match action {
        "comment" => board
            .add_note(actor, id, "", &act.body, None, None)
            .map(|_| "done=comment".into()),
        "assign" => update(Patch {
            assignee: Some(Some(actor.id.to_string())),
            ..Patch::default()
        })
        .map(|_| "done=assign".into()),
        "close" => {
            let current = board.get(id)?.ticket;
            // Done when the workflow goes there from here, else its first closed state that it does.
            let to = ["done".to_string()]
                .into_iter()
                .chain(
                    workflow
                        .states
                        .iter()
                        .filter(|s| s.kind == StateKind::Done)
                        .map(|s| s.key.clone()),
                )
                .find(|to| workflow.is_done(to) && workflow.allows(&current.state, to))
                .unwrap_or_else(|| "done".into());
            update(Patch {
                state: Some(to),
                ..Patch::default()
            })
            .map(|_| format!("done=close&from={}", encode(&current.state)))
        }
        "undo" => {
            let current = board.get(id)?.ticket;
            if current
                .closed_at
                .is_none_or(|closed| now_ms() >= closed + UNDO_MS)
            {
                return Ok("done=late".into());
            }
            // Back where it was, or to the queue when the workflow reopens only there.
            let to = if workflow.allows(&current.state, &act.from) {
                act.from
            } else {
                workflow.initial_state.clone()
            };
            // A workflow someone changed keeps its own rules: say which one Undo needs.
            if !workflow.allows(&current.state, &to) {
                return Err(crate::board::fail(
                    "transition_not_allowed",
                    format!(
                        "this board's workflow ({}) has no transition {} -> {}; add {{\"from\": \"{}\", \"to\": \"{}\"}} to its transitions to undo here",
                        workflow.id, current.state, to, current.state, to
                    ),
                ));
            }
            update(Patch {
                state: Some(to),
                ..Patch::default()
            })
            .map(|_| "done=undo".into())
        }
        _ => {
            let queue = workflow
                .lanes
                .first()
                .map(|l| l.queue.clone())
                .unwrap_or(workflow.initial_state.clone());
            update(Patch {
                state: Some(queue),
                assignee: Some(None),
                ..Patch::default()
            })
            .map(|_| "done=hand".into())
        }
    }
}
