//! Claims and leases: the protocol that lets several subscriptions work one board
//! without stepping on each other. Ported from the Go hub's `store/claim.go`,
//! `lease.go` and `reaper.go`; the fence is the whole story — every mutation of a
//! held ticket carries it, a lapsed lease hands out a new one, and the old number
//! is never reused, so a worker that wakes up late is rejected, not merged.

use devagotchi_contracts::BridgeResult;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{
    Actor, Board, Settings, Ticket, Writer, fail, insert_note, list_tickets, load_settings,
    load_workflow, read_ticket, store_error,
    workflow::{Lane, Workflow},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Lease {
    pub ticket: Ticket,
    pub fence: i64,
    pub lease_id: String,
    pub lease_expires_at: i64,
    /// About a third of the ttl: heartbeat at least this often.
    pub heartbeat_every_ms: i64,
    /// The caller already held it; the current lease is returned, nothing changed.
    pub already_held: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NextFilter {
    pub label: Option<String>,
    pub states: Vec<String>,
    pub parent: Option<String>,
    pub ttl_ms: Option<i64>,
}

/// Holder and fence must match and the lease must be live. Shared by every
/// mutation of a held ticket.
pub(crate) fn check_held(ticket: &Ticket, actor: &str, fence: Option<i64>) -> BridgeResult<()> {
    let Some(fence) = fence else {
        return Err(fail("stale_fence", "fence is required for a held ticket"));
    };
    if fence != ticket.fence {
        return Err(fail(
            "stale_fence",
            format!(
                "fence {fence} is not current ({}); your lease was lost",
                ticket.fence
            ),
        ));
    }
    if ticket.holder.as_deref() != Some(actor) {
        return Err(fail("not_holder", format!("you do not hold {}", ticket.id)));
    }
    if ticket.lease_expired {
        return Err(fail(
            "lease_expired",
            format!(
                "lease expired at {}; claim again",
                ticket.lease_expires_at.unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

impl Board {
    /// Take a lease on one ticket. Re-claiming a ticket you already hold returns the
    /// current lease; a lapsed lease held by someone else is expired first.
    pub fn claim(&self, actor: &Actor<'_>, id: &str, ttl_ms: Option<i64>) -> BridgeResult<Lease> {
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let settings = load_settings(w.tx)?;
            let mut ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            if ticket.holder.is_some() && !ticket.lease_expired {
                if ticket.holder.as_deref() == Some(actor.id) {
                    let ttl = ticket.lease_ttl_ms.unwrap_or(settings.lease_ttl_ms);
                    return Ok(Lease {
                        fence: ticket.fence,
                        lease_id: ticket.lease_id.clone().unwrap_or_default(),
                        lease_expires_at: ticket.lease_expires_at.unwrap_or_default(),
                        heartbeat_every_ms: beat_every(ttl),
                        already_held: true,
                        ticket,
                    });
                }
                return Err(fail(
                    "held_by_other",
                    format!(
                        "{id} is leased by {} until {}",
                        ticket.holder.clone().unwrap_or_default(),
                        ticket.lease_expires_at.unwrap_or_default()
                    ),
                ));
            }
            if ticket.holder.is_some() {
                ticket = expire(w, ticket, &workflow, &settings)?;
            }
            let Some(lane) = workflow.lane_by_queue(&ticket.state).cloned() else {
                return Err(fail(
                    "not_claimable",
                    format!("state {:?} is not a claimable queue", ticket.state),
                ));
            };
            if !ticket.blocked_by.is_empty() {
                return Err(fail(
                    "blocked",
                    format!("waiting on {}", ticket.blocked_by.join(", ")),
                ));
            }
            if lane.distinct_from_previous && ticket.last_worker.as_deref() == Some(actor.id) {
                return Err(fail(
                    "distinct_required",
                    format!(
                        "the {} lane needs a different principal than the previous worker",
                        lane.queue
                    ),
                ));
            }
            check_wip(w, actor, &settings)?;
            take_lease(w, &workflow, ticket, &lane, &settings, ttl_ms)
        })
    }

    /// Claim the best ticket the actor may take: assigned to them first, then
    /// priority, then age. `None` when nothing is ready.
    pub fn claim_next(
        &self,
        actor: &Actor<'_>,
        filter: &NextFilter,
    ) -> BridgeResult<Option<Lease>> {
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let settings = load_settings(w.tx)?;
            check_wip(w, actor, &settings)?;
            let list = super::Filter {
                label: filter.label.clone(),
                parent: filter.parent.clone(),
                limit: Some(25),
                ..super::Filter::default()
            };
            let candidates = list_tickets(w.tx, &workflow, &list, w.now, Some(actor.id))?;
            for mut ticket in candidates {
                if !filter.states.is_empty() && !filter.states.contains(&ticket.state) {
                    continue;
                }
                if ticket.holder.is_some() {
                    ticket = expire(w, ticket, &workflow, &settings)?;
                }
                let Some(lane) = workflow.lane_by_queue(&ticket.state).cloned() else {
                    continue;
                };
                if !filter.states.is_empty() && !filter.states.contains(&ticket.state) {
                    continue;
                }
                if lane.distinct_from_previous && ticket.last_worker.as_deref() == Some(actor.id) {
                    continue;
                }
                return take_lease(w, &workflow, ticket, &lane, &settings, filter.ttl_ms).map(Some);
            }
            Ok(None)
        })
    }

    /// Extend a live lease by its ttl; optionally record progress for watchers.
    pub fn heartbeat(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        progress: Option<serde_json::Value>,
    ) -> BridgeResult<Lease> {
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let settings = load_settings(w.tx)?;
            let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            check_held(&ticket, actor.id, Some(fence))?;
            let ttl = ticket.lease_ttl_ms.unwrap_or(settings.lease_ttl_ms);
            let progress_json = progress
                .as_ref()
                .map(|value| serde_json::to_string(value).expect("progress json"));
            w.tx.execute(
                "UPDATE tickets SET lease_expires_at = ?2, heartbeat_at = ?3,
                       progress = COALESCE(?4, progress), updated_at = ?3 WHERE id = ?1",
                params![id, w.now + ttl, w.now, progress_json],
            )
            .map_err(store_error)?;
            w.event(
                "ticket.heartbeat",
                Some(id),
                Some(fence),
                serde_json::json!({ "lease_expires_at": w.now + ttl, "progress": progress }),
            )?;
            let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            Ok(Lease {
                fence: ticket.fence,
                lease_id: ticket.lease_id.clone().unwrap_or_default(),
                lease_expires_at: ticket.lease_expires_at.unwrap_or_default(),
                heartbeat_every_ms: beat_every(ttl),
                already_held: true,
                ticket,
            })
        })
    }

    /// Give the ticket back to its lane's release state.
    pub fn release(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        reason: &str,
    ) -> BridgeResult<Ticket> {
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            check_held(&ticket, actor.id, Some(fence))?;
            let to = release_state(&workflow, &ticket);
            clear_lease(
                w,
                &workflow,
                ticket,
                &to,
                "ticket.released",
                serde_json::json!({ "reason": reason }),
            )
        })
    }

    /// Finish the lane: the ticket moves to the lane's `complete_to` and records
    /// the worker, so the next lane can require someone else.
    pub fn complete(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        summary: &str,
    ) -> BridgeResult<Ticket> {
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            check_held(&ticket, actor.id, Some(fence))?;
            let to = workflow
                .lane_by_active(&ticket.state)
                .map(|l| l.complete_to.clone())
                .unwrap_or_else(|| workflow.initial_state.clone());
            if !summary.trim().is_empty() {
                insert_note(w.tx, id, actor.id, "summary", summary.trim(), None, Some(fence), w.now)?;
            }
            let closed_at: Option<i64> = workflow.is_done(&to).then_some(w.now);
            w.tx
                .execute(
                    "UPDATE tickets SET last_worker = ?2, closed_at = COALESCE(?3, closed_at) WHERE id = ?1",
                    params![id, actor.id, closed_at],
                )
                .map_err(store_error)?;
            clear_lease(
                w,
                &workflow,
                ticket,
                &to,
                "ticket.completed",
                serde_json::json!({ "summary": !summary.trim().is_empty() }),
            )
        })
    }

    /// A handoff note, a release, and an assignee, in one transaction.
    pub fn handoff(
        &self,
        actor: &Actor<'_>,
        id: &str,
        fence: i64,
        note: &str,
        to: Option<&str>,
    ) -> BridgeResult<Ticket> {
        let note = note.trim();
        if note.is_empty() {
            return Err(fail(
                "invalid",
                "a handoff needs a note for whoever picks it up",
            ));
        }
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            check_held(&ticket, actor.id, Some(fence))?;
            insert_note(w.tx, id, actor.id, "handoff", note, to, Some(fence), w.now)?;
            w.tx.execute(
                "UPDATE tickets SET assignee = ?2 WHERE id = ?1",
                params![id, to],
            )
            .map_err(store_error)?;
            let release_to = release_state(&workflow, &ticket);
            clear_lease(
                w,
                &workflow,
                ticket,
                &release_to,
                "ticket.handoff",
                serde_json::json!({ "to": to }),
            )
        })
    }

    /// A person takes a lease away. Code rule, not policy: only a human may, and only with a reason.
    pub fn force_release(&self, actor: &Actor<'_>, id: &str, reason: &str) -> BridgeResult<Ticket> {
        if !actor.human {
            return Err(fail("humans_only", "force-release needs a human principal"));
        }
        if reason.trim().chars().count() < 3 {
            return Err(fail(
                "invalid",
                "a reason of at least 3 characters is required",
            ));
        }
        self.write(actor, |w| {
            let workflow = load_workflow(w.tx)?;
            let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
            if ticket.holder.is_none() {
                return Err(fail("not_held", format!("{id} is not held")));
            }
            let to = release_state(&workflow, &ticket);
            let previous = serde_json::json!({
                "previous_holder": ticket.holder,
                "previous_fence": ticket.fence,
                "reason": reason.trim(),
            });
            clear_lease(w, &workflow, ticket, &to, "ticket.force_released", previous)
        })
    }

    /// Expire every lapsed lease. Returns how many. Run it on an interval and lazily
    /// inside claims, as the Go hub did.
    pub fn reap(&self) -> BridgeResult<usize> {
        let system = Actor {
            id: "system",
            human: false,
            wip_limit: None,
        };
        self.write(&system, |w| {
            let workflow = load_workflow(w.tx)?;
            let settings = load_settings(w.tx)?;
            let mut statement = w
                .tx
                .prepare(
                    "SELECT id FROM tickets WHERE holder IS NOT NULL AND lease_expires_at <= ?1 ORDER BY id",
                )
                .map_err(store_error)?;
            let ids = statement
                .query_map(params![w.now], |row| row.get::<_, String>(0))
                .map_err(store_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(store_error)?;
            drop(statement);
            for id in &ids {
                let ticket = read_ticket(w.tx, &workflow, id, w.now)?;
                expire(w, ticket, &workflow, &settings)?;
            }
            Ok(ids.len())
        })
    }
}

