//! `board_settings` on a hosted hub over MCP (`tenant_tests.rs`'s world): the account sets
//! its board's repo and environments and reads them back, new tickets carry the repo, bad
//! input is refused with what to fix in `next`, and another account cannot reach them.

use serde_json::{Value, json};

use crate::tenant_tests::{Hosted, Person, hosted_with, refused};

async fn settings(hub: &Hosted, person: &mut Person, arguments: Value) -> Value {
    hub.call(person, "board_settings", arguments).await
}

#[tokio::test]
async fn an_account_names_its_boards_repo_and_environments_and_nobody_else_can() {
    let hub = hosted_with(None, |_, _| json!({})).await;
    let mut alice = hub.first_connect("Claude").await;
    let mut bob = hub.first_connect("Claude").await;
    let bound = hub
        .call(&mut alice, "board_bind", json!({ "project": "alpha" }))
        .await;
    assert!(!refused(&bound), "{bound}");

    // Set, then read back with nothing given.
    let set = settings(
        &hub,
        &mut alice,
        json!({ "project": "alpha", "repo": "https://github.com/x/alpha",
                "environments": ["dev", "prod"] }),
    )
    .await;
    assert!(!refused(&set), "{set}");
    let set = set["structuredContent"].clone();
    assert_eq!(set["repo"], "https://github.com/x/alpha");
    assert_eq!(set["environments"], json!(["dev", "prod"]));
    let read = settings(&hub, &mut alice, json!({ "project": "alpha" })).await;
    assert_eq!(read["structuredContent"], set, "read back as set");

    // A ticket made afterwards carries the repo (P2c) and picks from the environments.
    let made = hub
        .call(
            &mut alice,
            "board_create",
            json!({ "project": "alpha", "title": "after", "environment": "prod" }),
        )
        .await;
    assert!(!refused(&made), "{made}");
    assert_eq!(made["structuredContent"]["repo"], "https://github.com/x/alpha");
    let staging = hub
        .call(
            &mut alice,
            "board_create",
            json!({ "project": "alpha", "title": "nowhere", "environment": "staging" }),
        )
        .await;
    assert!(refused(&staging), "{staging}");

    // Bad input: refused, with what is wrong in `next`.
    for (arguments, says) in [
        (json!({ "project": "alpha", "repo": "http://github.com/x/alpha" }), "is not an https URL"),
        (json!({ "project": "alpha", "environments": [] }), "1 to 20 names"),
        (json!({ "project": "alpha", "environments": ["Prod"] }), "not a lowercase name"),
    ] {
        let answer = settings(&hub, &mut alice, arguments.clone()).await;
        assert!(refused(&answer), "{arguments}: {answer}");
        let text = answer.to_string();
        assert!(text.contains("invalid_settings"), "{answer}");
        let error: Value = answer
            .get("structuredContent")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or_else(|| {
                serde_json::from_str(answer["content"][0]["text"].as_str().unwrap()).unwrap()
            });
        let next = error["next"].as_str().unwrap();
        assert!(next.contains(says) && next.contains("board_settings again"), "{next}");
    }

    // B reaches neither A's settings nor A's board.
    for arguments in [
        json!({ "project": "alpha" }),
        json!({ "project": "alpha", "repo": "https://github.com/b/stolen" }),
    ] {
        let answer = settings(&hub, &mut bob, arguments).await;
        assert!(refused(&answer), "{answer}");
        assert!(!answer.to_string().contains("github.com/x/alpha"), "{answer}");
    }
    let read = settings(&hub, &mut alice, json!({ "project": "alpha" })).await;
    assert_eq!(read["structuredContent"]["repo"], "https://github.com/x/alpha");

    // Every change is on the chain, which still verifies.
    let events = hub
        .call(&mut alice, "board_events", json!({ "project": "alpha" }))
        .await;
    let events: Value =
        serde_json::from_str(events["content"][0]["text"].as_str().unwrap()).unwrap();
    let changes = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "board.settings_changed")
        .count();
    assert_eq!(changes, 1, "one change, no reads, no refusals: {events}");
    let verified = hub
        .call(&mut alice, "board_verify", json!({ "project": "alpha" }))
        .await;
    assert_eq!(verified["structuredContent"]["ok"], true, "{verified}");
}
