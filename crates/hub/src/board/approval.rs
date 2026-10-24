//! Approvals: an agent asks a person to approve one step, the person approves or denies
//! (from the hub's `/approvals` page on a phone, or any surface that calls
//! `decide_approval`), and the agent continues on the decision. An approval is a ticket of
//! kind `approval` that starts in the workflow's attention state, so it shows where people
//! look and never sits in an agent's queue. Only a person's decision moves it: approved
//! goes to a done state, denied to cancelled, each with an `approval.decided` event.

use devagotchi_contracts::BridgeResult;
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    Actor, Board, StateKind, Ticket, Workflow, fail, insert_note, invalid, load_workflow,
    read_ticket, store_error,
};

pub const KIND: &str = "approval";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approved,
    Denied,
}

/// Where an approval stands. `status` is `pending`, `approved` or `denied`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalStatus {
    pub id: String,
    pub title: String,
    pub status: String,
    /// What will happen, why, and how to undo it, as the agent wrote them.
    pub description: String,
    pub requested_by: String,
    pub requested_at: i64,
    /// Changes whenever the approval does; a decision names the version the person saw.
    pub version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn text(field: &str, value: &str, max: usize, required: bool) -> BridgeResult<String> {
    let value = value.trim();
    if (required && value.is_empty()) || value.chars().count() > max {
        let need = if required { "1" } else { "0" };
        return Err(invalid(format!(
            "{field} must be {need} to {max} characters"
        )));
    }
    Ok(value.to_string())
}

/// The done state a verdict lands in: `done` / `cancelled` when the workflow has them as
/// done states, else its first done state.
fn landing(workflow: &Workflow, verdict: Verdict) -> BridgeResult<String> {
    let preferred = match verdict {
        Verdict::Approved => "done",
        Verdict::Denied => "cancelled",
    };
    if workflow.is_done(preferred) {
        return Ok(preferred.into());
    }
    workflow
        .states
        .iter()
        .find(|s| s.kind == StateKind::Done)
        .map(|s| s.key.clone())
        .ok_or_else(|| fail("no_done_state", "the workflow has no done state"))
}

impl Board {
    /// Ask a person to approve one step. `what` is required; `why` and `undo` should be.
    pub fn request_approval(
        &self,
        actor: &Actor<'_>,
        title: &str,
        what: &str,
        why: &str,
        undo: &str,
    ) -> BridgeResult<ApprovalStatus> {
        let title = text("title", title, 300, true)?;
        let what = text("what", what, 4000, true)?;
        let why = text("why", why, 4000, false)?;
        let undo = text("undo", undo, 4000, false)?;
        let description = format!("What: {what}\n\nWhy: {why}\n\nUndo: {undo}");
        let id = self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let id = w.fresh_id(&self.prefix, &self.project, &title, actor.id)?;
            w.tx.execute(
                "INSERT INTO tickets (id, kind, title, description, state, priority, labels, parent,
                   assignee, created_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, '[\"approval\"]', NULL, NULL, ?6, ?7, ?7)",
                params![id, KIND, title, description, workflow.attention_state, actor.id, w.now],
            )
            .map_err(store_error)?;
            w.event(
                "approval.requested",
                Some(&id),
                None,
                serde_json::json!({ "title": title }),
            )?;
            Ok(id)
        })?;
        self.approval_status(&id)
    }

    /// A person approves or denies `id` as they saw it at `version`.
    pub fn decide_approval(
        &self,
        actor: &Actor<'_>,
        id: &str,
        version: i64,
        verdict: Verdict,
        note: &str,
    ) -> BridgeResult<ApprovalStatus> {
        if !actor.human {
            return Err(fail("humans_only", "only a person can approve or deny"));
        }
        let note = text("note", note, 2000, false)?;
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            if ticket.kind != KIND {
                return Err(fail("not_an_approval", format!("{id} is not an approval")));
            }
            if ticket.state != workflow.attention_state {
                return Err(fail("already_decided", format!("{id} is already decided")));
            }
            if ticket.version != version {
                return Err(fail(
                    "approval_changed",
                    format!("{id} changed since you saw it; look again before deciding"),
                ));
            }
            let to = landing(&workflow, verdict)?;
            w.tx.execute(
                "UPDATE tickets SET state = ?2, closed_at = ?3, version = version + 1, updated_at = ?3
                 WHERE id = ?1",
                params![id, to, w.now],
            )
            .map_err(store_error)?;
            if !note.is_empty() {
                insert_note(w.tx, id, actor.id, "decision", &note, None, None, w.now)?;
            }
            w.event(
                "approval.decided",
                Some(id),
                None,
                serde_json::json!({ "verdict": verdict, "note": note, "to": to }),
            )?;
            Ok(())
        })?;
        self.approval_status(id)
    }

    pub fn approval_status(&self, id: &str) -> BridgeResult<ApprovalStatus> {
        let connection = self.connection.lock().expect("board lock");
        let workflow = load_workflow(&connection)?;
        let ticket = read_ticket(&connection, &workflow, id, self.now())?;
        if ticket.kind != KIND {
            return Err(fail("not_an_approval", format!("{id} is not an approval")));
        }
        let decision: Option<(String, i64, String)> = connection
            .query_row(
                "SELECT actor, at, data FROM events WHERE ticket = ?1 AND action = 'approval.decided'
                 ORDER BY seq DESC LIMIT 1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(store_error)?;
        Ok(status_of(ticket, decision))
    }

    /// Approvals waiting for a person, oldest first.
    pub fn pending_approvals(&self) -> BridgeResult<Vec<ApprovalStatus>> {
        let ids: Vec<String> = {
            let connection = self.connection.lock().expect("board lock");
            let workflow = load_workflow(&connection)?;
            let mut statement = connection
                .prepare(
                    "SELECT id FROM tickets WHERE kind = ?1 AND state = ?2 ORDER BY created_at, id",
                )
                .map_err(store_error)?;
            let rows = statement
                .query_map(params![KIND, workflow.attention_state], |row| row.get(0))
                .map_err(store_error)?;
            rows.collect::<Result<_, _>>().map_err(store_error)?
        };
        ids.iter().map(|id| self.approval_status(id)).collect()
    }
}

fn status_of(ticket: Ticket, decision: Option<(String, i64, String)>) -> ApprovalStatus {
    let (status, decided_by, decided_at, note) = match decision {
        Some((by, at, data)) => {
            let data: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
            let verdict = data["verdict"].as_str().unwrap_or("denied").to_string();
            let note = data["note"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(str::to_string);
            (verdict, Some(by), Some(at), note)
        }
        None => ("pending".to_string(), None, None, None),
    };
    ApprovalStatus {
        id: ticket.id,
        title: ticket.title,
        status,
        description: ticket.description,
        requested_by: ticket.created_by,
        requested_at: ticket.created_at,
        version: ticket.version,
        decided_by,
        decided_at,
        note,
    }
}
