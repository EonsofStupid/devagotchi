//! The phone app's pages on a hosted hub over HTTP (`tenant_tests.rs`'s world): each page
//! renders signed in, sends the signed out to sign in, and shows only the account's own
//! machines and boards.

use serde_json::{Value, json};

use crate::tenant_tests::{Hosted, Person, hosted_with, refused};

async fn hosted() -> Hosted {
    hosted_with(None, |_, _| json!({})).await
}

async fn page(hub: &Hosted, path: &str, person: &Person) -> (u16, String) {
    Hosted::text(hub.get(path, Some(&person.cookie)).await).await
}

/// A's things: a machine that joined, a board with its getting-started ticket and one more
/// assigned to A (2 open). The machine's name as the hub stores it.
async fn things_of(hub: &Hosted, person: &mut Person) -> String {
    let added = hub
        .call(person, "machine_add", json!({ "name": "desk" }))
        .await;
    assert!(!refused(&added), "{added}");
    // It joins with its code, as `tenant_tests.rs` enrolls one.
    let added = &added["structuredContent"];
    let stored = added["manual"]
        .as_str()
        .unwrap()
        .split("--device ")
        .nth(1)
        .unwrap();
    let stored = stored.split(' ').next().unwrap().to_string();
    let enrolled = hub
        .http
        .post(format!("{}/enroll", hub.url))
        .json(&json!({ "name": stored, "code": added["code"], "public_key": "desk-key" }))
        .send()
        .await
        .unwrap();
    assert_eq!(enrolled.status(), 201);
    let bound = hub
        .call(person, "board_bind", json!({ "project": "alpha" }))
        .await;
    assert!(!refused(&bound), "{bound}");
    let me: Value = hub.call(person, "whoami", json!({})).await["structuredContent"].clone();
    let made = hub
        .call(
            person,
            "board_create",
            json!({ "project": "alpha", "title": "A-PRIVATE-TICKET", "assignee": me["subject"] }),
        )
        .await;
    assert!(!refused(&made), "{made}");
    stored
}

#[tokio::test]
async fn the_app_shows_only_the_accounts_own_machines_and_boards_in_one_frame() {
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    let bob = hub.first_connect("Claude").await;
    things_of(&hub, &mut alice).await;

    let (status, home) = page(&hub, "/app", &alice).await;
    assert_eq!(status, 200, "{home}");
    for seen in [
        "id=\"needs-you\"",
        "id=\"machines\"",
        "id=\"boards\"",
        "desk",
        "alpha",
        "2 open",
    ] {
        assert!(home.contains(seen), "home shows {seen}: {home}");
    }
    assert!(
        home.contains("A-PRIVATE-TICKET"),
        "Needs you lists the ticket assigned to A"
    );
    assert!(home.contains("<body class=\"app\">"), "the app's frame");
    assert!(home.contains("<nav class=\"tabs\"><a href=\"/app\" aria-current=\"page\">Home</a>"));
    for tab in ["/app/machines", "/app/boards", "/approvals"] {
        assert!(
            home.contains(&format!("<a href=\"{tab}\">")),
            "the tab bar leads to {tab}"
        );
    }
    assert_eq!(
        home.matches("<script").count(),
        1,
        "one script: the app's own file"
    );
    assert!(home.contains("<script src=\"/assets/app.js\" defer></script>"));
    assert!(
        home.contains("id=\"install\" hidden"),
        "the install banner waits for app.js"
    );

    for (path, tab) in [("/app/machines", "Machines"), ("/app/boards", "Boards")] {
        let (status, html) = page(&hub, path, &alice).await;
        assert_eq!(status, 200, "{path}: {html}");
        assert!(
            html.contains(&format!("aria-current=\"page\">{tab}</a>")),
            "{path}"
        );
    }
    let (_, machines) = page(&hub, "/app/machines", &alice).await;
    assert!(machines.contains("desk"));
    let (_, boards) = page(&hub, "/app/boards", &alice).await;
    assert!(boards.contains("alpha") && boards.contains("2 open"));

    // B sees none of A's: not on any page of the app.
    for path in ["/app", "/app/machines", "/app/boards"] {
        let (status, html) = page(&hub, path, &bob).await;
        assert_eq!(status, 200, "{path}");
        for theirs in ["desk", "alpha", "A-PRIVATE-TICKET"] {
            assert!(!html.contains(theirs), "B's {path} shows A's {theirs}");
        }
    }
    let (_, home) = page(&hub, "/app", &bob).await;
    assert!(home.contains("No machines yet.") && home.contains("No boards yet."));
    assert!(home.contains("Nothing needs you."));
}

#[tokio::test]
async fn signed_out_the_app_sends_you_to_sign_in_and_back() {
    let hub = hosted().await;
    for (path, back) in [
        ("/app", "%2Fapp"),
        ("/app/machines", "%2Fapp%2Fmachines"),
        ("/app/boards", "%2Fapp%2Fboards"),
    ] {
        let answer = hub.get(path, None).await;
        assert_eq!(answer.status(), 303, "{path}");
        assert_eq!(
            answer.headers()["location"],
            format!("/auth/login?return_to={back}").as_str(),
            "{path}"
        );
    }
}

