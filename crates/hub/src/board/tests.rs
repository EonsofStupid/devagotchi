//! The protocol, exercised the way the Go hub's `hubtest` did: fencing, expiry,
//! the distinct-reviewer lane, readiness through dependencies, hierarchy, the chain.

use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use super::{
    Actor, Board, DepKind, Filter, NewTicket, NextFilter, Patch, Settings, StateKind, Workflow,
};

struct Fixture {
    board: Board,
    now: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let now = Arc::new(AtomicI64::new(1_700_000_000_000));
    let clock = now.clone();
    let board = Board::open_with_clock(
        &dir.path().join("board.sqlite3"),
        "devagotchi",
        Arc::new(move || clock.load(Ordering::SeqCst)),
    )
    .expect("open board");
    Fixture {
        board,
        now,
        _dir: dir,
    }
}

impl Fixture {
    fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }

    fn new_ticket(&self, actor: &Actor<'_>, title: &str) -> String {
        self.board
            .create(
                actor,
                NewTicket {
                    title: title.into(),
                    ..NewTicket::default()
                },
            )
            .expect("create")
            .id
    }
}

#[test]
fn open_seeds_meta_settings_and_workflow_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("board.sqlite3");
    let board = Board::open(&path, "devagotchi").expect("open");
    assert_eq!(board.prefix(), "deva");
    assert_eq!(board.settings().expect("settings"), Settings::default());
    assert_eq!(
        board.workflow().expect("workflow"),
        Workflow::default_workflow()
    );
    drop(board);
    let again = Board::open(&path, "devagotchi").expect("reopen");
    assert_eq!(again.prefix(), "deva");
    drop(again);
    let wrong = match Board::open(&path, "other") {
        Ok(_) => panic!("another project must be refused"),
        Err(error) => error,
    };
    assert_eq!(wrong.code, "board_mismatch");
}

#[test]
fn ids_are_prefixed_hashes_and_children_are_hierarchical() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let epic = f
        .board
        .create(
            &jesse,
            NewTicket {
                title: "Board in the Rust hub".into(),
                kind: Some("epic".into()),
                ..NewTicket::default()
            },
        )
        .expect("epic");
    assert!(epic.id.starts_with("deva-"), "{}", epic.id);
    assert_eq!(epic.id.len(), "deva-".len() + 4);
    let child_a = f
        .board
        .create(
            &jesse,
            NewTicket {
                title: "store".into(),
                parent: Some(epic.id.clone()),
                ..NewTicket::default()
            },
        )
        .expect("child a");
    let child_b = f
        .board
        .create(
            &jesse,
            NewTicket {
                title: "mcp tools".into(),
                parent: Some(epic.id.clone()),
                ..NewTicket::default()
            },
        )
        .expect("child b");
    assert_eq!(child_a.id, format!("{}.1", epic.id));
    assert_eq!(child_b.id, format!("{}.2", epic.id));
    let grandchild = f
        .board
        .create(
            &jesse,
            NewTicket {
                title: "schema".into(),
                parent: Some(child_a.id.clone()),
                ..NewTicket::default()
            },
        )
        .expect("grandchild");
    assert_eq!(grandchild.id, format!("{}.1.1", epic.id));
    let view = f.board.get(&epic.id).expect("view");
    assert_eq!(view.children, vec![child_a.id.clone(), child_b.id.clone()]);
    assert_eq!(view.dependents.len(), 2);
    assert!(view.dependents.iter().all(|d| d.kind == DepKind::Parent));
    // Hash ids from the same actor and title in the same millisecond still differ.
    let one = f.new_ticket(&jesse, "same title");
    let two = f.new_ticket(&jesse, "same title");
    assert_ne!(one, two);
}

#[test]
fn claim_heartbeat_complete_moves_through_the_lanes() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let claude = Actor::agent("claude");
    let codex = Actor::agent("codex");
    let id = f.new_ticket(&jesse, "port the claim protocol");

    let lease = f.board.claim(&claude, &id, None).expect("claim");
    assert_eq!(lease.ticket.state, "in_progress");
    assert_eq!(lease.ticket.holder.as_deref(), Some("claude"));
    assert_eq!(lease.fence, 1);
    assert_eq!(
        lease.heartbeat_every_ms,
        Settings::default().lease_ttl_ms / 3
    );
    assert!(!lease.already_held);

    let again = f
        .board
        .claim(&claude, &id, None)
        .expect("re-claim is idempotent");
    assert!(again.already_held);
    assert_eq!(again.fence, 1);

    let other = f.board.claim(&codex, &id, None).expect_err("held by other");
    assert_eq!(other.code, "held_by_other");

    f.advance(60_000);
    let beat = f
        .board
        .heartbeat(
            &claude,
            &id,
            lease.fence,
            Some(serde_json::json!({ "step": "tests" })),
        )
        .expect("heartbeat");
    assert_eq!(
        beat.lease_expires_at,
        f.now.load(Ordering::SeqCst) + Settings::default().lease_ttl_ms
    );
    assert_eq!(
        beat.ticket.progress,
        Some(serde_json::json!({ "step": "tests" }))
    );

    let stale = f
        .board
        .heartbeat(&claude, &id, 99, None)
        .expect_err("wrong fence");
    assert_eq!(stale.code, "stale_fence");
    let not_holder = f
        .board
        .complete(&codex, &id, lease.fence, "nope")
        .expect_err("codex does not hold it");
    assert_eq!(not_holder.code, "not_holder");

    let reviewed = f
        .board
        .complete(&claude, &id, lease.fence, "store and tests landed")
        .expect("complete");
    assert_eq!(reviewed.state, "review");
    assert_eq!(reviewed.holder, None);
    assert_eq!(reviewed.last_worker.as_deref(), Some("claude"));
    assert_eq!(reviewed.closed_at, None);

    // The author may not review their own work; the lane says so as data.
    let same = f
        .board
        .claim(&claude, &id, None)
        .expect_err("distinct reviewer");
    assert_eq!(same.code, "distinct_required");
    let review = f.board.claim(&codex, &id, None).expect("codex reviews");
    assert_eq!(review.ticket.state, "reviewing");
    assert_eq!(review.fence, 2, "fences never repeat");
    let done = f
        .board
        .complete(&codex, &id, review.fence, "looks right")
        .expect("review complete");
    assert_eq!(done.state, "done");
    assert!(done.closed_at.is_some());

    let view = f.board.get(&id).expect("view");
    let kinds: Vec<&str> = view.notes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, vec!["summary", "summary"]);
    let chain = f.board.verify_chain().expect("verify");
    assert!(chain.ok);
    assert_eq!(
        chain.entries, 6,
        "created, claimed, heartbeat, completed, claimed, completed"
    );
}

