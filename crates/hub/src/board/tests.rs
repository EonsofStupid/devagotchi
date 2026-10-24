//! The protocol, exercised the way the Go hub's `hubtest` did: fencing, expiry,
//! the distinct-reviewer lane, readiness through dependencies, hierarchy, the chain.

use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use super::{Actor, Board, DepKind, Filter, NewTicket, NextFilter, Patch, Settings, StateKind, Workflow};

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
    assert_eq!(board.workflow().expect("workflow"), Workflow::default_workflow());
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
    assert_eq!(lease.heartbeat_every_ms, Settings::default().lease_ttl_ms / 3);
    assert!(!lease.already_held);

    let again = f.board.claim(&claude, &id, None).expect("re-claim is idempotent");
    assert!(again.already_held);
    assert_eq!(again.fence, 1);

    let other = f.board.claim(&codex, &id, None).expect_err("held by other");
    assert_eq!(other.code, "held_by_other");

    f.advance(60_000);
    let beat = f
        .board
        .heartbeat(&claude, &id, lease.fence, Some(serde_json::json!({ "step": "tests" })))
        .expect("heartbeat");
    assert_eq!(
        beat.lease_expires_at,
        f.now.load(Ordering::SeqCst) + Settings::default().lease_ttl_ms
    );
    assert_eq!(beat.ticket.progress, Some(serde_json::json!({ "step": "tests" })));

    let stale = f.board.heartbeat(&claude, &id, 99, None).expect_err("wrong fence");
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
    let same = f.board.claim(&claude, &id, None).expect_err("distinct reviewer");
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
    assert_eq!(chain.entries, 6, "created, claimed, heartbeat, completed, claimed, completed");
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
    let taken = f.board.claim(&codex, &id, None).expect("claim after expiry");
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
    let not_claimable = f.board.claim(&agent, &id, None).expect_err("attention is not a queue");
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
    assert!(f.board.get(&tools).expect("get").ticket.blocked_by.contains(&schema));
    let review = f.board.claim(&Actor::agent("codex"), &schema, None).expect("review");
    f.board
        .complete(&Actor::agent("codex"), &schema, review.fence, "ok")
        .expect("done");
    assert!(f.board.get(&tools).expect("get").ticket.blocked_by.is_empty());
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
    assert_eq!(codex_next.ticket.id, mine, "assigned work outranks a better priority");
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
    f.board.claim(&agent, &two, None).expect("second (default limit is 2)");
    let third = f.board.claim(&agent, &three, None).expect_err("over the limit");
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
    f.board.claim(&generous, &three, None).expect("0 = unlimited");
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
        .handoff(&claude, &id, lease.fence, "tests are red on Windows; see note 1", Some("codex"))
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
    let short = f.board.force_release(&jesse, &id, "x").expect_err("reason too short");
    assert_eq!(short.code, "invalid");
    let freed = f.board.force_release(&jesse, &id, "claude's session died").expect("forced");
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
    assert_eq!(cancelled.iter().map(|t| &t.id).collect::<Vec<_>>(), vec![&a]);
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
    workflow.transitions.retain(|t| t.from != "ready" && t.to != "ready");
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
    f.board.put_workflow(&jesse, &workflow).expect("now it fits");
    let lease = f.board.claim(&claude, &id, None).expect("backlog is the lane queue now");
    assert_eq!(lease.ticket.state, "in_progress");
    assert_eq!(f.board.workflow().expect("workflow").kind("in_progress"), Some(StateKind::Active));
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

    use crate::board::cli::{self, EXIT_CONFLICT, EXIT_NOT_FOUND, EXIT_OK, EXIT_UNAVAILABLE, EXIT_USAGE, Invocation};

    fn run(cwd: &Path, actor: Option<&str>, terminal: bool, args: &[&str]) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = cli::run(
            Invocation {
                args: args.iter().map(OsString::from).collect(),
                cwd: cwd.to_path_buf(),
                actor_env: actor.map(str::to_string),
                os_user: Some("jesse".into()),
                stdout_is_terminal: terminal,
            },
            &mut out,
            &mut err,
        );
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text.lines().next().unwrap_or("")).expect("one json line")
    }

    #[test]
    fn init_creates_the_board_and_appends_the_agents_block_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Repo\n").unwrap();
        let (code, out, _) = run(dir.path(), None, false, &["init", "--project", "demo", "--agents-md"]);
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
        assert_eq!(json(&out)["project"], "demo", "the project name is kept on re-init");
        let again = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(again.matches(cli::AGENTS_MD_START).count(), 1);
    }

    #[test]
    fn the_loop_works_from_a_subdirectory_with_a_cached_fence() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run(dir.path(), None, false, &["init", "--project", "demo"]).0, EXIT_OK);
        let sub = dir.path().join("crates").join("x");
        std::fs::create_dir_all(&sub).unwrap();
        let (code, out, _) = run(&sub, Some("claude"), false, &["new", "port it", "--label", "Hub"]);
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
        let (code, _, _) = run(&sub, Some("claude"), false, &["note", &id, "learned a thing", "--to", "codex"]);
        assert_eq!(code, EXIT_OK);
        let (code, out, _) = run(&sub, Some("claude"), false, &["done", &id, "shipped"]);
        assert_eq!(code, EXIT_OK, "{out}");
        assert_eq!(json(&out)["state"], "review");
        assert!(!dir.path().join(".devagotchi/claims/claude.json").exists(), "cache cleared on done");
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
    fn exit_codes_and_next_hints_by_failure_class() {
        let dir = tempfile::tempdir().unwrap();
        let (code, _, err) = run(dir.path(), None, false, &["prime"]);
        assert_eq!(code, EXIT_UNAVAILABLE, "no board here");
        assert!(err.is_empty(), "json mode puts the error on stdout");
        assert_eq!(run(dir.path(), None, false, &["init"]).0, EXIT_OK);
        let (code, out, _) = run(dir.path(), None, false, &["show", "nope"]);
        assert_eq!(code, EXIT_NOT_FOUND);
        assert!(json(&out)["error"]["next"].as_str().unwrap().contains("board ls"));
        let (code, _, err) = run(dir.path(), None, true, &["bogus"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.contains("next: board --help"));
        let (code, _, err) = run(dir.path(), None, true, &["new", "x", "--limit", "many"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.contains("--limit must be an integer"));
        let (code, out, _) = run(dir.path(), Some("claude"), false, &["new", "held"]);
        assert_eq!(code, EXIT_OK);
        let id = json(&out)["id"].as_str().unwrap().to_string();
        assert_eq!(run(dir.path(), Some("claude"), false, &["claim", &id]).0, EXIT_OK);
        let (code, out, _) = run(dir.path(), Some("claude"), false, &["beat", &id, "--fence", "99"]);
        assert_eq!(code, EXIT_CONFLICT);
        assert_eq!(json(&out)["error"]["code"], "stale_fence");
        // An agent may not force-release; the bare OS user (a person) may.
        let (code, out, _) = run(dir.path(), Some("codex"), false, &["force", &id, "stuck session"]);
        assert_eq!(code, 3);
        assert_eq!(json(&out)["error"]["code"], "humans_only");
        let (code, out, _) = run(dir.path(), None, false, &["force", &id, "stuck session"]);
        assert_eq!(code, EXIT_OK, "{out}");
        assert_eq!(json(&out)["holder"], serde_json::Value::Null);
    }

    #[test]
    fn prime_text_stays_short_and_names_the_protocol() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run(dir.path(), None, false, &["init", "--project", "demo"]).0, EXIT_OK);
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
        assert_eq!(run(dir.path(), None, false, &["init", "--project", "demo"]).0, EXIT_OK);
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
