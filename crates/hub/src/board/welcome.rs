//! What a new board shows first: a "Getting started" ticket in the workflow's attention
//! state. The first AI session primes the board and finds the steps under `attention`;
//! the person finds them on the board, instead of an empty one. It is made once, only on
//! a board with no tickets at all, and recorded with a `board.welcomed` event. It sits
//! outside the agents' queue, so nobody claims it by accident.

use devagotchi_contracts::BridgeResult;
use rusqlite::params;

use super::{Actor, Board, load_workflow, store_error};

pub const TITLE: &str = "Getting started: check Devagotchi works for you";
pub const LABEL: &str = "getting-started";

/// The steps, with this hub's address in them.
pub fn steps(public_url: &str) -> String {
    let hub = public_url.trim_end_matches('/');
    format!(
        "Devagotchi is connected. Four steps, about five minutes:\n\n\
1. Ask your AI: \"Which of my machines can you reach?\" It calls `machines` and lists each \
machine and whether it is online.\n\
2. On your phone, open {hub}/approvals. Tap **Notify this phone**, then **Send me a test \
approval**. Approve it from the notification.\n\
3. Ask your AI for something small on one machine, for example: \"Show me the free disk \
space on <machine>.\" Anything you should sign off on waits for you on your phone.\n\
4. Tell your AI it worked, and ask it to close this ticket.\n\n\
Stuck on a step? Tell your AI which one; it can read this ticket."
    )
}

impl Board {
    /// Put the getting-started ticket on a board that has never had a ticket. Returns
    /// its id, or `None` when the board already has tickets.
    pub fn welcome_if_new(
        &self,
        actor: &Actor<'_>,
        public_url: &str,
    ) -> BridgeResult<Option<String>> {
        self.write(actor, |w| {
            let tickets: i64 =
                w.tx.query_row("SELECT count(*) FROM tickets", [], |row| row.get(0))
                    .map_err(store_error)?;
            if tickets > 0 {
                return Ok(None);
            }
            let workflow = load_workflow(w.tx)?;
            let id = w.fresh_id(&self.prefix, &self.project, TITLE, actor.id)?;
            w.tx.execute(
                "INSERT INTO tickets (id, kind, title, description, state, priority, labels, parent,
                   assignee, created_by, created_at, updated_at)
                 VALUES (?1, 'task', ?2, ?3, ?4, 1, ?5, NULL, NULL, ?6, ?7, ?7)",
                params![
                    id,
                    TITLE,
                    steps(public_url),
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
                serde_json::json!({ "title": TITLE }),
            )?;
            Ok(Some(id))
        })
    }
}
