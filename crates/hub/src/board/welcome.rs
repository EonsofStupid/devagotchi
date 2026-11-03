//! What a new board shows first: a "Getting started" ticket in the workflow's attention
//! state. The first AI session primes the board and finds the steps under `attention`;
//! the person finds them on the board, instead of an empty one. It is made once, only on
//! a board with no tickets at all, and recorded with a `board.welcomed` event. It sits
//! outside the agents' queue, so nobody claims it by accident. Its title and steps are the
//! site's (`content/messages.json` `welcome`, `Site::welcome`).

use devagotchi_contracts::BridgeResult;
use rusqlite::params;

use super::{Actor, Board, load_workflow, store_error};

pub const LABEL: &str = "getting-started";

impl Board {
    /// Put the getting-started ticket, `title` and its `steps`, on a board that has never
    /// had a ticket. Returns its id, or `None` when the board already has tickets.
    pub fn welcome_if_new(
        &self,
        actor: &Actor<'_>,
        title: &str,
        steps: &str,
    ) -> BridgeResult<Option<String>> {
        self.write(actor, |w| {
            let tickets: i64 =
                w.tx.query_row("SELECT count(*) FROM tickets", [], |row| row.get(0))
                    .map_err(store_error)?;
            if tickets > 0 {
                return Ok(None);
            }
            let workflow = load_workflow(w.tx)?;
            let id = w.fresh_id(&self.prefix, &self.project, title, actor.id)?;
            w.tx.execute(
                "INSERT INTO tickets (id, kind, title, description, state, priority, labels, parent,
                   assignee, created_by, created_at, updated_at)
                 VALUES (?1, 'task', ?2, ?3, ?4, 1, ?5, NULL, NULL, ?6, ?7, ?7)",
                params![
                    id,
                    title,
                    steps,
                    workflow.attention_state,
                    serde_json::json!([LABEL]).to_string(),
                    actor.id,
                    w.now
                ],
            )
            .map_err(store_error)?;
            w.event(
                "board.welcomed",
                Some(&id),
                None,
                serde_json::json!({ "title": title }),
            )?;
            Ok(Some(id))
        })
    }
}
