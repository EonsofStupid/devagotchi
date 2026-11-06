//! Approvals on an account's own board, on a hosted hub over HTTP (`tenant_tests.rs`'s
//! world): the person signed in decides them on `/approvals` and sees them under Needs you;
//! another account neither sees nor decides them; the account's AI, with its bearer token,
//! decides nothing.

use serde_json::{Value, json};

use super::tests::{hosted, page};
use crate::tenant_tests::{Hosted, Person, refused};

/// A's board `alpha` with one approval A's AI asked for; the approval as the tool gave it.
async fn asked(hub: &Hosted, person: &mut Person) -> Value {
    let bound = hub
        .call(person, "board_bind", json!({ "project": "alpha" }))
        .await;
    assert!(!refused(&bound), "{bound}");
    let asked = hub
        .call(
            person,
            "approval_request",
            json!({ "project": "alpha", "title": "A-PRIVATE-APPROVAL", "what": "restart the site" }),
        )
        .await;
    assert!(!refused(&asked), "{asked}");
    asked["structuredContent"].clone()
}

async fn decide(hub: &Hosted, cookie: Option<&str>, asked: &Value, verdict: &str) -> (u16, String) {
    let version = asked["version"].to_string();
    let answer = hub
        .form(
            "/approvals/decide",
            cookie,
            &[
                ("project", "alpha"),
                ("id", asked["id"].as_str().unwrap()),
                ("version", &version),
                ("verdict", verdict),
            ],
        )
        .await;
    let location = answer
        .headers()
        .get("location")
        .map(|l| l.to_str().unwrap().to_string())
        .unwrap_or_default();
    (answer.status().as_u16(), location)
}

async fn status(hub: &Hosted, person: &mut Person, asked: &Value) -> Value {
    let waited = hub
        .call(
            person,
            "approval_wait",
            json!({ "project": "alpha", "id": asked["id"], "timeout_ms": 0 }),
        )
        .await;
    assert!(!refused(&waited), "{waited}");
    waited["structuredContent"]["status"].clone()
}

#[tokio::test]
async fn a_person_decides_approvals_on_their_own_board_and_nobody_else_does() {
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    let bob = hub.first_connect("Claude").await;
    let asked = asked(&hub, &mut alice).await;
    let id = asked["id"].as_str().unwrap().to_string();

    // A sees it on the approvals page and under Needs you.
    let (code, list) = page(&hub, "/approvals", &alice).await;
    assert_eq!(code, 200, "{list}");
    assert!(list.contains("A-PRIVATE-APPROVAL"), "{list}");
    assert!(list.contains(&format!("value=\"{id}\"")), "{list}");
    let (_, home) = page(&hub, "/app", &alice).await;
    assert!(
        home.contains(&format!("href=\"/approvals#alpha-{id}\"")) && home.contains("A-PRIVATE-APPROVAL"),
        "Needs you lists A's approval: {home}"
    );

    // B neither sees nor decides it.
    let (code, list) = page(&hub, "/approvals", &bob).await;
    assert_eq!(code, 200, "B has an approvals page of their own: {list}");
    assert!(!list.contains("A-PRIVATE-APPROVAL"), "{list}");
    let (_, home) = page(&hub, "/app", &bob).await;
    assert!(!home.contains("A-PRIVATE-APPROVAL"), "{home}");
    let (code, _) = decide(&hub, Some(&bob.cookie), &asked, "approved").await;
    assert_eq!(code, 403, "B cannot decide A's approval");
    assert_eq!(status(&hub, &mut alice, &asked).await, "pending");

    // A's AI, with its bearer token and no browser session, is sent to sign in.
    let answer = hub
        .http
        .post(format!("{}/approvals/decide", hub.url))
        .header("origin", &hub.url)
        .bearer_auth(&alice.token)
        .form(&[
            ("project", "alpha"),
            ("id", id.as_str()),
            ("version", &asked["version"].to_string()),
            ("verdict", "approved"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(answer.status(), 303);
    assert!(
        answer.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with("/auth/login"),
        "a bearer token is not a person"
    );
    assert_eq!(status(&hub, &mut alice, &asked).await, "pending");
    // And no board tool decides one.
    let tools = hub.mcp(&mut alice, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains("decide") && *n != "approval_approve"),
        "{names:?}"
    );

    // A decides it from the phone page; the agent sees the decision.
    let (code, location) = decide(&hub, Some(&alice.cookie), &asked, "approved").await;
    assert_eq!(code, 303);
    assert_eq!(location, format!("/approvals?done={id}"));
    assert_eq!(status(&hub, &mut alice, &asked).await, "approved");
    let (_, list) = page(&hub, "/approvals", &alice).await;
    assert!(list.contains("Nothing is waiting for you."), "{list}");
    let (_, home) = page(&hub, "/app", &alice).await;
    assert!(!home.contains("A-PRIVATE-APPROVAL"), "decided, it leaves Needs you");
}