#[test]
fn expired_lease_goes_back_to_ready_and_the_old_fence_is_rejected() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let claude = Actor::agent("claude");
    let codex = Actor::agent("codex");
    let id = f.new_ticket(&jesse, "something slow");
    let lease = f.board.claim(&claude, &id, Some(60_000)).expect("claim");
    assert_eq!(lease.ticket.lease_ttl_ms, Some(60_000));

    f.advance(60_001);
    let lapsed = f.board.get(&id).expect("get").ticket;
    assert!(lapsed.lease_expired, "lapsed but not yet reaped");

    // Codex claims it; the expiry happens lazily inside the claim and the fence moves on.
    let taken = f
        .board
        .claim(&codex, &id, None)
        .expect("claim after expiry");
    assert_eq!(taken.fence, 2);
    assert_eq!(taken.ticket.expiry_count, 1);
    let late = f
        .board
        .heartbeat(&claude, &id, lease.fence, None)
        .expect_err("the woken-up old holder is rejected");
    assert_eq!(late.code, "stale_fence");
    let late_complete = f
        .board
        .complete(&claude, &id, lease.fence, "done?")
        .expect_err("same for complete");
    assert_eq!(late_complete.code, "stale_fence");
}

#[test]
fn reaper_sends_a_ticket_to_attention_after_too_many_expiries() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let agent = Actor::agent("flaky");
    let id = f.new_ticket(&jesse, "keeps dying");
    for expiry in 1..=3 {
        f.board.claim(&agent, &id, Some(30_000)).expect("claim");
        f.advance(30_001);
        assert_eq!(f.board.reap().expect("reap"), 1);
        let ticket = f.board.get(&id).expect("get").ticket;
        assert_eq!(ticket.expiry_count, expiry);
        if expiry < 3 {
            assert_eq!(ticket.state, "ready");
        } else {
            assert_eq!(ticket.state, "needs_attention");
        }
    }
    assert_eq!(f.board.reap().expect("nothing left"), 0);
    let not_claimable = f
        .board
        .claim(&agent, &id, None)
        .expect_err("attention is not a queue");
    assert_eq!(not_claimable.code, "not_claimable");
    // A person puts it back.
    let ticket = f.board.get(&id).expect("get").ticket;
    let back = f
        .board
        .update(
            &jesse,
            &id,
            ticket.version,
            Patch {
                state: Some("ready".into()),
                ..Patch::default()
            },
        )
        .expect("back to ready");
    assert_eq!(back.state, "ready");
}

#[test]
fn ready_excludes_blocked_tickets_until_the_blocker_is_done() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let claude = Actor::agent("claude");
    let schema = f.new_ticket(&jesse, "schema");
    let tools = f.new_ticket(&jesse, "mcp tools");
    f.board
        .add_dep(&jesse, &tools, &schema, DepKind::Blocks)
        .expect("dep");
    let cycle = f
        .board
        .add_dep(&jesse, &schema, &tools, DepKind::Blocks)
        .expect_err("cycle refused");
    assert_eq!(cycle.code, "dependency_cycle");

    let ready: Vec<String> = f
        .board
        .ready(&claude, 10)
        .expect("ready")
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ready, vec![schema.clone()]);
    let blocked = f.board.claim(&claude, &tools, None).expect_err("blocked");
    assert_eq!(blocked.code, "blocked");

    let next = f
        .board
        .claim_next(&claude, &NextFilter::default())
        .expect("claim_next")
        .expect("something ready");
    assert_eq!(next.ticket.id, schema);
    f.board
        .complete(&claude, &schema, next.fence, "schema landed")
        .expect("complete");
    // Review is not done: still blocking.
    assert!(
        f.board
            .get(&tools)
            .expect("get")
            .ticket
            .blocked_by
            .contains(&schema)
    );
    let review = f
        .board
        .claim(&Actor::agent("codex"), &schema, None)
        .expect("review");
    f.board
        .complete(&Actor::agent("codex"), &schema, review.fence, "ok")
        .expect("done");
    assert!(
        f.board
            .get(&tools)
            .expect("get")
            .ticket
            .blocked_by
            .is_empty()
    );
    let next = f
        .board
        .claim_next(&claude, &NextFilter::default())
        .expect("claim_next")
        .expect("tools now ready");
    assert_eq!(next.ticket.id, tools);
}

#[test]
fn assigned_tickets_go_to_their_assignee_first_and_only() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let claude = Actor::agent("claude");
    let codex = Actor::agent("codex");
    let anyone = f.new_ticket(&jesse, "anyone");
    let mine = f
        .board
        .create(
            &jesse,
            NewTicket {
                title: "for codex".into(),
                assignee: Some("codex".into()),
                priority: Some(3),
                ..NewTicket::default()
            },
        )
        .expect("create")
        .id;
    let claude_sees: Vec<String> = f
        .board
        .ready(&claude, 10)
        .expect("ready")
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(claude_sees, vec![anyone.clone()]);
    let codex_next = f
        .board
        .claim_next(&codex, &NextFilter::default())
        .expect("claim_next")
        .expect("ready");
    assert_eq!(
        codex_next.ticket.id, mine,
        "assigned work outranks a better priority"
    );
}

#[test]
fn wip_limit_stops_a_second_claim() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let one = f.new_ticket(&jesse, "one");
    let two = f.new_ticket(&jesse, "two");
    let three = f.new_ticket(&jesse, "three");
    let agent = Actor::agent("claude");
    f.board.claim(&agent, &one, None).expect("first");
    f.board
        .claim(&agent, &two, None)
        .expect("second (default limit is 2)");
    let third = f
        .board
        .claim(&agent, &three, None)
        .expect_err("over the limit");
    assert_eq!(third.code, "wip_limit");
    let none = f
        .board
        .claim_next(&agent, &NextFilter::default())
        .expect_err("claim_next also checks WIP");
    assert_eq!(none.code, "wip_limit");
    let generous = Actor {
        id: "claude",
        human: false,
        wip_limit: Some(0),
    };
    f.board
        .claim(&generous, &three, None)
        .expect("0 = unlimited");
}

