//! The workflow a board runs: states with a kind the code reasons about, lanes that
//! say what claiming, completing and releasing do, and the transitions a plain
//! update may make. Names are data; kinds are what the store understands.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    /// Claimable: a lane's queue.
    Queue,
    /// Held under a lease; changes only through lease operations.
    Active,
    /// Waiting for a different principal to pick it up (a queue with a role).
    Review,
    /// Parked on something outside the board.
    Blocked,
    /// Finished, including cancelled. Dependencies on it are satisfied.
    Done,
    /// A person has to look: too many expired leases, or an agent asked.
    Attention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub key: String,
    pub label: String,
    pub kind: StateKind,
}

/// Claiming a ticket in `queue` moves it to `active`; completing moves it to
/// `complete_to`; releasing, expiry and handoff move it to `release_to`.
/// `distinct_from_previous` is "reviewer ≠ author" as data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub queue: String,
    pub active: String,
    pub complete_to: String,
    pub release_to: String,
    #[serde(default)]
    pub distinct_from_previous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// A state key, or `*` for any non-active state.
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub initial_state: String,
    pub attention_state: String,
    pub states: Vec<State>,
    pub lanes: Vec<Lane>,
    pub transitions: Vec<Transition>,
}

impl Workflow {
    /// The default: ready → in progress → review → reviewing → done, with backlog,
    /// blocked, needs-attention and cancelled beside it. Same shape the Go hub seeded.
    pub fn default_workflow() -> Self {
        let state = |key: &str, label: &str, kind: StateKind| State {
            key: key.into(),
            label: label.into(),
            kind,
        };
        let transition = |from: &str, to: &str| Transition {
            from: from.into(),
            to: to.into(),
        };
        Self {
            id: "default".into(),
            name: "Default (ready, work, review, done)".into(),
            initial_state: "ready".into(),
            attention_state: "needs_attention".into(),
            states: vec![
                state("backlog", "Backlog", StateKind::Queue),
                state("ready", "Ready", StateKind::Queue),
                state("in_progress", "In progress", StateKind::Active),
                state("review", "Review", StateKind::Review),
                state("reviewing", "Reviewing", StateKind::Active),
                state("blocked", "Blocked", StateKind::Blocked),
                state("needs_attention", "Needs attention", StateKind::Attention),
                state("done", "Done", StateKind::Done),
                state("cancelled", "Cancelled", StateKind::Done),
            ],
            lanes: vec![
                Lane {
                    queue: "ready".into(),
                    active: "in_progress".into(),
                    complete_to: "review".into(),
                    release_to: "ready".into(),
                    distinct_from_previous: false,
                },
                Lane {
                    queue: "review".into(),
                    active: "reviewing".into(),
                    complete_to: "done".into(),
                    release_to: "review".into(),
                    distinct_from_previous: true,
                },
            ],
            transitions: vec![
                transition("backlog", "ready"),
                transition("ready", "backlog"),
                transition("*", "blocked"),
                transition("blocked", "ready"),
                transition("needs_attention", "ready"),
                transition("review", "done"),
                transition("review", "ready"),
                transition("*", "cancelled"),
            ],
        }
    }

    pub fn state(&self, key: &str) -> Option<&State> {
        self.states.iter().find(|s| s.key == key)
    }

    pub fn kind(&self, key: &str) -> Option<StateKind> {
        self.state(key).map(|s| s.kind)
    }

    /// Backlog is a queue kind too, but only lanes make a state claimable.
    pub fn lane_by_queue(&self, key: &str) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.queue == key)
    }

    pub fn lane_by_active(&self, key: &str) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.active == key)
    }

    pub fn is_done(&self, key: &str) -> bool {
        self.kind(key) == Some(StateKind::Done)
    }

    pub fn allows(&self, from: &str, to: &str) -> bool {
        self.transitions
            .iter()
            .any(|t| (t.from == "*" || t.from == from) && t.to == to)
    }

    /// A workflow the store can run: every lane and transition names a known state,
    /// lane queues are claimable kinds, lane actives are active, and the two special
    /// states exist.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn validate(&self) -> Result<(), String> {
        let known = |key: &str| self.state(key).is_some();
        if !known(&self.initial_state) {
            return Err(format!(
                "initial_state {:?} is not a state",
                self.initial_state
            ));
        }
        if !known(&self.attention_state) {
            return Err(format!(
                "attention_state {:?} is not a state",
                self.attention_state
            ));
        }
        for lane in &self.lanes {
            for key in [
                &lane.queue,
                &lane.active,
                &lane.complete_to,
                &lane.release_to,
            ] {
                if !known(key) {
                    return Err(format!("lane names unknown state {key:?}"));
                }
            }
            if !matches!(
                self.kind(&lane.queue),
                Some(StateKind::Queue | StateKind::Review)
            ) {
                return Err(format!(
                    "lane queue {:?} is not a queue or review state",
                    lane.queue
                ));
            }
            if self.kind(&lane.active) != Some(StateKind::Active) {
                return Err(format!(
                    "lane active {:?} is not an active state",
                    lane.active
                ));
            }
        }
        for t in &self.transitions {
            if t.from != "*" && !known(&t.from) {
                return Err(format!("transition from unknown state {:?}", t.from));
            }
            if !known(&t.to) {
                return Err(format!("transition to unknown state {:?}", t.to));
            }
        }
        Ok(())
    }
}
