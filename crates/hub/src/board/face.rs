//! The board as the command line sees it: one set of operations, two faces. `Board` is
//! the local store in `.devagotchi/`; `remote::Remote` is the same operations against the
//! hosted hub's `board_*` tools, as the signed-in account (B11: a bound project's board of
//! record is the hub, which arbitrates claims, leases and fences). The command line renders
//! the same records either way and never knows which it got.

use devagotchi_contracts::BridgeResult;

use super::{
    Actor, Board, ChainStatus, Dep, DepKind, Filter, Lease, NewTicket, NextFilter, Note, Patch,
    Prime, Ticket, TicketView,
};

/// Where the holder filter means "whoever I am on this board": the local face knows the
/// actor's name; the remote face lets the hub resolve it (`@me`), since on a shared board
/// the hub spells a seat `<account>/<seat>`.
pub const ME: &str = "@me";

pub trait Face {
    /// Milliseconds since the epoch, as this face reckons time.
    fn now(&self) -> i64;
    fn prefix(&self) -> String;
    /// Where this board lives, for messages: `here` or the hub's URL.
    fn location(&self) -> String;
    fn prime(&self, actor: &Actor<'_>, limit: usize) -> BridgeResult<Prime>;
    fn ready(&self, actor: &Actor<'_>, limit: usize) -> BridgeResult<Vec<Ticket>>;
    /// `filter.holder == Some(ME)` means the caller, on either face.
    fn list(&self, actor: &Actor<'_>, filter: &Filter) -> BridgeResult<Vec<Ticket>>;
    fn get(&self, id: &str) -> BridgeResult<TicketView>;
    fn create(&self, actor: &Actor<'_>, new: NewTicket) -> BridgeResult<Ticket>;
    fn claim(&self, actor: &Actor<'_>, id: &str, ttl_ms: Option<i64>) -> BridgeResult<Lease>;
    fn claim_next(&self, actor: &Actor<'_>, filter: &NextFilter) -> BridgeResult<Option<Lease>>;
    fn heartbeat(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        progress: Option<serde_json::Value>,
    ) -> BridgeResult<Lease>;
    fn add_note(
        &self,
        actor: &Actor<'_>,
        id: &str,
        kind: &str,
        body: &str,
        to: Option<&str>,
        fence: Option<i64>,
    ) -> BridgeResult<Note>;
    fn complete(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        summary: &str,
    ) -> BridgeResult<Ticket>;
    fn handoff(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        note: &str,
        to: Option<&str>,
    ) -> BridgeResult<Ticket>;
    fn release(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        reason: &str,
    ) -> BridgeResult<Ticket>;
    fn force_release(&self, actor: &Actor<'_>, id: &str, reason: &str) -> BridgeResult<Ticket>;
    fn update(
        &self,
        actor: &Actor<'_>,
        id: &str,
        version: i64,
        patch: Patch,
    ) -> BridgeResult<Ticket>;
    fn add_dep(
        &self,
        actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>>;
    fn remove_dep(
        &self,
        actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>>;
    /// Expire lapsed leases; the hub does its own, so the remote face returns 0.
    fn reap(&self) -> BridgeResult<usize>;
    fn verify_chain(&self) -> BridgeResult<ChainStatus>;
}

impl Face for Board {
    fn now(&self) -> i64 {
        Board::now(self)
    }

    fn prefix(&self) -> String {
        Board::prefix(self).to_string()
    }

    fn location(&self) -> String {
        "here".into()
    }

    fn prime(&self, actor: &Actor<'_>, limit: usize) -> BridgeResult<Prime> {
        Board::prime(self, actor, limit)
    }

    fn ready(&self, actor: &Actor<'_>, limit: usize) -> BridgeResult<Vec<Ticket>> {
        Board::ready(self, actor, limit)
    }

    fn list(&self, actor: &Actor<'_>, filter: &Filter) -> BridgeResult<Vec<Ticket>> {
        if filter.holder.as_deref() == Some(ME) {
            let mine = Filter {
                holder: Some(actor.id.to_string()),
                ..filter.clone()
            };
            return Board::list(self, &mine);
        }
        Board::list(self, filter)
    }

    fn get(&self, id: &str) -> BridgeResult<TicketView> {
        Board::get(self, id)
    }

    fn create(&self, actor: &Actor<'_>, new: NewTicket) -> BridgeResult<Ticket> {
        Board::create(self, actor, new)
    }

    fn claim(&self, actor: &Actor<'_>, id: &str, ttl_ms: Option<i64>) -> BridgeResult<Lease> {
        Board::claim(self, actor, id, ttl_ms)
    }

    fn claim_next(&self, actor: &Actor<'_>, filter: &NextFilter) -> BridgeResult<Option<Lease>> {
        Board::claim_next(self, actor, filter)
    }

    fn heartbeat(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        progress: Option<serde_json::Value>,
    ) -> BridgeResult<Lease> {
        Board::heartbeat(self, actor, id, fence, progress)
    }

    fn add_note(
        &self,
        actor: &Actor<'_>,
        id: &str,
        kind: &str,
        body: &str,
        to: Option<&str>,
        fence: Option<i64>,
    ) -> BridgeResult<Note> {
        Board::add_note(self, actor, id, kind, body, to, fence)
    }

    fn complete(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        summary: &str,
    ) -> BridgeResult<Ticket> {
        Board::complete(self, actor, id, fence, summary)
    }

    fn handoff(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        note: &str,
        to: Option<&str>,
    ) -> BridgeResult<Ticket> {
        Board::handoff(self, actor, id, fence, note, to)
    }

    fn release(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        reason: &str,
    ) -> BridgeResult<Ticket> {
        Board::release(self, actor, id, fence, reason)
    }

    fn force_release(&self, actor: &Actor<'_>, id: &str, reason: &str) -> BridgeResult<Ticket> {
        Board::force_release(self, actor, id, reason)
    }

    fn update(
        &self,
        actor: &Actor<'_>,
        id: &str,
        version: i64,
        patch: Patch,
    ) -> BridgeResult<Ticket> {
        Board::update(self, actor, id, version, patch)
    }

    fn add_dep(
        &self,
        actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>> {
        Board::add_dep(self, actor, id, depends_on, kind)
    }

    fn remove_dep(
        &self,
        actor: &Actor<'_>,
        id: &str,
        depends_on: &str,
        kind: DepKind,
    ) -> BridgeResult<Vec<Dep>> {
        Board::remove_dep(self, actor, id, depends_on, kind)
    }

    fn reap(&self) -> BridgeResult<usize> {
        Board::reap(self)
    }

    fn verify_chain(&self) -> BridgeResult<ChainStatus> {
        Board::verify_chain(self)
    }
}