#[test]
fn handoff_notes_releases_and_assigns_in_one_step() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let claude = Actor::agent("claude");
    let id = f.new_ticket(&jesse, "cross-vendor");
    let lease = f.board.claim(&claude, &id, None).expect("claim");
    let empty = f
        .board
        .handoff(&claude, &id, lease.fence, "   ", Some("codex"))
        .expect_err("a handoff needs a note");
    assert_eq!(empty.code, "invalid");
    let handed = f
        .board
        .handoff(
            &claude,
            &id,
            lease.fence,
            "tests are red on Windows; see note 1",
            Some("codex"),
        )
        .expect("handoff");
    assert_eq!(handed.state, "ready");
    assert_eq!(handed.holder, None);
    assert_eq!(handed.assignee.as_deref(), Some("codex"));
    let view = f.board.get(&id).expect("view");
    assert_eq!(view.notes.len(), 1);
    assert_eq!(view.notes[0].kind, "handoff");
    assert_eq!(view.notes[0].to_principal.as_deref(), Some("codex"));
    assert_eq!(view.notes[0].fence, Some(lease.fence));
}

#[test]
fn force_release_is_for_humans_with_a_reason() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let claude = Actor::agent("claude");
    let id = f.new_ticket(&jesse, "stuck");
    f.board.claim(&claude, &id, None).expect("claim");
    let agent = f
        .board
        .force_release(&Actor::agent("codex"), &id, "mine now")
        .expect_err("agents may not");
    assert_eq!(agent.code, "humans_only");
    let short = f
        .board
        .force_release(&jesse, &id, "x")
        .expect_err("reason too short");
    assert_eq!(short.code, "invalid");
    let freed = f
        .board
        .force_release(&jesse, &id, "claude's session died")
        .expect("forced");
    assert_eq!(freed.holder, None);
    assert_eq!(freed.state, "ready");
    let events = f.board.events(0, 100).expect("events");
    let forced = events
        .iter()
        .find(|e| e.action == "ticket.force_released")
        .expect("event");
    assert_eq!(forced.data["previous_holder"], "claude");
    assert_eq!(forced.data["previous_fence"], 1);
}

#[test]
fn update_needs_the_current_version_and_a_declared_transition() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let id = f.new_ticket(&jesse, "edit me");
    let ticket = f.board.get(&id).expect("get").ticket;
    let stale = f
        .board
        .update(
            &jesse,
            &id,
            ticket.version + 5,
            Patch {
                title: Some("renamed".into()),
                ..Patch::default()
            },
        )
        .expect_err("version mismatch");
    assert_eq!(stale.code, "version_conflict");
    let renamed = f
        .board
        .update(
            &jesse,
            &id,
            ticket.version,
            Patch {
                title: Some("renamed".into()),
                labels: Some(vec!["Hub".into(), "hub".into(), " rust ".into()]),
                ..Patch::default()
            },
        )
        .expect("update");
    assert_eq!(renamed.title, "renamed");
    assert_eq!(renamed.labels, vec!["hub", "rust"]);
    assert_eq!(renamed.version, ticket.version + 1);
    let illegal = f
        .board
        .update(
            &jesse,
            &id,
            renamed.version,
            Patch {
                state: Some("done".into()),
                ..Patch::default()
            },
        )
        .expect_err("ready -> done is not a transition");
    assert_eq!(illegal.code, "transition_not_allowed");
    let active = f
        .board
        .update(
            &jesse,
            &id,
            renamed.version,
            Patch {
                state: Some("in_progress".into()),
                ..Patch::default()
            },
        )
        .expect_err("active states are lease operations");
    assert_eq!(active.code, "use_lease_ops");
    let blocked = f
        .board
        .update(
            &jesse,
            &id,
            renamed.version,
            Patch {
                state: Some("blocked".into()),
                ..Patch::default()
            },
        )
        .expect("* -> blocked");
    assert_eq!(blocked.state, "blocked");
    let claude = Actor::agent("claude");
    let held_err = {
        let ready = f
            .board
            .update(
                &jesse,
                &id,
                blocked.version,
                Patch {
                    state: Some("ready".into()),
                    ..Patch::default()
                },
            )
            .expect("unblock");
        f.board.claim(&claude, &id, None).expect("claim");
        let held = f.board.get(&id).expect("get").ticket;
        f.board
            .update(
                &jesse,
                &id,
                held.version,
                Patch {
                    state: Some("cancelled".into()),
                    ..Patch::default()
                },
            )
            .expect_err("held tickets move only through lease ops");
        let _ = ready;
        f.board
            .update(
                &jesse,
                &id,
                held.version,
                Patch {
                    state: Some("cancelled".into()),
                    ..Patch::default()
                },
            )
            .expect_err("still held")
    };
    assert_eq!(held_err.code, "use_lease_ops");
}

#[test]
fn list_filters_by_label_text_state_and_open_only() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let a = f
        .board
        .create(
            &jesse,
            NewTicket {
                title: "Windows installer".into(),
                labels: vec!["windows".into()],
                ..NewTicket::default()
            },
        )
        .expect("a")
        .id;
    let b = f
        .board
        .create(
            &jesse,
            NewTicket {
                title: "Linux installer".into(),
                description: "one script for any server".into(),
                labels: vec!["linux".into()],
                ..NewTicket::default()
            },
        )
        .expect("b")
        .id;
    let by_label = f
        .board
        .list(&Filter {
            label: Some("linux".into()),
            ..Filter::default()
        })
        .expect("list");
    assert_eq!(by_label.iter().map(|t| &t.id).collect::<Vec<_>>(), vec![&b]);
    let by_text = f
        .board
        .list(&Filter {
            text: Some("any server".into()),
            ..Filter::default()
        })
        .expect("list");
    assert_eq!(by_text.iter().map(|t| &t.id).collect::<Vec<_>>(), vec![&b]);
    let like_safe = f
        .board
        .list(&Filter {
            text: Some("100%".into()),
            ..Filter::default()
        })
        .expect("a literal percent sign matches nothing here");
    assert!(like_safe.is_empty());
    let ticket = f.board.get(&a).expect("get").ticket;
    f.board
        .update(
            &jesse,
            &a,
            ticket.version,
            Patch {
                state: Some("cancelled".into()),
                ..Patch::default()
            },
        )
        .expect("cancel");
    let open = f
        .board
        .list(&Filter {
            open_only: true,
            ..Filter::default()
        })
        .expect("list");
    assert_eq!(open.iter().map(|t| &t.id).collect::<Vec<_>>(), vec![&b]);
    let cancelled = f
        .board
        .list(&Filter {
            state: Some("cancelled".into()),
            ..Filter::default()
        })
        .expect("list");
    assert_eq!(
        cancelled.iter().map(|t| &t.id).collect::<Vec<_>>(),
        vec![&a]
    );
}

