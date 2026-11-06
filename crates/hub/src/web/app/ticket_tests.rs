//! The board and ticket pages on a hosted hub over HTTP: filters and search, and each action
//! (comment, assign, close, undo, hand to AI) changing the ticket as the person, in the
//! board's event chain and the audit; nothing of A's board reachable by B.

use serde_json::{Value, json};

use super::tests::{hosted, page, things_of};
use crate::tenant_tests::{Hosted, Person, refused};

async fn ticket(hub: &Hosted, alice: &mut Person, id: &str) -> Value {
    let got = hub.call(alice, "board_get", json!({ "project": "alpha", "id": id })).await;
    assert!(!refused(&got), "{got}");
    got["structuredContent"].clone()
}

/// Press one of the ticket page's buttons; where it sends the browser next.
async fn press(hub: &Hosted, person: &Person, id: &str, action: &str, form: &[(&str, &str)]) -> String {
    let answer = hub
        .form(&format!("/app/tickets/alpha/{id}/{action}"), Some(&person.cookie), form)
        .await;
    assert_eq!(answer.status(), 303, "{action}");
    answer.headers()["location"].to_str().unwrap().to_string()
}

fn version(view: &Value) -> String {
    view["ticket"]["version"].as_i64().unwrap().to_string()
}

#[tokio::test]
async fn a_person_works_a_ticket_from_the_phone_and_the_board_records_it() {
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    let bob = hub.first_connect("Claude").await;
    things_of(&hub, &mut alice).await;
    let me = hub.call(&mut alice, "whoami", json!({})).await["structuredContent"]["subject"]
        .as_str()
        .unwrap()
        .to_string();
    let made = hub
        .call(
            &mut alice,
            "board_create",
            json!({ "project": "alpha", "title": "Check the backups", "labels": ["ops"],
                    "description": "Nightly <b>backups</b> on desk." }),
        )
        .await;
    let id = made["structuredContent"]["id"].as_str().unwrap().to_string();

    // The board: every open ticket, chips for its states, assignees and labels, a search box.
    let (status, html) = page(&hub, "/app", &alice).await;
    assert_eq!(status, 200);
    assert!(html.contains("<a href=\"/app/boards/alpha\">"), "home leads to the board");
    let (status, html) = page(&hub, "/app/boards/alpha", &alice).await;
    assert_eq!(status, 200, "{html}");
    for seen in ["Check the backups", "A-PRIVATE-TICKET", "id=\"chips-state\"", "id=\"chips-label\"",
                 "id=\"chips-assignee\"", ">You</a>", "type=\"search\""] {
        assert!(html.contains(seen), "the board shows {seen}: {html}");
    }
    assert!(html.contains(&format!("href=\"/app/tickets/alpha/{id}\"")));
    let (_, labelled) = page(&hub, "/app/boards/alpha?label=ops", &alice).await;
    assert!(labelled.contains("Check the backups") && !labelled.contains("A-PRIVATE-TICKET"));
    assert!(labelled.contains("href=\"/app/boards/alpha\" aria-current=\"true\">ops</a>"), "tapping it again clears it");
    let (_, found) = page(&hub, "/app/boards/alpha?q=backups", &alice).await;
    assert!(found.contains("Check the backups") && !found.contains("A-PRIVATE-TICKET"));
    let (_, none) = page(&hub, "/app/boards/alpha?q=nothing-like-it", &alice).await;
    assert!(none.contains("No open tickets match."));

    // The ticket: its fields, description escaped, and the four actions.
    let (status, html) = page(&hub, &format!("/app/tickets/alpha/{id}"), &alice).await;
    assert_eq!(status, 200, "{html}");
    assert!(html.contains("<h1>Check the backups</h1>") && html.contains("Nightly &lt;b&gt;backups"));
    for action in ["comment", "assign", "close", "hand"] {
        assert!(html.contains(&format!("action=\"/app/tickets/alpha/{id}/{action}\"")), "{action}");
    }

    // Another site's form changes nothing.
    let cross = hub
        .http
        .post(format!("{}/app/tickets/alpha/{id}/comment", hub.url))
        .header("cookie", &alice.cookie)
        .header("sec-fetch-site", "cross-site")
        .form(&[("body", "x")])
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), 403);

    // Comment.
    let to = press(&hub, &alice, &id, "comment", &[("body", "Looked from the phone")]).await;
    assert_eq!(to, format!("/app/tickets/alpha/{id}?done=comment"));
    let view = ticket(&hub, &mut alice, &id).await;
    let note = &view["notes"][0];
    assert_eq!((note["body"].as_str(), note["author"].as_str()), (Some("Looked from the phone"), Some(me.as_str())));
    let (_, html) = page(&hub, &to, &alice).await;
    assert!(html.contains("Comment added.") && html.contains("Looked from the phone"));

    // Assign to me.
    let to = press(&hub, &alice, &id, "assign", &[("version", &version(&view))]).await;
    assert!(to.ends_with("?done=assign"), "{to}");
    let view = ticket(&hub, &mut alice, &id).await;
    assert_eq!(view["ticket"]["assignee"], me.as_str());

    // Close: at once, with Undo on the page for its 10 s; Undo reopens it where it was.
    let to = press(&hub, &alice, &id, "close", &[("version", &version(&view))]).await;
    assert!(to.ends_with("?done=close&from=ready"), "{to}");
    let view = ticket(&hub, &mut alice, &id).await;
    assert_eq!(view["ticket"]["state"], "cancelled", "the default workflow closes a ready ticket as cancelled");
    let (_, html) = page(&hub, &to, &alice).await;
    assert!(html.contains("id=\"undo\" data-until=") && html.contains("name=\"from\" value=\"ready\""), "{html}");
    let to = press(&hub, &alice, &id, "undo", &[("version", &version(&view)), ("from", "ready")]).await;
    assert!(to.ends_with("?done=undo"), "{to}");
    let view = ticket(&hub, &mut alice, &id).await;
    assert_eq!(view["ticket"]["state"], "ready");

    // Hand to AI: in the ready queue, unassigned.
    let to = press(&hub, &alice, &id, "hand", &[("version", &version(&view))]).await;
    assert!(to.ends_with("?done=hand"), "{to}");
    let view = ticket(&hub, &mut alice, &id).await;
    assert_eq!((view["ticket"]["state"].as_str(), &view["ticket"]["assignee"]), (Some("ready"), &Value::Null));

    // A stale version is refused and says why.
    let to = press(&hub, &alice, &id, "assign", &[("version", "1")]).await;
    assert!(to.contains("?done=no&why="), "{to}");

    // Every change is in the board's event chain as the person, and in the audit.
    let events = hub.call(&mut alice, "board_events", json!({ "project": "alpha" })).await;
    // An array of events (as text: a structured result is an object).
    let events: Value =
        serde_json::from_str(events["content"][0]["text"].as_str().unwrap()).unwrap();
    let mine: Vec<&Value> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["ticket"] == id.as_str() && e["actor"] == me.as_str())
        .collect();
    let updates = mine.iter().filter(|e| e["action"] == "ticket.updated").count();
    assert_eq!(updates, 4, "assign, close, undo, hand: {mine:?}");
    assert!(mine.iter().any(|e| e["action"].as_str().unwrap_or_default().starts_with("note")), "{mine:?}");
    let audit = rusqlite::Connection::open(hub.dir.path().join("audit.sqlite3")).unwrap();
    let audited: Vec<(String, String)> = audit
        .prepare("SELECT tool, outcome FROM audit WHERE broker = 'alpha' AND operation_id = ?1 AND subject = ?2 ORDER BY seq")
        .unwrap()
        .query_map([&id, &me], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .filter(|(tool, _)| tool == "board_note" || tool == "board_update")
        .collect();
    let tools: Vec<&str> = audited.iter().map(|(tool, _)| tool.as_str()).collect();
    assert!(tools.ends_with(&["board_note", "board_update", "board_update", "board_update", "board_update", "board_update"]), "{audited:?}");
    assert_eq!(audited.last().unwrap().1, "version_conflict");

    // B: no board, no ticket, no action.
    for path in ["/app/boards/alpha".to_string(), format!("/app/tickets/alpha/{id}")] {
        let (status, html) = page(&hub, &path, &bob).await;
        assert_eq!(status, 404, "{path}");
        assert!(!html.contains("Check the backups"));
    }
    let theirs = hub
        .form(&format!("/app/tickets/alpha/{id}/comment"), Some(&bob.cookie), &[("body", "B was here")])
        .await;
    assert_eq!(theirs.status(), 404);
    assert_eq!(ticket(&hub, &mut alice, &id).await["notes"].as_array().unwrap().len(), 1);
    let signed_out = hub.get(&format!("/app/tickets/alpha/{id}"), None).await;
    assert_eq!(
        signed_out.headers()["location"].to_str().unwrap(),
        format!("/auth/login?return_to=%2Fapp%2Ftickets%2Falpha%2F{}", crate::web::encode(&id))
    );
}