fn beat_every(ttl_ms: i64) -> i64 {
    (ttl_ms / 3).max(1_000)
}

fn clamp_ttl(settings: &Settings, asked: Option<i64>) -> i64 {
    asked
        .filter(|ttl| *ttl > 0)
        .unwrap_or(settings.lease_ttl_ms)
        .clamp(settings.lease_min_ttl_ms, settings.lease_max_ttl_ms)
}

fn release_state(workflow: &Workflow, ticket: &Ticket) -> String {
    workflow
        .lane_by_active(&ticket.state)
        .map(|l| l.release_to.clone())
        .unwrap_or_else(|| workflow.initial_state.clone())
}

fn check_wip(w: &Writer<'_>, actor: &Actor<'_>, settings: &Settings) -> BridgeResult<()> {
    let limit = actor.wip_limit.unwrap_or(settings.wip_limit_default);
    if limit <= 0 {
        return Ok(());
    }
    let held: i64 =
        w.tx.query_row(
            "SELECT count(*) FROM tickets WHERE holder = ?1 AND lease_expires_at > ?2",
            params![actor.id, w.now],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    if held >= limit {
        return Err(fail(
            "wip_limit",
            format!("you already hold {held} ticket(s); the WIP limit is {limit}"),
        ));
    }
    Ok(())
}

fn take_lease(
    w: &Writer<'_>,
    workflow: &Workflow,
    ticket: Ticket,
    lane: &Lane,
    settings: &Settings,
    asked_ttl: Option<i64>,
) -> BridgeResult<Lease> {
    let ttl = clamp_ttl(settings, asked_ttl);
    let fence = w.next_fence()?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|error| fail("board_random", error.to_string()))?;
    let lease_id = hex::encode(random);
    let expires_at = w.now + ttl;
    w.tx.execute(
        "UPDATE tickets SET holder = ?2, lease_id = ?3, lease_ttl_ms = ?4, lease_expires_at = ?5,
               heartbeat_at = ?6, fence = ?7, state = ?8, version = version + 1, updated_at = ?6
             WHERE id = ?1",
        params![
            ticket.id,
            w.actor,
            lease_id,
            ttl,
            expires_at,
            w.now,
            fence,
            lane.active
        ],
    )
    .map_err(store_error)?;
    w.event(
        "ticket.claimed",
        Some(&ticket.id),
        Some(fence),
        serde_json::json!({
            "from": ticket.state,
            "to": lane.active,
            "ttl_ms": ttl,
            "lease_expires_at": expires_at,
        }),
    )?;
    let ticket = read_ticket(w.tx, workflow, &ticket.id, w.now)?;
    Ok(Lease {
        fence,
        lease_id,
        lease_expires_at: expires_at,
        heartbeat_every_ms: beat_every(ttl),
        already_held: false,
        ticket,
    })
}