#[test]
fn notes_are_append_only_and_the_chain_notices_tampering() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let id = f.new_ticket(&jesse, "audited");
    f.board
        .add_note(&jesse, &id, "note", "first", None, None)
        .expect("note");
    {
        let connection = f.board.connection.lock().expect("lock");
        let refused = connection.execute("DELETE FROM notes", []);
        assert!(refused.is_err(), "notes are append-only");
        let refused = connection.execute("UPDATE events SET actor = 'mallory' WHERE seq = 1", []);
        assert!(refused.is_err(), "events are append-only");
        // Only a raw edit outside the triggers could break the chain; simulate one
        // by dropping the trigger first, as an attacker with the file would.
        connection
            .execute_batch(
                "DROP TRIGGER events_no_update; UPDATE events SET actor = 'mallory' WHERE seq = 1;",
            )
            .expect("tamper");
    }
    let status = f.board.verify_chain().expect("verify");
    assert!(!status.ok);
    assert_eq!(status.first_broken, Some(1));
}

#[test]
fn workflow_replacement_is_validated_against_live_tickets() {
    let f = fixture();
    let jesse = Actor::human("jesse");
    let id = f.new_ticket(&jesse, "in ready");
    let mut workflow = Workflow::default_workflow();
    workflow.states.retain(|s| s.key != "ready");
    workflow.initial_state = "backlog".into();
    workflow.lanes[0].queue = "backlog".into();
    workflow.lanes[0].release_to = "backlog".into();
    workflow
        .transitions
        .retain(|t| t.from != "ready" && t.to != "ready");
    assert!(workflow.validate().is_ok());
    let refused = f
        .board
        .put_workflow(&jesse, &workflow)
        .expect_err("a ticket sits in ready");
    assert_eq!(refused.code, "invalid");
    let claude = Actor::agent("claude");
    let ticket = f.board.get(&id).expect("get").ticket;
    f.board
        .update(
            &jesse,
            &id,
            ticket.version,
            Patch {
                state: Some("backlog".into()),
                ..Patch::default()
            },
        )
        .expect("move it");
    f.board
        .put_workflow(&jesse, &workflow)
        .expect("now it fits");
    let lease = f
        .board
        .claim(&claude, &id, None)
        .expect("backlog is the lane queue now");
    assert_eq!(lease.ticket.state, "in_progress");
    assert_eq!(
        f.board.workflow().expect("workflow").kind("in_progress"),
        Some(StateKind::Active)
    );
}

#[test]
fn discovered_work_links_back_to_where_it_was_found() {
    let f = fixture();
    let claude = Actor::agent("claude");
    let jesse = Actor::human("jesse");
    let parent = f.new_ticket(&jesse, "port the store");
    let found = f
        .board
        .create(
            &claude,
            NewTicket {
                title: "rusqlite RETURNING needs 3.35".into(),
                discovered_from: Some(parent.clone()),
                ..NewTicket::default()
            },
        )
        .expect("create");
    let view = f.board.get(&found.id).expect("view");
    assert_eq!(view.deps.len(), 1);
    assert_eq!(view.deps[0].kind, DepKind::DiscoveredFrom);
    assert_eq!(view.deps[0].depends_on, parent);
    assert!(found.blocked_by.is_empty(), "discovered-from never blocks");
}

mod cli {
    use std::{ffi::OsString, path::Path};

    use crate::board::cli::{
        self, EXIT_CONFLICT, EXIT_NOT_FOUND, EXIT_OK, EXIT_UNAVAILABLE, EXIT_USAGE, Invocation,
    };

