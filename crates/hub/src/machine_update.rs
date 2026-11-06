//! Machines update themselves to the build this hub hands out, and on request
//! (docs/spec-machine-update.md, U4). The hub only asks: a device whose hello said its
//! `build` gets `HubFrame::Update`, and its link does the rest (`crates/link/src/update.rs`),
//! reporting each step back. This file holds what the hub remembers about each update and
//! which machines get one now; `link.rs` sends and receives the frames.

use std::{collections::HashMap, time::Duration};

/// How often the hub looks for machines on another build.
pub const EVERY: Duration = Duration::from_secs(60 * 60);
/// Machines updating at once on one hub, at most.
pub const MAX_AT_ONCE: usize = 20;
/// A machine asked within this many seconds is not asked again automatically, and an
/// update that has said nothing for this long no longer counts as running.
pub const RECENT_SECS: u64 = 60 * 60;

/// One machine's latest update as the hub knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The build it was asked to go to.
    pub target: String,
    /// `requested`, then what the link reports: `waiting_for_jobs`, `downloading`,
    /// `installing`, `done` or `failed`.
    pub state: String,
    pub detail: Option<String>,
    /// When it was asked, in seconds since the Unix epoch.
    pub requested_at: u64,
}

impl Update {
    pub fn new(target: &str, now: u64) -> Self {
        Self {
            target: target.to_string(),
            state: "requested".into(),
            detail: None,
            requested_at: now,
        }
    }

    /// Still under way: not finished, and not silent for longer than [`RECENT_SECS`].
    pub fn running(&self, now: u64) -> bool {
        !matches!(self.state.as_str(), "done" | "failed")
            && now.saturating_sub(self.requested_at) < RECENT_SECS
    }

    /// What `machines` shows for it: `updating` while it runs, `failed: <reason>, back on
    /// <build>` after a failure, nothing once it is done.
    pub fn shown(&self, build: Option<&str>, now: u64) -> Option<String> {
        if self.running(now) {
            return Some("updating".into());
        }
        (self.state == "failed").then(|| {
            let reason = self
                .detail
                .as_deref()
                .unwrap_or("the new build did not come up");
            match build {
                Some(build) => format!("failed: {reason}, back on {build}"),
                None => format!("failed: {reason}"),
            }
        })
    }
}

/// Whether a live machine on `build` is due an automatic update to `version`: it says its
/// build, the build differs, and it was not asked within the last hour.
pub fn due(build: Option<&str>, version: &str, last: Option<&Update>, now: u64) -> bool {
    build.is_some_and(|build| build != version)
        && last.is_none_or(|last| now.saturating_sub(last.requested_at) >= RECENT_SECS)
}

/// The machines among `live` (name and the build its hello said) to ask now: the due ones,
/// in name order, while fewer than [`MAX_AT_ONCE`] updates run on this hub.
pub fn select(
    live: &[(String, Option<String>)],
    version: &str,
    updates: &HashMap<String, Update>,
    now: u64,
) -> Vec<String> {
    let running = updates.values().filter(|u| u.running(now)).count();
    let mut due: Vec<String> = live
        .iter()
        .filter(|(name, build)| due(build.as_deref(), version, updates.get(name), now))
        .map(|(name, _)| name.clone())
        .collect();
    due.sort();
    due.truncate(MAX_AT_ONCE.saturating_sub(running));
    due
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "main-bbbbbbb";

    #[test]
    fn only_a_machine_on_another_build_that_says_its_build_is_due() {
        assert!(due(Some("main-aaaaaaa"), V, None, 10_000));
        assert!(!due(Some(V), V, None, 10_000), "already on it");
        assert!(!due(None, V, None, 10_000), "an old link cannot be asked");
        let asked = Update::new(V, 10_000);
        assert!(!due(
            Some("main-aaaaaaa"),
            V,
            Some(&asked),
            10_000 + RECENT_SECS - 1
        ));
        assert!(due(
            Some("main-aaaaaaa"),
            V,
            Some(&asked),
            10_000 + RECENT_SECS
        ));
    }

    #[test]
    fn at_most_twenty_update_at_once_and_finished_ones_free_their_place() {
        let now = 50_000;
        let live: Vec<(String, Option<String>)> = (0..30)
            .map(|i| (format!("m{i:02}"), Some("main-aaaaaaa".to_string())))
            .collect();
        let mut updates = HashMap::new();
        assert_eq!(select(&live, V, &updates, now).len(), MAX_AT_ONCE);
        for i in 0..5 {
            updates.insert(format!("other{i}"), Update::new(V, now));
        }
        assert_eq!(select(&live, V, &updates, now).len(), MAX_AT_ONCE - 5);
        for update in updates.values_mut() {
            update.state = "done".into();
        }
        assert_eq!(select(&live, V, &updates, now).len(), MAX_AT_ONCE);
        // An update silent for an hour no longer holds a place.
        let stale = Update::new(V, now - RECENT_SECS);
        assert!(!stale.running(now));
    }

    #[test]
    fn machines_shows_updating_then_the_failure_and_the_build_it_is_back_on() {
        let mut update = Update::new(V, 100);
        assert_eq!(
            update.shown(Some("main-aaaaaaa"), 100).as_deref(),
            Some("updating")
        );
        update.state = "failed".into();
        update.detail = Some("the broker did not report ready".into());
        assert_eq!(
            update.shown(Some("main-aaaaaaa"), 200).as_deref(),
            Some("failed: the broker did not report ready, back on main-aaaaaaa")
        );
        update.state = "done".into();
        assert_eq!(update.shown(Some(V), 200), None);
    }
}