#[tokio::test]
async fn undo_lasts_ten_seconds() {
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    things_of(&hub, &mut alice).await;
    let made = hub
        .call(&mut alice, "board_create", json!({ "project": "alpha", "title": "Closed for good" }))
        .await;
    let id = made["structuredContent"]["id"].as_str().unwrap().to_string();
    let view = ticket(&hub, &mut alice, &id).await;
    press(&hub, &alice, &id, "close", &[("version", &version(&view))]).await;
    tokio::time::sleep(std::time::Duration::from_millis(super::ticket::UNDO_MS as u64 + 300)).await;
    let view = ticket(&hub, &mut alice, &id).await;
    let to = press(&hub, &alice, &id, "undo", &[("version", &version(&view)), ("from", "ready")]).await;
    assert!(to.ends_with("?done=late"), "{to}");
    assert_eq!(ticket(&hub, &mut alice, &id).await["ticket"]["state"], "cancelled");
    let (_, html) = page(&hub, &format!("/app/tickets/alpha/{id}?done=close&from=ready"), &alice).await;
    assert!(!html.contains("id=\"undo\""), "no Undo once its time is up");
}

#[tokio::test]
async fn the_board_filters_by_anchor_chips_and_the_ticket_shows_its_anchors() {
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    things_of(&hub, &mut alice).await;
    let made = hub
        .call(
            &mut alice,
            "board_create",
            json!({ "project": "alpha", "title": "dabo is down", "machine": "dabo",
                    "environment": "prod", "repo": "https://github.com/x/y" }),
        )
        .await;
    let id = made["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, html) = page(&hub, "/app/boards/alpha", &alice).await;
    assert_eq!(status, 200, "{html}");
    for seen in [
        "id=\"chips-machine\"",
        "id=\"chips-environment\"",
        "id=\"chips-repo\"",
        "id=\"chips-agent\"",
        ">dabo</a>",
        ">prod</a>",
    ] {
        assert!(html.contains(seen), "the board shows {seen}: {html}");
    }
    let (_, on_dabo) = page(&hub, "/app/boards/alpha?machine=dabo", &alice).await;
    assert!(
        on_dabo.contains("dabo is down") && !on_dabo.contains("A-PRIVATE-TICKET"),
        "{on_dabo}"
    );
    assert!(
        on_dabo.contains("href=\"/app/boards/alpha\" aria-current=\"true\">dabo</a>"),
        "tapping it again clears it"
    );
    let (_, in_dev) = page(&hub, "/app/boards/alpha?environment=dev", &alice).await;
    assert!(!in_dev.contains("dabo is down"), "{in_dev}");
    let (_, searched) = page(&hub, "/app/boards/alpha?machine=dabo&q=down", &alice).await;
    assert!(
        searched.contains("name=\"machine\" value=\"dabo\""),
        "the search keeps the chip"
    );

    let (status, html) = page(&hub, &format!("/app/tickets/alpha/{id}"), &alice).await;
    assert_eq!(status, 200, "{html}");
    assert!(
        html.contains(
            "<dt>Where</dt><dd id=\"anchors\">dabo · prod · github.com/x/y · ai (session "
        ),
        "{html}"
    );
}

/// A's board `alpha` as an earlier build left it, before this hub opens it: its workflow is
/// `workflow`, stored without an event.
async fn earlier_board(hub: &Hosted, alice: &mut Person, workflow: &crate::board::Workflow) {
    let me = hub.call(alice, "whoami", json!({})).await["structuredContent"]["subject"]
        .as_str()
        .unwrap()
        .to_string();
    let dir = hub.dir.path().join("account-boards").join(&me);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("alpha.sqlite3");
    drop(crate::board::Board::open(&path, "alpha").unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE settings SET value = ?1 WHERE key = 'workflow'",
            [serde_json::to_string(workflow).unwrap()],
        )
        .unwrap();
}

/// Close a new ticket on `alpha` from the phone and press Undo; where Undo sends the browser.
async fn close_and_undo(hub: &Hosted, alice: &mut Person) -> (String, String) {
    let made = hub
        .call(alice, "board_create", json!({ "project": "alpha", "title": "Reopen me" }))
        .await;
    assert!(!refused(&made), "{made}");
    let id = made["structuredContent"]["id"].as_str().unwrap().to_string();
    let view = ticket(hub, alice, &id).await;
    let to = press(hub, alice, &id, "close", &[("version", &version(&view))]).await;
    assert!(to.ends_with("?done=close&from=ready"), "{to}");
    let view = ticket(hub, alice, &id).await;
    assert_eq!(view["ticket"]["state"], "cancelled");
    let to = press(hub, alice, &id, "undo", &[("version", &version(&view)), ("from", "ready")]).await;
    (id, to)
}

#[tokio::test]
async fn undo_works_on_a_board_made_before_closed_tickets_reopened() {
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    earlier_board(&hub, &mut alice, &crate::board::Workflow::previous_default()).await;
    let (id, to) = close_and_undo(&hub, &mut alice).await;
    assert!(to.ends_with("?done=undo"), "{to}");
    assert_eq!(ticket(&hub, &mut alice, &id).await["ticket"]["state"], "ready");
    let events = hub.call(&mut alice, "board_events", json!({ "project": "alpha" })).await;
    let events: Value =
        serde_json::from_str(events["content"][0]["text"].as_str().unwrap()).unwrap();
    let upgrades = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "board.workflow_upgraded")
        .count();
    assert_eq!(upgrades, 1, "{events}");
    let verified = hub.call(&mut alice, "board_verify", json!({ "project": "alpha" })).await;
    assert_eq!(verified["structuredContent"]["ok"], true, "{verified}");
}

#[tokio::test]
async fn undo_on_a_customised_workflow_says_which_transition_to_add() {
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    let mut ours = crate::board::Workflow::previous_default();
    ours.id = "ours".into();
    earlier_board(&hub, &mut alice, &ours).await;
    let (id, to) = close_and_undo(&hub, &mut alice).await;
    assert!(to.contains("?done=no&why="), "{to}");
    let why = crate::tenant_tests::query_param(&to, "why").unwrap();
    assert!(
        why.contains("workflow (ours) has no transition cancelled -> ready")
            && why.contains("add {\"from\": \"cancelled\", \"to\": \"ready\"}"),
        "{why}"
    );
    assert_eq!(ticket(&hub, &mut alice, &id).await["ticket"]["state"], "cancelled");
}