    fn run(cwd: &Path, actor: Option<&str>, terminal: bool, args: &[&str]) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = cli::run(
            Invocation {
                args: args.iter().map(OsString::from).collect(),
                cwd: cwd.to_path_buf(),
                actor_env: actor.map(str::to_string),
                os_user: Some("jesse".into()),
                stdout_is_terminal: terminal,
                account_file: Some(cwd.join("account.json")),
            },
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text.lines().next().unwrap_or("")).expect("one json line")
    }

    #[test]
    fn init_creates_the_board_and_appends_the_agents_block_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Repo\n").unwrap();
        let (code, out, _) = run(
            dir.path(),
            None,
            false,
            &["init", "--project", "demo", "--agents-md"],
        );
        assert_eq!(code, EXIT_OK, "{out}");
        assert!(dir.path().join(".devagotchi/board.sqlite3").is_file());
        assert!(dir.path().join(".devagotchi/config.json").is_file());
        assert!(dir.path().join(".devagotchi/.gitignore").is_file());
        let agents = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(agents.starts_with("# Repo\n"));
        assert_eq!(agents.matches(cli::AGENTS_MD_START).count(), 1);
        let (code, out, _) = run(dir.path(), None, false, &["init", "--agents-md"]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(json(&out)["agents_md"], "already present");
        assert_eq!(
            json(&out)["project"],
            "demo",
            "the project name is kept on re-init"
        );
        let again = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(again.matches(cli::AGENTS_MD_START).count(), 1);
    }

    #[test]
    fn the_loop_works_from_a_subdirectory_with_a_cached_fence() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run(dir.path(), None, false, &["init", "--project", "demo"]).0,
            EXIT_OK
        );
        let sub = dir.path().join("crates").join("x");
        std::fs::create_dir_all(&sub).unwrap();
        let (code, out, _) = run(
            &sub,
            Some("claude"),
            false,
            &["new", "port it", "--label", "Hub"],
        );
        assert_eq!(code, EXIT_OK, "{out}");
        let id = json(&out)["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("demo-"));
        let (code, out, _) = run(&sub, Some("claude"), false, &["claim"]);
        assert_eq!(code, EXIT_OK, "{out}");
        assert_eq!(json(&out)["ticket"]["id"], id);
        assert!(dir.path().join(".devagotchi/claims/claude.json").is_file());
        // No --fence: the cache supplies it.
        let (code, _, _) = run(&sub, Some("claude"), false, &["beat", &id]);
        assert_eq!(code, EXIT_OK);
        let (code, _, _) = run(
            &sub,
            Some("claude"),
            false,
            &["note", &id, "learned a thing", "--to", "codex"],
        );
        assert_eq!(code, EXIT_OK);
        let (code, out, _) = run(&sub, Some("claude"), false, &["done", &id, "shipped"]);
        assert_eq!(code, EXIT_OK, "{out}");
        assert_eq!(json(&out)["state"], "review");
        assert!(
            !dir.path().join(".devagotchi/claims/claude.json").exists(),
            "cache cleared on done"
        );
        // The author may not review; codex may, and prime tells codex about the note.
        let (code, out, _) = run(&sub, Some("claude"), false, &["claim", &id]);
        assert_eq!(code, EXIT_CONFLICT);
        assert_eq!(json(&out)["error"]["code"], "distinct_required");
        let (code, out, _) = run(&sub, Some("codex"), false, &["prime"]);
        assert_eq!(code, EXIT_OK);
        let prime = json(&out);
        assert_eq!(prime["notes_to_me"][0]["ticket"], id);
        assert_eq!(prime["ready"][0]["id"], id);
        let (code, _, _) = run(&sub, Some("codex"), false, &["claim", &id]);
        assert_eq!(code, EXIT_OK);
        let (code, out, _) = run(&sub, Some("codex"), false, &["done", &id, "looks right"]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(json(&out)["state"], "done");
    }

    #[test]
    fn held_beat_all_and_the_hook_pack() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run(dir.path(), None, false, &["init", "--project", "demo"]).0,
            EXIT_OK
        );
        // Nothing held: `held` is an empty list, and empty text (the stop hook keys on that).
        let (code, out, _) = run(dir.path(), Some("claude"), false, &["held"]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(json(&out), serde_json::json!([]));
        let (code, out, _) = run(dir.path(), Some("claude"), true, &["held"]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(out.trim(), "");
        let (code, out, _) = run(dir.path(), Some("claude"), false, &["beat"]);
        assert_eq!(code, EXIT_OK, "{out}");
        assert_eq!(json(&out), serde_json::json!([]));

        for title in ["first", "second"] {
            assert_eq!(run(dir.path(), None, false, &["new", title]).0, EXIT_OK);
        }
        for _ in 0..2 {
            assert_eq!(
                run(dir.path(), Some("claude"), false, &["claim"]).0,
                EXIT_OK
            );
        }
        let (code, out, _) = run(dir.path(), Some("claude"), false, &["held"]);
        assert_eq!(code, EXIT_OK);
        let held = json(&out);
        assert_eq!(held.as_array().unwrap().len(), 2);
        let before: Vec<i64> = held
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["lease_expires_at"].as_i64().unwrap())
            .collect();
        // `beat` with no id extends every held lease, with no cached fence needed.
        std::fs::remove_dir_all(dir.path().join(".devagotchi/claims")).unwrap();
        let (code, out, _) = run(dir.path(), Some("claude"), false, &["beat"]);
        assert_eq!(code, EXIT_OK, "{out}");
        let beaten = json(&out);
        assert_eq!(beaten.as_array().unwrap().len(), 2);
        for (lease, earlier) in beaten.as_array().unwrap().iter().zip(before) {
            assert!(lease["lease_expires_at"].as_i64().unwrap() >= earlier);
        }
        let (_, out, _) = run(dir.path(), Some("claude"), true, &["held"]);
        assert_eq!(out.lines().count(), 2, "{out}");
        assert!(out.contains("fence"), "{out}");

        // The hook pack: three executable scripts, the ignore line, both snippets.
        let (code, out, _) = run(dir.path(), None, false, &["hooks"]);
        assert_eq!(code, EXIT_OK, "{out}");
        let hooks = json(&out);
        for script in ["session-start.sh", "beat.sh", "stop.sh"] {
            let path = dir.path().join(".devagotchi/hooks").join(script);
            assert!(path.is_file(), "{script}");
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(body.starts_with("#!/bin/sh\n"), "{script}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
                    0o111
                );
            }
        }
        assert!(
            std::fs::read_to_string(dir.path().join(".devagotchi/hooks/stop.sh"))
                .unwrap()
                .contains("board held --text")
        );
        assert!(
            std::fs::read_to_string(dir.path().join(".devagotchi/.gitignore"))
                .unwrap()
                .contains(cli::HOOK_MARKER_IGNORE)
        );
        assert!(
            hooks["claude_code"]["hooks"]["Stop"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("stop.sh claude")
        );
        assert!(
            hooks["codex"]["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("session-start.sh codex")
        );
        let (code, out, _) = run(dir.path(), None, true, &["hooks", "--for", "codex"]);
        assert_eq!(code, EXIT_OK);
        assert!(
            out.contains("Codex") && !out.contains("Claude Code"),
            "{out}"
        );
        let (code, _, err) = run(dir.path(), None, false, &["hooks", "--for", "vim"]);
        assert_eq!(code, EXIT_USAGE, "{err}");
    }

    #[test]
    fn exit_codes_and_next_hints_by_failure_class() {
        let dir = tempfile::tempdir().unwrap();
        let (code, _, err) = run(dir.path(), None, false, &["prime"]);
        assert_eq!(code, EXIT_UNAVAILABLE, "no board here");
        assert!(err.is_empty(), "json mode puts the error on stdout");
        assert_eq!(run(dir.path(), None, false, &["init"]).0, EXIT_OK);
        let (code, out, _) = run(dir.path(), None, false, &["show", "nope"]);
        assert_eq!(code, EXIT_NOT_FOUND);
        assert!(
            json(&out)["error"]["next"]
                .as_str()
                .unwrap()
                .contains("board ls")
        );
        let (code, _, err) = run(dir.path(), None, true, &["bogus"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.contains("next: board --help"));
        let (code, _, err) = run(dir.path(), None, true, &["new", "x", "--limit", "many"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.contains("--limit must be an integer"));
        let (code, out, _) = run(dir.path(), Some("claude"), false, &["new", "held"]);
        assert_eq!(code, EXIT_OK);
        let id = json(&out)["id"].as_str().unwrap().to_string();
        assert_eq!(
            run(dir.path(), Some("claude"), false, &["claim", &id]).0,
            EXIT_OK
        );
        let (code, out, _) = run(
            dir.path(),
            Some("claude"),
            false,
            &["beat", &id, "--fence", "99"],
        );
        assert_eq!(code, EXIT_CONFLICT);
        assert_eq!(json(&out)["error"]["code"], "stale_fence");
        // An agent may not force-release; the bare OS user (a person) may.
        let (code, out, _) = run(
            dir.path(),
            Some("codex"),
            false,
            &["force", &id, "stuck session"],
        );
        assert_eq!(code, 3);
        assert_eq!(json(&out)["error"]["code"], "humans_only");
        let (code, out, _) = run(dir.path(), None, false, &["force", &id, "stuck session"]);
        assert_eq!(code, EXIT_OK, "{out}");
        assert_eq!(json(&out)["holder"], serde_json::Value::Null);
    }

    #[test]
    fn prime_text_stays_short_and_names_the_protocol() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run(dir.path(), None, false, &["init", "--project", "demo"]).0,
            EXIT_OK
        );
        for n in 0..30 {
            let title = format!("ticket {n}");
            assert_eq!(run(dir.path(), None, false, &["new", &title]).0, EXIT_OK);
        }
        let (code, out, _) = run(dir.path(), Some("claude"), true, &["prime"]);
        assert_eq!(code, EXIT_OK);
        assert!(out.lines().count() <= 50, "{} lines", out.lines().count());
        assert!(out.contains("protocol: claim → beat → note → done"));
        assert!(out.starts_with("board demo · you are claude"));
        let (code, out, _) = run(dir.path(), Some("claude"), true, &["prime", "--quiet"]);
        assert_eq!(code, EXIT_OK);
        assert!(out.is_empty());
    }

    #[test]
    fn export_writes_one_json_line_per_ticket_and_verify_reads_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run(dir.path(), None, false, &["init", "--project", "demo"]).0,
            EXIT_OK
        );
        assert_eq!(run(dir.path(), None, false, &["new", "a"]).0, EXIT_OK);
        assert_eq!(run(dir.path(), None, false, &["new", "b"]).0, EXIT_OK);
        let (code, out, _) = run(dir.path(), None, false, &["export"]);
        assert_eq!(code, EXIT_OK, "{out}");
        let text = std::fs::read_to_string(dir.path().join(".devagotchi/board.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        let (code, out, _) = run(dir.path(), None, false, &["verify"]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(json(&out)["entries"], 2);
        assert_eq!(json(&out)["ok"], true);
    }
}