#[tokio::test]
async fn the_app_installs_at_app_and_opens_offline() {
    let hub = hosted().await;
    let manifest: Value = hub
        .get("/manifest.webmanifest", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(manifest["start_url"], "/app");
    assert_eq!(manifest["id"], "/app");
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["orientation"], "portrait");
    let purposes: Vec<&str> = manifest["icons"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|icon| icon["purpose"].as_str())
        .collect();
    assert!(
        purposes.contains(&"any") && purposes.contains(&"maskable"),
        "{manifest}"
    );

    // The page the service worker keeps for offline needs no sign-in, and it keeps it.
    let (status, offline) = Hosted::text(hub.get("/app/offline", None).await).await;
    assert_eq!(status, 200);
    assert!(offline.contains("<h1>You&#39;re offline</h1>"), "{offline}");
    assert!(
        offline.contains("<nav class=\"tabs\">"),
        "the offline page is the app's frame"
    );
    let (_, worker) = Hosted::text(hub.get("/sw.js", None).await).await;
    for kept in [
        "\"/app/offline\"",
        "\"/assets/hub.css\"",
        "\"/assets/app.js\"",
    ] {
        assert!(worker.contains(kept), "the service worker keeps {kept}");
    }
    let script = hub.get("/assets/app.js", None).await;
    assert_eq!(script.status(), 200);
    assert!(
        script.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
}

/// An operation as the broker answers it, for the hub's routed-jobs ring.
fn view(id: &str, state: &str, exit_code: Option<i32>) -> devagotchi_contracts::OperationView {
    serde_json::from_value(json!({
        "operation_id": id, "executable": "sh", "args": [], "cwd": "/", "deadline_ms": 1000,
        "state": state, "created_at_ms": 1_700_000_000_000u64, "updated_at_ms": 1_700_000_000_000u64,
        "exit_code": exit_code, "detail": null, "ownership_scope": "x",
        "cancellation_requested": false, "output_complete": false
    }))
    .unwrap()
}

#[tokio::test]
async fn a_machines_page_shows_its_state_and_jobs_and_updates_from_here_only() {
    use devagotchi_contracts::{ResponseBody, StartOutput};
    let hub = hosted().await;
    let mut alice = hub.first_connect("Claude").await;
    let bob = hub.first_connect("Claude").await;
    let device = things_of(&hub, &mut alice).await;

    let (status, html) = page(&hub, "/app/machines", &alice).await;
    assert_eq!(status, 200);
    assert!(html.contains("<a href=\"/app/machines/desk\">"), "the list leads to the page");
    let (status, html) = page(&hub, "/app/machines/desk", &alice).await;
    assert_eq!(status, 200, "{html}");
    assert!(html.contains("<h1>desk</h1>"));
    for seen in ["Status", "Offline", "Build", "Last seen", "Not since the hub started", "Update now"] {
        assert!(html.contains(seen), "the page shows {seen}: {html}");
    }
    assert!(html.contains("action=\"/app/machines/desk/update\""));
    assert!(html.contains("No jobs through the hub yet."));
    assert!(html.contains("aria-current=\"page\">Machines</a>"));

    // The ring: the last 20 operations routed to it, newest first, each moved by later answers.
    let links = &hub.hub.links;
    for n in 0..25 {
        let start = ResponseBody::Start(StartOutput {
            operation: view(&format!("op-{n}"), "running", None),
            replayed: false,
        });
        links.jobs_saw(&device, Some(format!("echo job-{n} {}", "x".repeat(100)).chars().take(80).collect()), &start);
    }
    links.jobs_saw(&device, None, &ResponseBody::Operation(view("op-24", "succeeded", Some(0))));
    links.jobs_saw(&device, None, &ResponseBody::Operation(view("op-0", "failed", Some(1))));
    let jobs = links.recent_jobs(&device);
    assert_eq!(jobs.len(), crate::link::RECENT_JOBS);
    assert_eq!(jobs[0].operation_id, "op-24");
    assert_eq!((jobs[0].state.as_str(), jobs[0].exit_code), ("succeeded", Some(0)));
    assert!(jobs.iter().all(|job| job.operation_id != "op-0"), "the oldest fell out");
    assert!(jobs.iter().all(|job| job.command.chars().count() <= 80));
    let (_, html) = page(&hub, "/app/machines/desk", &alice).await;
    assert!(html.contains("echo job-24") && html.contains("succeeded"), "{html}");
    assert!(!html.contains("echo job-4 "), "only the last 20");

    // Update now: another site's form changes nothing; from here, it is the machine_update
    // call (audited), and the page says what it came to.
    let cross = hub
        .http
        .post(format!("{}/app/machines/desk/update", hub.url))
        .header("cookie", &alice.cookie)
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), 403);
    let pressed = hub.form("/app/machines/desk/update", Some(&alice.cookie), &[]).await;
    assert_eq!(pressed.status(), 303);
    let to = pressed.headers()["location"].to_str().unwrap().to_string();
    assert!(to.starts_with("/app/machines/desk?state=failed&detail="), "{to}");
    let (_, html) = page(&hub, &to, &alice).await;
    assert!(html.contains("id=\"update-result\"") && html.contains("Update: failed"), "{html}");
    let audit = rusqlite::Connection::open(hub.dir.path().join("audit.sqlite3")).unwrap();
    let asked: i64 = audit
        .query_row(
            "SELECT count(*) FROM audit WHERE tool = 'machine.update' AND broker = ?1",
            [&device],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(asked, 1, "the update is in the audit");

    // Not B's: no page, no update, and the name says nothing about A.
    let (status, html) = page(&hub, "/app/machines/desk", &bob).await;
    assert_eq!(status, 404);
    assert!(!html.contains("echo job"));
    let theirs = hub.form("/app/machines/desk/update", Some(&bob.cookie), &[]).await;
    let to = theirs.headers()["location"].to_str().unwrap().to_string();
    assert!(to.contains("state=failed") && to.contains("no%20broker%20named"), "{to}");
    let signed_out = hub.get("/app/machines/desk", None).await;
    assert_eq!(
        signed_out.headers()["location"],
        "/auth/login?return_to=%2Fapp%2Fmachines%2Fdesk"
    );
}