/// End a lapsed lease: back to the lane's release state, or to attention after
/// too many expiries. The fence stays burned.
fn expire(
    w: &Writer<'_>,
    ticket: Ticket,
    workflow: &Workflow,
    settings: &Settings,
) -> BridgeResult<Ticket> {
    let count = ticket.expiry_count + 1;
    let mut to = release_state(workflow, &ticket);
    if settings.max_lease_expiries > 0 && count >= settings.max_lease_expiries {
        to = workflow.attention_state.clone();
    }
    w.tx.execute(
        "UPDATE tickets SET expiry_count = ?2 WHERE id = ?1",
        params![ticket.id, count],
    )
    .map_err(store_error)?;
    let data = serde_json::json!({
        "previous_holder": ticket.holder,
        "expiry_count": count,
    });
    clear_lease(w, workflow, ticket, &to, "lease.expired", data)
}

/// Drop the claim, move the ticket, log the event, return the fresh row.
fn clear_lease(
    w: &Writer<'_>,
    workflow: &Workflow,
    ticket: Ticket,
    to: &str,
    action: &str,
    mut data: serde_json::Value,
) -> BridgeResult<Ticket> {
    w.tx
        .execute(
            "UPDATE tickets SET holder = NULL, lease_id = NULL, lease_ttl_ms = NULL, lease_expires_at = NULL,
               state = ?2, version = version + 1, updated_at = ?3 WHERE id = ?1",
            params![ticket.id, to, w.now],
        )
        .map_err(store_error)?;
    if let serde_json::Value::Object(map) = &mut data {
        map.insert("from".into(), serde_json::json!(ticket.state));
        map.insert("to".into(), serde_json::json!(to));
    }
    w.event(action, Some(&ticket.id), Some(ticket.fence), data)?;
    read_ticket(w.tx, workflow, &ticket.id, w.now)
}