#[test]
fn an_approval_waits_for_a_person_and_records_the_verdict() {
    use super::approval::Verdict;
    let f = fixture();
    let agent = Actor::agent("claude");
    let person = Actor::human("jesse");
    let asked = f
        .board
        .request_approval(
            &agent,
            "Restart the hub",
            "systemctl restart devagotchi-hub",
            "new build",
            "start the old build",
        )
        .expect("request");
    assert_eq!(asked.status, "pending");
    assert_eq!(asked.requested_by, "claude");
    assert!(asked.description.starts_with("What: systemctl restart"));
    let ticket = f.board.get(&asked.id).expect("get").ticket;
    assert_eq!(
        ticket.state, "needs_attention",
        "people see it; no agent queue holds it"
    );
    assert!(
        f.board
            .ready(&agent, 10)
            .expect("ready")
            .iter()
            .all(|t| t.id != asked.id)
    );
    let pending = f.board.pending_approvals().expect("pending");
    assert_eq!(pending.len(), 1);

    // Agents cannot decide or move it; only a person, on the version they saw.
    let refused = f
        .board
        .decide_approval(&agent, &asked.id, asked.version, Verdict::Approved, "")
        .unwrap_err();
    assert_eq!(refused.code, "humans_only");
    let moved = f
        .board
        .update(
            &agent,
            &asked.id,
            ticket.version,
            Patch {
                state: Some("ready".into()),
                ..Patch::default()
            },
        )
        .unwrap_err();
    assert_eq!(moved.code, "approval_decide_only");
    let stale = f
        .board
        .decide_approval(&person, &asked.id, asked.version + 1, Verdict::Approved, "")
        .unwrap_err();
    assert_eq!(stale.code, "approval_changed");

    f.advance(5_000);
    let decided = f
        .board
        .decide_approval(
            &person,
            &asked.id,
            asked.version,
            Verdict::Approved,
            "go ahead",
        )
        .expect("decide");
    assert_eq!(decided.status, "approved");
    assert_eq!(decided.decided_by.as_deref(), Some("jesse"));
    assert_eq!(decided.note.as_deref(), Some("go ahead"));
    assert_eq!(f.board.get(&asked.id).expect("get").ticket.state, "done");
    assert!(f.board.pending_approvals().expect("pending").is_empty());
    let again = f
        .board
        .decide_approval(&person, &asked.id, decided.version, Verdict::Denied, "")
        .unwrap_err();
    assert_eq!(again.code, "already_decided");
    let events = f.board.events(0, 100).expect("events");
    assert!(events.iter().any(|e| e.action == "approval.requested"));
    assert!(
        events
            .iter()
            .any(|e| e.action == "approval.decided" && e.data["verdict"] == "approved")
    );
    assert!(f.board.verify_chain().expect("verify").ok);

    let denied = f
        .board
        .request_approval(&agent, "Drop the table", "DROP TABLE x", "", "")
        .expect("request");
    let denied = f
        .board
        .decide_approval(&person, &denied.id, denied.version, Verdict::Denied, "")
        .expect("deny");
    assert_eq!(denied.status, "denied");
    assert_eq!(denied.note, None);
    assert_eq!(
        f.board.get(&denied.id).expect("get").ticket.state,
        "cancelled"
    );
    assert_eq!(
        f.board
            .request_approval(&agent, "", "x", "", "")
            .unwrap_err()
            .code,
        "invalid"
    );
}

