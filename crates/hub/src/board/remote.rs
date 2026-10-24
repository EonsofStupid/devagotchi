//! The board's remote face: the command line's operations as calls to a hub's `board_*`
//! tools, through one session as the signed-in account (`account::client`). This is what a
//! bound project uses (B11): the hub is the board of record and the arbiter; this machine's
//! actor name travels as the seat, so one account's agents are distinct there. The records
//! that come back are the store's own types, so the command line renders them as it always
//! has. Refusals keep the hub's codes (`stale_fence`, `not_holder`, …) and the command line's
//! `next` hints apply; a transport refusal is `hub_unreachable`, `signed_out` or
//! `not_signed_in`, each saying what to do.

use std::{
    cell::RefCell,
    time::{SystemTime, UNIX_EPOCH},
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Actor, ChainStatus, Dep, DepKind, Filter, Lease, NewTicket, NextFilter, Note, Patch, Prime,
    Ticket, TicketView,
    face::{Face, ME},
};
use crate::account::client::HubClient;

pub struct Remote {
    client: RefCell<HubClient>,
    project: String,
    prefix: String,
}

#[derive(Deserialize)]
struct ClaimOut {
    claimed: bool,
    #[serde(default)]
    lease: Option<Lease>,
}

impl Remote {
    pub fn new(client: HubClient, project: &str, prefix: &str) -> Self {
        Self {
            client: RefCell::new(client),
            project: project.to_string(),
            prefix: prefix.to_string(),
        }
    }

    fn call<T: serde::de::DeserializeOwned>(&self, tool: &str, mut args: Value) -> BridgeResult<T> {
        args["project"] = Value::String(self.project.clone());
        self.client.borrow_mut().call(tool, args)
    }

    /// The hub is the arbiter; its tools name the seat from the session, never from a
    /// parameter — so `actor` is not sent, only used by the face for `@me`.
    fn as_me(filter: &Filter) -> Option<String> {
        filter.holder.clone()
    }
}

impl Face for Remote {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn prefix(&self) -> String {
        self.prefix.clone()
    }

    fn location(&self) -> String {
        self.client.borrow().hub().to_string()
    }

    fn prime(&self, _actor: &Actor<'_>, limit: usize) -> BridgeResult<Prime> {
        self.call("board_prime", json!({ "limit": limit.min(10) }))
    }

    fn ready(&self, _actor: &Actor<'_>, limit: usize) -> BridgeResult<Vec<Ticket>> {
        self.call("board_ready", json!({ "limit": limit }))
    }

    fn list(&self, _actor: &Actor<'_>, filter: &Filter) -> BridgeResult<Vec<Ticket>> {
        if filter.ready {
            return Err(BridgeError::new(
                "usage",
                "the ready filter is `board ready` on a hosted board",
            ));
        }
        self.call(
            "board_list",
            json!({
                "state": filter.state,
                "holder": Self::as_me(filter).map(|h| if h == ME { ME.to_string() } else { h }),
                "assignee": filter.assignee,
                "label": filter.label,
                "parent": filter.parent,
                "kind": filter.kind,
                "open_only": filter.open_only,
                "text": filter.text,
                "limit": filter.limit,
            }),
        )
    }

    fn get(&self, id: &str) -> BridgeResult<TicketView> {
        self.call("board_get", json!({ "id": id }))
    }

    fn create(&self, _actor: &Actor<'_>, new: NewTicket) -> BridgeResult<Ticket> {
        self.call(
            "board_create",
            json!({
                "title": new.title,
                "description": new.description,
                "kind": new.kind,
                "priority": new.priority,
                "labels": new.labels,
                "parent": new.parent,
                "assignee": new.assignee,
                "discovered_from": new.discovered_from,
            }),
        )
    }

    fn claim(&self, _actor: &Actor<'_>, id: &str, ttl_ms: Option<i64>) -> BridgeResult<Lease> {
        let out: ClaimOut = self.call("board_claim", json!({ "id": id, "ttl_ms": ttl_ms }))?;
        out.lease.ok_or_else(|| {
            BridgeError::new("board_store", "the hub claimed nothing and said why not")
        })
    }

    fn claim_next(&self, _actor: &Actor<'_>, filter: &NextFilter) -> BridgeResult<Option<Lease>> {
        let out: ClaimOut = self.call(
            "board_claim",
            json!({
                "ttl_ms": filter.ttl_ms,
                "label": filter.label,
                "states": filter.states,
                "parent": filter.parent,
            }),
        )?;
        Ok(out.claimed.then_some(out.lease).flatten())
    }

    fn heartbeat(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        fence: i64,
        progress: Option<Value>,
    ) -> BridgeResult<Lease> {
        self.call(
            "board_heartbeat",
            json!({ "id": id, "fence": fence, "progress": progress }),
        )
    }

    fn add_note(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        kind: &str,
        body: &str,
        to: Option<&str>,
        fence: Option<i64>,
    ) -> BridgeResult<Note> {
        self.call(
            "board_note",
            json!({ "id": id, "kind": kind, "body": body, "to": to, "fence": fence }),
        )
    }

    fn complete(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        fence: i64,
        summary: &str,
    ) -> BridgeResult<Ticket> {
        self.call(
            "board_complete",
            json!({ "id": id, "fence": fence, "summary": summary }),
        )
    }

    fn handoff(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        fence: i64,
        note: &str,
        to: Option<&str>,
    ) -> BridgeResult<Ticket> {
        self.call(
            "board_handoff",
            json!({ "id": id, "fence": fence, "note": note, "to": to }),
        )
    }

    fn release(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        fence: i64,
        reason: &str,
    ) -> BridgeResult<Ticket> {
        self.call(
            "board_release",
            json!({ "id": id, "fence": fence, "reason": reason }),
        )
    }

    fn force_release(&self, _actor: &Actor<'_>, id: &str, reason: &str) -> BridgeResult<Ticket> {
        self.call("board_force_release", json!({ "id": id, "reason": reason }))
    }

    fn update(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        version: i64,
        patch: Patch,
    ) -> BridgeResult<Ticket> {
        let (assignee, clear_assignee) = match patch.assignee {
            Some(Some(who)) => (Some(who), false),
            Some(None) => (None, true),
            None => (None, false),
        };
        self.call(
            "board_update",
            json!({
                "id": id,
                "version": version,
                "title": patch.title,
                "description": patch.description,
                "priority": patch.priority,
                "labels": patch.labels,
                "assignee": assignee,
                "clear_assignee": clear_assignee,
                "state": patch.state,
            }),
        )
    }

    fn add_dep(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>> {
        self.call(
            "board_dep",
            json!({ "id": id, "depends_on": depends_on, "kind": kind.as_str(), "remove": false }),
        )
    }

    fn remove_dep(
        &self,
        _actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>> {
        self.call(
            "board_dep",
            json!({ "id": id, "depends_on": depends_on, "kind": kind.as_str(), "remove": true }),
        )
    }

    fn reap(&self) -> BridgeResult<usize> {
        Ok(0)
    }

    fn verify_chain(&self) -> BridgeResult<ChainStatus> {
        self.call("board_verify", json!({}))
    }
}