/// A bound project (B11): the command line against the real hub, in process, as one
/// account from two machines; the hub arbitrates, the agents are seats.
mod bound {
    use std::{collections::HashMap, ffi::OsString, path::Path, sync::Arc};

    use serde_json::{Value, json};

    use crate::{
        AppState,
        account::{self, Account, Stored},
        audit::Audit,
        auth::Verifier,
        board::cli::{self, EXIT_OK, EXIT_REFUSED, EXIT_UNAVAILABLE, Invocation},
        config::HubConfig,
        devices::Devices,
        hub::Hub,
        link::Links,
    };

    const MEMBER: &str = "300000000000000002";

    async fn hub(dir: &Path) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let config = Arc::new(
            HubConfig::parse(
                &json!({
                    "listen": "127.0.0.1:1",
                    "public_url": url,
                    "auth": {"mode": "dev", "tokens_file": dir.join("tokens.json")},
                    "brokers": [],
                    "grants": [],
                    "account_boards": { "dir": dir.join("accounts") }
                })
                .to_string(),
            )
            .unwrap(),
        );
        let devices = Arc::new(Devices::open(&dir.join("devices.sqlite3")).unwrap());
        let audit = Arc::new(Audit::open(&dir.join("audit.sqlite3")).unwrap());
        let links = Arc::new(Links::new(config.clone(), devices.clone(), audit.clone()));
        let state = AppState {
            verifier: Arc::new(Verifier::Dev(HashMap::from([(
                "tok-member".to_string(),
                MEMBER.to_string(),
            )]))),
            hub: Hub {
                push: None,
                boards: Arc::new(crate::board::mcp::Boards::from_config(&config, dir)),
                config,
                audit,
                links,
            },
            web: None,
        };
        let app = crate::app(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    fn signed_in(path: &Path, token: &str) {
        let now = account::unix_now();
        account::write(
            path,
            &Stored::Account(Account::for_tests(
                "https://id.example",
                MEMBER,
                token,
                now + 3600,
            )),
        )
        .unwrap();
    }

    fn run(cwd: &Path, account: &Path, actor: Option<&str>, args: &[&str]) -> (u8, Value, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = cli::run(
            Invocation {
                args: args.iter().map(OsString::from).collect(),
                cwd: cwd.to_path_buf(),
                actor_env: actor.map(str::to_string),
                os_user: Some("jesse".into()),
                stdout_is_terminal: false,
                account_file: Some(account.to_path_buf()),
            },
            &mut out,
            &mut err,
        );
        // JSON mode (stdout is not a terminal): records and errors alike are on stdout;
        // the third element is everything written, for assertions on the words.
        let out = String::from_utf8(out).unwrap();
        let value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
        (code, value, out + &String::from_utf8(err).unwrap())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_machines_one_account_one_board() {
        let dir = tempfile::tempdir().unwrap();
        let url = hub(dir.path()).await;
        let machine_a = dir.path().join("a").join("demo");
        let machine_b = dir.path().join("b").join("demo");
        std::fs::create_dir_all(&machine_a).unwrap();
        std::fs::create_dir_all(&machine_b).unwrap();
        let account_a = dir.path().join("a").join("account.json");
        let account_b = dir.path().join("b").join("account.json");

        // Not signed in: bind says so, with the command to run, and writes nothing.
        let (code, _, err) = tokio::task::spawn_blocking({
            let (machine_a, account_a, url) = (machine_a.clone(), account_a.clone(), url.clone());
            move || run(&machine_a, &account_a, None, &["bind", "--hub", &url])
        })
        .await
        .unwrap();
        assert_eq!(code, EXIT_REFUSED, "{err}");
        assert!(err.contains("devagotchi login"), "{err}");
        assert!(!machine_a.join(".devagotchi").exists());

        signed_in(&account_a, "tok-member");
        signed_in(&account_b, "tok-member");

        // Machine A binds (the hub makes the board), creates, and claims as seat claude.
        let a = {
            let (machine_a, account_a, url) = (machine_a.clone(), account_a.clone(), url.clone());
            tokio::task::spawn_blocking(move || {
                let (code, bound, err) =
                    run(&machine_a, &account_a, None, &["bind", "--hub", &url]);
                assert_eq!(code, EXIT_OK, "{err}");
                assert_eq!(bound["project"], "demo");
                assert_eq!(bound["created"], true);
                assert!(
                    !machine_a.join(".devagotchi").join("board.sqlite3").exists(),
                    "no local store for a bound project"
                );
                let config: Value = serde_json::from_str(
                    &std::fs::read_to_string(machine_a.join(".devagotchi").join("config.json"))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(config["hub"], url);
                let (code, ticket, err) = run(
                    &machine_a,
                    &account_a,
                    Some("claude"),
                    &["new", "Prove the bound board"],
                );
                assert_eq!(code, EXIT_OK, "{err}");
                let id = ticket["id"].as_str().unwrap().to_string();
                let (code, lease, err) =
                    run(&machine_a, &account_a, Some("claude"), &["claim", &id]);
                assert_eq!(code, EXIT_OK, "{err}");
                assert_eq!(
                    lease["ticket"]["holder"], "claude",
                    "the seat is the holder on the account's board"
                );
                (id, lease["fence"].as_i64().unwrap())
            })
            .await
            .unwrap()
        };
        let (id, fence) = a;

        // Machine B binds the same project (found, not made) and sees the claim at once.
        {
            let (machine_b, account_b, url, id) = (
                machine_b.clone(),
                account_b.clone(),
                url.clone(),
                id.clone(),
            );
            tokio::task::spawn_blocking(move || {
                let (code, bound, err) = run(
                    &machine_b,
                    &account_b,
                    None,
                    &["bind", "--hub", &url, "--project", "demo"],
                );
                assert_eq!(code, EXIT_OK, "{err}");
                assert_eq!(bound["created"], false, "{bound}");
                let (code, prime, err) = run(&machine_b, &account_b, Some("codex"), &["prime"]);
                assert_eq!(code, EXIT_OK, "{err}");
                assert_eq!(prime["actor"], "codex");
                assert_eq!(prime["ready"].as_array().unwrap().len(), 0, "{prime}");
                let (code, _, err) = run(
                    &machine_b,
                    &account_b,
                    Some("codex"),
                    &["beat", &id, "--fence", &fence.to_string()],
                );
                assert_eq!(code, 2, "the other seat does not hold it: {err}");
                assert!(err.contains("not_holder"), "{err}");
                // `held` and `beat` with no id resolve `@me` on the hub.
                let (code, held, err) = run(&machine_b, &account_b, Some("codex"), &["held"]);
                assert_eq!(code, EXIT_OK, "{err}");
                assert_eq!(held.as_array().map(Vec::len), Some(0));
                // The person (no seat) sees the ticket and may force it free.
                let (code, open, _) = run(&machine_b, &account_b, None, &["ls", "--open"]);
                assert_eq!(code, EXIT_OK);
                let open = open.as_array().unwrap();
                let claimed = open.iter().find(|t| t["id"] == id.as_str()).unwrap();
                assert_eq!(claimed["holder"], "claude");
                assert!(
                    open.iter()
                        .any(|t| t["title"] == crate::board::welcome::TITLE
                            && t["state"] == "needs_attention"),
                    "a new account board starts with the getting-started steps (B12): {open:?}"
                );
                let (code, freed, err) = run(
                    &machine_b,
                    &account_b,
                    None,
                    &["force", &id, "the demo moves on"],
                );
                assert_eq!(code, EXIT_OK, "{err}");
                assert_eq!(freed["state"], "ready", "{freed}");
            })
            .await
            .unwrap();
        }

        // Machine A: the stale fence is refused by the hub, as the local store would.
        {
            let (machine_a, account_a, id) = (machine_a.clone(), account_a.clone(), id.clone());
            tokio::task::spawn_blocking(move || {
                let (code, _, err) = run(&machine_a, &account_a, Some("claude"), &["beat", &id]);
                assert_eq!(code, 2, "{err}");
                // The hub's own chain verifies through the remote face.
                let (code, status, err) = run(&machine_a, &account_a, Some("claude"), &["verify"]);
                assert_eq!(code, EXIT_OK, "{err}");
                assert_eq!(status["ok"], true);
                // Unbind: the local store is the board again, and there is none yet.
                let (code, _, err) = run(&machine_a, &account_a, None, &["unbind"]);
                assert_eq!(code, EXIT_OK, "{err}");
                let (code, _, err) = run(&machine_a, &account_a, None, &["ls"]);
                assert_eq!(code, EXIT_UNAVAILABLE, "{err}");
                assert!(err.contains("no_board"), "{err}");
            })
            .await
            .unwrap();
        }

        // Signed out underneath a bound project: every command says what to run.
        std::fs::remove_file(&account_b).unwrap();
        let (code, _, err) = tokio::task::spawn_blocking({
            let (machine_b, account_b) = (machine_b.clone(), account_b.clone());
            move || run(&machine_b, &account_b, Some("codex"), &["prime"])
        })
        .await
        .unwrap();
        assert_eq!(code, EXIT_REFUSED, "{err}");
        assert!(
            err.contains("not_signed_in") && err.contains("devagotchi login"),
            "{err}"
        );
    }
}

#[test]
fn a_new_board_starts_with_the_getting_started_steps_once() {
    let f = fixture();
    let hub = Actor::agent("devagotchi");
    let id = f
        .board
        .welcome_if_new(&hub, "https://app.devagotchi.com/")
        .expect("welcome")
        .expect("a new board is welcomed");
    let tickets = f.board.list(&Filter::default()).expect("list");
    assert_eq!(tickets.len(), 1);
    let welcome = &tickets[0];
    assert_eq!(welcome.id, id);
    assert_eq!(welcome.title, super::welcome::TITLE);
    assert_eq!(welcome.state, "needs_attention");
    assert_eq!(welcome.labels, vec![super::welcome::LABEL.to_string()]);
    assert!(
        welcome
            .description
            .contains("https://app.devagotchi.com/approvals")
            && welcome.description.contains("Send me a test approval"),
        "{}",
        welcome.description
    );

    // An agent priming the board sees it under attention, never as work to claim.
    let claude = Actor::agent("claude");
    let prime = f.board.prime(&claude, 10).expect("prime");
    assert!(prime.ready.is_empty() && prime.held.is_empty());
    assert_eq!(
        prime
            .attention
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        [id.as_str()]
    );

    // Once only, and never on a board that has tickets.
    assert_eq!(
        f.board.welcome_if_new(&hub, "https://x").expect("again"),
        None
    );
    let used = fixture();
    used.new_ticket(&claude, "real work");
    assert_eq!(
        used.board.welcome_if_new(&hub, "https://x").expect("used"),
        None
    );
    assert_eq!(used.board.list(&Filter::default()).expect("list").len(), 1);
    assert!(f.board.verify_chain().expect("chain").ok);
}

#[test]
fn a_committed_change_advances_the_signal_and_an_empty_reap_does_not() {
    let f = fixture();
    let mut changes = f.board.changes();
    assert!(!changes.has_changed().unwrap());
    let id = f.new_ticket(&Actor::human("jesse"), "wake the waiters");
    assert!(changes.has_changed().unwrap(), "a committed event wakes waiters");
    changes.borrow_and_update();
    assert_eq!(f.board.reap().unwrap(), 0);
    assert!(!changes.has_changed().unwrap(), "a write that records nothing wakes nobody");
    assert_eq!(f.board.next_lease_expiry().unwrap(), None);
    f.board
        .claim(&Actor::agent("claude"), &id, Some(30_000))
        .unwrap();
    let now = f.now.load(Ordering::SeqCst);
    assert_eq!(f.board.next_lease_expiry().unwrap(), Some(now + 30_000));
    assert!(changes.has_changed().unwrap());
}
