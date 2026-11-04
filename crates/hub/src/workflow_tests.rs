//! Workflows end to end over HTTP, the whole router as it serves (increment 68,
//! `workflows.rs`, `web/workflow.rs`): an AI lists and starts a workflow, its person opens
//! the link in a browser that never signed in, and each kind of step is done and ticked as
//! the hub sees it. Opening the link spends and reveals nothing; Get my command issues the
//! line; keeping the account is offered after the first success, to the browser that did
//! it; a link expires, is cancelled, is replaced and grants nothing once done; plans and
//! early access are said plainly; a wait returns the moment something moves. On SQLite,
//! and on Postgres when the test server is set (CI).

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use axum::http::header;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    accounts::identities::Proven,
    devices::now_secs,
    tenant_tests::{Hosted, Person, hosted_with, set_cookie},
};

/// A browser that never signed in: only the cookies a workflow's page gave it.
const NOBODY: Option<&str> = None;

/// A build to hand out, as `/dist` serves one: both installers and an archive for each OS.
fn dist(dir: &Path) -> PathBuf {
    let dist = dir.join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    let mut sums = String::new();
    for name in [
        "install-devagotchi.sh",
        "install-devagotchi.ps1",
        "devagotchi-x86_64-unknown-linux-gnu.tar.xz",
        "devagotchi-x86_64-pc-windows-msvc.zip",
    ] {
        std::fs::write(dist.join(name), name).unwrap();
        sums.push_str(&format!(
            "{}  {name}\n",
            hex::encode(Sha256::digest(name.as_bytes()))
        ));
    }
    std::fs::write(dist.join("SHA256SUMS"), sums).unwrap();
    std::fs::write(dist.join("VERSION"), "test-1\n").unwrap();
    dist
}

/// A hosted hub that hands out a build, with `extra` config (plans, a link's life).
async fn hub_on(store: Option<&str>, extra: Value) -> Hosted {
    hosted_with(store, move |dir, _| {
        let mut value = json!({ "downloads": dist(dir).to_string_lossy() });
        for (key, more) in extra.as_object().unwrap() {
            value[key] = more.clone();
        }
        value
    })
    .await
}

async fn stores() -> Vec<Option<String>> {
    let mut out = vec![None];
    if let Some(url) = store::testing::empty_postgres().await {
        out.push(Some(url));
    }
    out
}

/// A site directory in `dir`: the built-in site.json with `edit` applied.
fn site_dir(dir: &Path, edit: impl FnOnce(&mut Value)) -> PathBuf {
    let mut value: Value =
        serde_json::from_str(include_str!("../site/default/site.json")).unwrap();
    edit(&mut value);
    let site = dir.join("site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("site.json"), value.to_string()).unwrap();
    site
}

async fn structured(hub: &Hosted, person: &mut Person, tool: &str, arguments: Value) -> Value {
    let answer = hub.call(person, tool, arguments).await;
    assert_ne!(answer["isError"], true, "{tool}: {answer}");
    answer["structuredContent"].clone()
}

async fn who(hub: &Hosted, person: &mut Person) -> (String, String) {
    let who = structured(hub, person, "whoami", json!({})).await;
    (
        who["subject"].as_str().unwrap().to_string(),
        who["tenant"].as_str().unwrap().to_string(),
    )
}

/// The link's path on the hub.
fn path_of(link: &str, hub: &Hosted) -> String {
    let path = link
        .strip_prefix(&hub.url)
        .expect("a link on this hub")
        .to_string();
    assert!(path.starts_with("/go/dgw_"), "{path}");
    path
}

async fn page(hub: &Hosted, path: &str, cookie: Option<&str>) -> (u16, String) {
    Hosted::text(hub.get(path, cookie).await).await
}

async fn events(hub: &Hosted, tenant: &str) -> Vec<String> {
    let mut events: Vec<String> = hub
        .accounts
        .events(tenant, 200)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.event)
        .collect();
    events.reverse();
    events
}

fn step(status: &Value, index: usize) -> &Value {
    &status["steps"][index]
}

/// Secure `subject`'s account with a linked sign-in.
async fn secure(hub: &Hosted, subject: &str, n: u32) {
    hub.accounts
        .link_identity(
            subject,
            &Proven {
                provider: "github".into(),
                subject: format!("{n}"),
                verified_email: None,
                name: None,
            },
        )
        .await
        .unwrap();
}

/// Text as a page writes it.
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Get my command, pressed in a browser holding `cookies` (none: a fresh one): the answer,
/// and the cookies it holds after (the line's and its own).
async fn press(hub: &Hosted, path: &str, cookies: Option<&str>, form: &[(&str, &str)]) -> (u16, String) {
    let pressed = hub.form(&format!("{path}/command"), cookies, form).await;
    let status = pressed.status().as_u16();
    let mut held: Vec<String> = cookies.map(|c| c.split("; ").map(str::to_string).collect()).unwrap_or_default();
    for name in ["dg_wfc", "dg_wfb"] {
        if let Some(cookie) = set_cookie(&pressed, name) {
            held.retain(|c| !c.starts_with(&format!("{name}=")));
            held.push(cookie);
        }
    }
    (status, held.join("; "))
}

/// The code in a browser's line cookie.
fn code_in(cookies: &str) -> String {
    let line = cookies
        .split("; ")
        .find(|c| c.starts_with("dg_wfc="))
        .expect("the line's cookie");
    line.rsplit('.').next().unwrap().to_string()
}

/// A phone's subscription as push.js sends it.
fn subscription(n: u8) -> Value {
    use base64::Engine;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    json!({
        "endpoint": format!("https://push.example.net/send/{n}"),
        "keys": { "p256dh": b64(&[4u8; 65]), "auth": b64(&[n; 16]) }
    })
}

/// The phone posts its subscription to the link, from a browser holding `cookies`.
async fn turn_on_phone(hub: &Hosted, path: &str, cookies: Option<&str>, n: u8) -> reqwest::Response {
    let mut request = hub
        .http
        .post(format!("{}{path}/phone", hub.url))
        .header(header::ORIGIN, &hub.url)
        .json(&subscription(n));
    if let Some(cookies) = cookies {
        request = request.header(header::COOKIE, cookies);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn the_list_offers_each_workflow_and_says_plainly_what_cannot_be_done_here() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({})).await;
        let mut person = hub.first_connect("Claude").await;
        let listed = structured(&hub, &mut person, "workflow_list", json!({})).await;
        let all = listed["workflows"].as_array().unwrap().clone();
        let ids: Vec<&str> = all.iter().map(|w| w["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            [
                "get-started", "connect-linux", "connect-windows", "connect-mac", "phone-approvals",
                "connect-claude", "connect-chatgpt", "connect-grok", "connect-cursor"
            ]
        );
        let offer = |id: &str| all.iter().find(|w| w["id"] == id).unwrap().clone();
        for id in ["get-started", "connect-linux", "connect-windows", "phone-approvals", "connect-claude",
                   "connect-chatgpt", "connect-grok", "connect-cursor"] {
            assert_eq!(offer(id)["available"], true, "{id}");
        }
        assert_eq!(offer("connect-grok")["steps"][0]["client"], "grok");
        let kinds: Vec<String> = offer("get-started")["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, ["machine", "phone", "secure_account", "done"]);
        let mac = offer("connect-mac");
        assert_eq!(mac["available"], false);
        assert!(
            mac["why"].as_str().unwrap().contains("A Mac build of Devagotchi is not available yet"),
            "{mac}"
        );
        assert_eq!(mac["steps"], json!([]));
        assert_eq!(offer("connect-linux")["steps"][0]["os"], "linux");
        // A Mac is never started: there is nothing to install on one.
        let answer = hub.call(&mut person, "workflow_start", json!({ "workflow": "connect-mac" })).await;
        assert_eq!(answer["isError"], true, "{answer}");
        assert!(answer.to_string().contains("not available yet"), "{answer}");
        for arguments in [
            json!({ "workflow": "connect-nothing" }),
            json!({ "workflow": "connect-linux", "machine_name": "Not A Name!" }),
        ] {
            let answer = hub.call(&mut person, "workflow_start", arguments).await;
            assert_eq!(answer["isError"], true, "{answer}");
        }
    }
    // A hub that hands out no build has no one-line install to give, and says so.
    let hub = hosted_with(None, |_, _| json!({})).await;
    let mut person = hub.first_connect("Claude").await;
    let listed = structured(&hub, &mut person, "workflow_list", json!({})).await;
    for workflow in listed["workflows"].as_array().unwrap() {
        let id = workflow["id"].as_str().unwrap();
        match id {
            "connect-linux" | "connect-windows" | "get-started" => {
                assert_eq!(workflow["available"], false, "{id}");
                assert!(workflow["why"].as_str().unwrap().contains("hands out no builds"), "{workflow}");
            }
            "phone-approvals" => assert_eq!(workflow["available"], true),
            _ => {}
        }
    }
    // One with Linux builds alone has no Windows line, and offers no Windows to pick.
    let hub = hosted_with(None, |dir, _| {
        let dist = dist(dir);
        let sums = std::fs::read_to_string(dist.join("SHA256SUMS")).unwrap();
        let linux: String = sums.lines().filter(|l| !l.contains("windows") && !l.contains(".ps1")).map(|l| format!("{l}\n")).collect();
        std::fs::write(dist.join("SHA256SUMS"), linux).unwrap();
        json!({ "downloads": dist.to_string_lossy() })
    })
    .await;
    let mut person = hub.first_connect("Claude").await;
    let listed = structured(&hub, &mut person, "workflow_list", json!({})).await;
    let windows = listed["workflows"].as_array().unwrap().iter().find(|w| w["id"] == "connect-windows").unwrap().clone();
    assert_eq!(windows["available"], false);
    assert!(windows["why"].as_str().unwrap().contains("no Windows build"), "{windows}");
    let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "get-started" })).await;
    let path = path_of(started["link"].as_str().unwrap(), &hub);
    let (_, body) = page(&hub, &path, NOBODY).await;
    assert!(body.contains("name=\"os\" value=\"linux\""), "{body}");
    assert!(!body.contains("name=\"os\" value=\"windows\""), "{body}");
    let (status, _) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "windows"), ("name", "pc")]).await;
    assert_eq!(status, 303, "an OS it cannot give a line for goes back to the page");
    assert_eq!(hub.devices.held(&who(&hub, &mut person).await.0).unwrap(), 0);
}

#[tokio::test]
async fn opening_the_link_needs_no_sign_in_spends_nothing_and_shows_no_code() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({})).await;
        let mut person = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut person).await;
        let started = structured(
            &hub,
            &mut person,
            "workflow_start",
            json!({ "workflow": "connect-linux", "machine_name": "nas" }),
        )
        .await;
        let link = started["link"].as_str().unwrap();
        let path = path_of(link, &hub);
        assert_eq!(step(&started, 0)["state"], "current");
        assert_eq!(step(&started, 0)["machine"], "nas");
        let left = started["expires_at"].as_u64().unwrap() - now_secs();
        assert!((1790..=1800).contains(&left), "30 minutes by default: {left}");
        assert!(started["next"].as_str().unwrap().contains("30 minutes"), "{started}");
        let before = events(&hub, &tenant).await;
        assert_eq!(before.last().unwrap(), "workflow.started");
        // What a person, a chat app's preview or a mail scanner gets, as often as it asks:
        // the workflow and its steps, the button, and never a code or a sign-in.
        for _ in 0..3 {
            let (status, body) = page(&hub, &path, NOBODY).await;
            assert_eq!(status, 200);
            assert!(body.contains("<h1>Connect a Linux server</h1>"), "{body}");
            assert!(body.contains("Run one line on your server"), "{body}");
            assert!(body.contains("id=\"get-command-1\""), "{body}");
            assert!(body.contains("value=\"nas\""), "{body}");
            assert!(body.contains("Step 1 of 2"), "{body}");
            assert!(body.contains("<script src=\"/assets/workflow.js\" defer></script>"), "{body}");
            for absent in ["dvg-", "/join/", "Sign in", "sign-in", "passkey", "http-equiv", subject.as_str(), tenant.as_str()] {
                assert!(!body.contains(absent), "the page shows {absent}:\n{body}");
            }
            let answer = hub.get(&format!("{path}/status"), NOBODY).await;
            assert_eq!(answer.status(), 200);
            let answer: Value = answer.json().await.unwrap();
            assert_eq!(answer["state"], "open");
            assert_eq!(answer["keep"], false);
            assert_eq!(answer["steps"][0]["state"], "current");
        }
        assert_eq!(events(&hub, &tenant).await, before, "opening it writes nothing");
        assert_eq!(hub.devices.held(&subject).unwrap(), 0, "and issues no code");
        // A link that never was, and a malformed one: the same answer as a dead one.
        for wrong in ["/go/dgw_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "/go/x"] {
            assert_eq!(page(&hub, wrong, NOBODY).await.0, 404, "{wrong}");
            assert_eq!(hub.get(&format!("{wrong}/status"), NOBODY).await.status(), 404);
        }
    }
}

#[tokio::test]
async fn get_my_command_issues_the_line_and_the_step_ticks_once_online() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({})).await;
        let mut person = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut person).await;
        let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-linux" })).await;
        let id = started["id"].as_str().unwrap().to_string();
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        assert_eq!(step(&started, 0)["machine"], "server", "the site's name for a Linux machine");
        // From another site: nothing.
        let refused = hub
            .http
            .post(format!("{}{path}/command", hub.url))
            .header(header::ORIGIN, "https://evil.example")
            .form(&[("step", "0"), ("os", "linux"), ("name", "server")])
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 403);
        assert_eq!(hub.devices.held(&subject).unwrap(), 0);
        // Pressed, in a browser that never signed in: a code, in this browser's own cookie.
        let pressed = hub.form(&format!("{path}/command"), NOBODY, &[("step", "0"), ("os", "linux"), ("name", "server")]).await;
        assert_eq!(pressed.status(), 303);
        assert_eq!(pressed.headers()[header::LOCATION].to_str().unwrap(), format!("{path}#step-1"));
        assert!(set_cookie(&pressed, "dg_session").is_none(), "nobody is signed in");
        let line_cookie = set_cookie(&pressed, "dg_wfc").expect("the line's cookie");
        let browser = set_cookie(&pressed, "dg_wfb").expect("the browser's own cookie");
        assert!(browser.starts_with("dg_wfb=dgb_"), "{browser}");
        assert_eq!(hub.devices.held(&subject).unwrap(), 1);
        let cookies = format!("{line_cookie}; {browser}");
        let code = code_in(&cookies);
        assert!(crate::join::valid_code(&code), "{code}");
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(body.contains(&format!("curl -fsSL {}/join/{code} | sudo sh</code>", hub.url)), "{body}");
        assert!(body.contains("class=\"copy\" data-copy=\"command-1\" hidden"), "{body}");
        // Anyone else opening the link sees the button, never that code.
        let (_, other) = page(&hub, &path, NOBODY).await;
        assert!(!other.contains(&code) && other.contains("id=\"get-command-1\""), "{other}");
        // The join script for that code is served, and fetching it spends nothing.
        let script = hub.get(&format!("/join/{code}"), NOBODY).await.text().await.unwrap();
        assert!(script.contains("device='t"), "{script}");
        let status = structured(&hub, &mut person, "workflow_status", json!({ "id": id })).await;
        assert_eq!(step(&status, 0)["state"], "current");
        assert_eq!(step(&status, 0)["fact"], "Waiting for server to join.");
        // The machine enrolls with that code: joined, offline.
        let stored = format!("{}-server", crate::accounts::tenants::tag(&tenant));
        hub.devices.enroll(&stored, &code, &"ab".repeat(32), false).unwrap();
        let status = structured(&hub, &mut person, "workflow_status", json!({ "id": id })).await;
        assert_eq!(step(&status, 0)["state"], "current");
        assert_eq!(step(&status, 0)["fact"], "server joined but is offline.");
        // Online: the step is done, and so is the workflow; its link grants nothing more.
        let _link = hub.hub.links.connect_for_test(&stored);
        let status = structured(&hub, &mut person, "workflow_status", json!({ "id": id })).await;
        assert_eq!(step(&status, 0)["state"], "done");
        assert_eq!(step(&status, 0)["fact"], "server is online.");
        assert_eq!(step(&status, 1)["state"], "done");
        assert_eq!(status["state"], "completed");
        assert!(status.get("link").is_none(), "{status}");
        let chain = events(&hub, &tenant).await;
        for (event, times) in [
            ("workflow.started", 1),
            ("workflow.command", 1),
            ("workflow.step_done", 2),
            ("workflow.completed", 1),
        ] {
            assert_eq!(chain.iter().filter(|e| *e == event).count(), times, "{event} in {chain:?}");
        }
        assert!(hub.accounts.chain_intact(&tenant).await.unwrap());
        // Looking again records nothing twice.
        structured(&hub, &mut person, "workflow_status", json!({ "id": id })).await;
        assert_eq!(events(&hub, &tenant).await, chain);
        // Done: the page says so, with no button; the status agrees.
        let (status, body) = page(&hub, &path, NOBODY).await;
        assert_eq!(status, 200);
        assert!(body.contains("id=\"finished\" role=\"status\">"), "{body}");
        assert!(!body.contains("get-command") && !body.contains("data-state=\"current\""), "{body}");
        let answer: Value = hub.get(&format!("{path}/status"), NOBODY).await.json().await.unwrap();
        assert_eq!(answer["state"], "completed");
        assert_eq!(answer["steps"][0]["state"], "done");
        // Pressing again after it completed issues nothing.
        let (late, _) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "linux"), ("name", "other")]).await;
        assert_eq!(late, 404);
        assert_eq!(hub.devices.held(&subject).unwrap(), 1);
        let late = turn_on_phone(&hub, &path, NOBODY, 9).await;
        assert_eq!(late.status(), 404);
    }
}

#[tokio::test]
async fn another_account_reaches_nothing_of_a_workflow_through_its_ai() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({})).await;
        let mut alice = hub.first_connect("Claude").await;
        let mut bob = hub.first_connect("Cursor").await;
        let started = structured(&hub, &mut alice, "workflow_start", json!({ "workflow": "connect-linux" })).await;
        let id = started["id"].as_str().unwrap();
        // Bob's AI with Alice's workflow id: not found, in every tool.
        for (tool, arguments) in [
            ("workflow_status", json!({ "id": id })),
            ("workflow_cancel", json!({ "id": id })),
            ("workflow_wait", json!({ "id": id, "timeout_secs": 1 })),
        ] {
            let answer = hub.call(&mut bob, tool, arguments).await;
            assert_eq!(answer["isError"], true, "{tool}: {answer}");
            assert!(answer.to_string().contains("unknown_workflow"), "{tool}: {answer}");
        }
        // Alice's workflow is as it was, and Bob lists none of hers.
        let status = structured(&hub, &mut alice, "workflow_status", json!({ "id": id })).await;
        assert_eq!(status["state"], "open");
        let tools: Vec<String> = hub.mcp(&mut bob, "tools/list", json!({})).await["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for tool in ["workflow_list", "workflow_start", "workflow_status", "workflow_wait", "workflow_cancel"] {
            assert!(tools.iter().any(|t| t == tool), "{tool} in {tools:?}");
        }
        assert!(!tools.iter().any(|t| t == "workflow_pair"), "no pairing: {tools:?}");
    }
}

#[tokio::test]
async fn expiry_cancelling_and_a_new_link_each_end_the_old_one() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({ "workflows": { "link_ttl_secs": 300 } })).await;
        let mut person = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut person).await;
        // The configured life.
        let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-linux" })).await;
        let left = started["expires_at"].as_u64().unwrap() - now_secs();
        assert!((295..=300).contains(&left), "{left}");
        assert!(started["next"].as_str().unwrap().contains("5 minutes"), "{started}");
        // Started ten minutes ago with five to live: expired, and it does nothing.
        let site = crate::site::builtin();
        let (old, token, _) = hub
            .accounts
            .start_workflow(
                &tenant,
                &subject,
                site.workflows.get("get-started").unwrap(),
                None,
                300,
                [3; 12],
                [3; 32],
                now_secs() - 600,
            )
            .await
            .unwrap();
        let path = format!("/go/{token}");
        assert_eq!(page(&hub, &path, NOBODY).await.0, 404);
        assert_eq!(hub.get(&format!("{path}/status"), NOBODY).await.status(), 404);
        let status = structured(&hub, &mut person, "workflow_status", json!({ "id": old })).await;
        assert_eq!(status["state"], "expired");
        assert!(status["next"].as_str().unwrap().contains("start the workflow again"));
        let (pressed, _) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "windows"), ("name", "pc")]).await;
        assert_eq!(pressed, 404);
        assert_eq!(turn_on_phone(&hub, &path, NOBODY, 1).await.status(), 404);
        assert_eq!(hub.devices.held(&subject).unwrap(), 0);
        // Cancelled by its AI: dead at once.
        let id = started["id"].as_str().unwrap();
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        assert_eq!(page(&hub, &path, NOBODY).await.0, 200);
        let cancelled = structured(&hub, &mut person, "workflow_cancel", json!({ "id": id })).await;
        assert_eq!(cancelled["cancelled"], true);
        assert_eq!(page(&hub, &path, NOBODY).await.0, 404);
        let (pressed, _) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "linux"), ("name", "nas")]).await;
        assert_eq!(pressed, 404);
        let again = structured(&hub, &mut person, "workflow_cancel", json!({ "id": id })).await;
        assert_eq!(again["cancelled"], false);
        let status = structured(&hub, &mut person, "workflow_status", json!({ "id": id })).await;
        assert_eq!(status["state"], "cancelled");
        // Starting the same workflow again replaces its earlier link.
        let first = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-linux" })).await;
        let second = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-linux" })).await;
        let first = path_of(first["link"].as_str().unwrap(), &hub);
        let second = path_of(second["link"].as_str().unwrap(), &hub);
        assert_eq!(page(&hub, &first, NOBODY).await.0, 404);
        assert_eq!(page(&hub, &second, NOBODY).await.0, 200);
        let chain = events(&hub, &tenant).await;
        assert_eq!(chain.iter().filter(|e| *e == "workflow.cancelled").count(), 2, "{chain:?}");
        // The link is only ever kept as its hash.
        let token = second.trim_start_matches("/go/");
        let row = hub.accounts.workflow_by_link(token).await.unwrap().unwrap();
        assert_eq!(row.state, "open");
        assert_eq!(crate::accounts::workflows::link_hash(token).len(), 64);
    }
}

#[tokio::test]
async fn keeping_the_account_is_offered_after_the_first_success_to_the_browser_that_did_it() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({})).await;
        let mut person = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut person).await;
        let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-linux" })).await;
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        let keep = |cookies: Option<&str>| {
            let mut request = hub
                .http
                .post(format!("{}{path}/keep", hub.url))
                .header(header::ORIGIN, &hub.url)
                .form(&[("how", "passkey")]);
            if let Some(cookies) = cookies {
                request = request.header(header::COOKIE, cookies.to_string());
            }
            request.send()
        };
        let (_, cookies) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "linux"), ("name", "server")]).await;
        // Before anything worked: no offer, and asking opens nothing.
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(body.contains("id=\"keep\" aria-labelledby=\"keep-title\" hidden"), "{body}");
        let answer = keep(Some(&cookies)).await.unwrap();
        assert_eq!(answer.status(), 303);
        assert!(set_cookie(&answer, "dg_session").is_none());
        // The machine joins and comes online: the first success.
        let stored = format!("{}-server", crate::accounts::tenants::tag(&tenant));
        hub.devices.enroll(&stored, &code_in(&cookies), &"ab".repeat(32), false).unwrap();
        let _link = hub.hub.links.connect_for_test(&stored);
        let answer: Value = hub.get(&format!("{path}/status"), Some(&cookies)).await.json().await.unwrap();
        assert_eq!(answer["state"], "completed");
        assert_eq!(answer["keep"], true, "the browser that did it may keep the account");
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(body.contains("<section class=\"keep\" id=\"keep\" aria-labelledby=\"keep-title\"><h2"), "{body}");
        assert!(body.contains(&format!("id=\"passkey-add\" type=\"button\" data-prepare=\"{path}/keep\" data-return-to=\"{path}\"")), "{body}");
        assert!(body.contains("<script src=\"/assets/passkey.js\" defer></script>"), "{body}");
        // Another browser holding the same link is offered nothing and gets nothing.
        let (_, other) = page(&hub, &path, NOBODY).await;
        assert!(!other.contains("id=\"keep\""), "{other}");
        let answer: Value = hub.get(&format!("{path}/status"), NOBODY).await.json().await.unwrap();
        assert_eq!(answer["keep"], false);
        let stranger = format!("dg_wfb={}", crate::accounts::workflows::new_browser_secret([1; 32]));
        for cookies in [None, Some(stranger.as_str())] {
            let answer = keep(cookies).await.unwrap();
            assert_eq!(answer.status(), 303);
            assert!(set_cookie(&answer, "dg_session").is_none());
        }
        // From another site: refused.
        let refused = hub
            .http
            .post(format!("{}{path}/keep", hub.url))
            .header(header::ORIGIN, "https://evil.example")
            .header(header::COOKIE, &cookies)
            .form(&[("how", "passkey")])
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 403);
        // The browser that did it keeps the account: a session of it, for its passkey.
        let answer = keep(Some(&cookies)).await.unwrap();
        assert_eq!(answer.status(), 204);
        let session = set_cookie(&answer, "dg_session").expect("a session for the passkey");
        let me: Value = hub.get("/api/v1/me", Some(&session)).await.json().await.unwrap();
        assert_eq!(me["subject"], subject.as_str(), "{me}");
        assert!(events(&hub, &tenant).await.iter().any(|e| e == "session.opened"));
        // Once the account is secured there is nothing more to keep.
        secure(&hub, &subject, 41).await;
        let answer = keep(Some(&cookies)).await.unwrap();
        assert_eq!(answer.status(), 303);
        assert!(set_cookie(&answer, "dg_session").is_none());
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(!body.contains("id=\"keep\""), "{body}");
    }
    // With Google or GitHub configured, those are offered instead of a passkey.
    let hub = hosted_with(None, |dir, _| {
        let secret = dir.join("provider-secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        json!({
            "downloads": dist(dir).to_string_lossy(),
            "auth": { "providers": [{
                "kind": "github", "client_id": "client-1",
                "client_secret_file": secret.to_string_lossy(),
                "authorize_url": "https://github.example/login/oauth/authorize",
                "token_url": "https://github.example/login/oauth/access_token",
                "api_url": "https://api.github.example"
            }] }
        })
    })
    .await;
    let mut person = hub.first_connect("Claude").await;
    let (_, tenant) = who(&hub, &mut person).await;
    let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-linux" })).await;
    let path = path_of(started["link"].as_str().unwrap(), &hub);
    let (_, cookies) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "linux"), ("name", "server")]).await;
    let stored = format!("{}-server", crate::accounts::tenants::tag(&tenant));
    hub.devices.enroll(&stored, &code_in(&cookies), &"ab".repeat(32), false).unwrap();
    let _link = hub.hub.links.connect_for_test(&stored);
    let (_, body) = page(&hub, &path, Some(&cookies)).await;
    assert!(body.contains("name=\"how\" value=\"github\""), "{body}");
    assert!(body.contains("Continue with GitHub"), "{body}");
    assert!(!body.contains("passkey-add"), "{body}");
    let answer = hub.form(&format!("{path}/keep"), Some(&cookies), &[("how", "github")]).await;
    assert_eq!(answer.status(), 303);
    let location = answer.headers()[header::LOCATION].to_str().unwrap().to_string();
    assert!(location.starts_with("/auth/provider/github?return_to=%2Fgo%2Fdgw_"), "{location}");
    assert!(location.ends_with("&link=1"), "{location}");
    assert!(set_cookie(&answer, "dg_session").is_some());
    let answer = hub.form(&format!("{path}/keep"), Some(&cookies), &[("how", "passkey")]).await;
    assert_eq!(answer.status(), 303, "a way this page does not offer");
}

#[tokio::test]
async fn each_step_kind_is_done_when_the_hub_sees_it_and_a_wait_returns_the_moment_it_is() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({})).await;
        let mut person = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut person).await;
        let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "get-started" })).await;
        let id = started["id"].as_str().unwrap().to_string();
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        let column = |status: &Value, key: &str| -> Vec<String> {
            status["steps"].as_array().unwrap().iter().map(|s| s[key].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(column(&started, "kind"), ["machine", "phone", "secure_account", "done"]);
        assert_eq!(column(&started, "state"), ["current", "pending", "pending", "pending"]);
        assert_eq!(step(&started, 2)["optional"], true);
        assert_eq!(step(&started, 2)["fact"], "Your account is not secured yet.");
        // The page: the OS to pick, no Mac to pick and why, the phone's button.
        let (_, body) = page(&hub, &path, NOBODY).await;
        for os in ["linux", "windows", "docker", "host"] {
            assert!(body.contains(&format!("name=\"os\" value=\"{os}\"")), "{os}: {body}");
        }
        assert!(!body.contains("value=\"mac\""));
        assert!(body.contains("not available yet"), "{body}");
        assert!(body.contains(&format!("data-subscribe=\"{path}/phone\" data-auto=\"off\"")), "{body}");
        assert!(body.contains("<script src=\"/assets/push.js\" defer></script>"), "{body}");
        assert!(body.contains("data-optional=\"true\""), "{body}");
        // Without an OS picked, nothing is issued.
        let (status, _) = press(&hub, &path, NOBODY, &[("step", "0"), ("name", "box")]).await;
        assert_eq!(status, 303);
        assert_eq!(hub.devices.held(&subject).unwrap(), 0);
        // machine: the person picks Docker on the page.
        let (status, cookies) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "docker"), ("name", "box")]).await;
        assert_eq!(status, 303);
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(
            body.contains(&format!("curl -fsSL {}/join/dvg-", hub.url)) && body.contains(".compose.yml -o compose.yml"),
            "{body}"
        );
        let stored = format!("{}-box", crate::accounts::tenants::tag(&tenant));
        hub.devices.enroll(&stored, &code_in(&cookies), &"cd".repeat(32), false).unwrap();
        // A wait returns when the machine connects.
        let at = Instant::now();
        let (waited, _link) = tokio::join!(
            structured(&hub, &mut person, "workflow_wait", json!({ "id": id, "timeout_secs": 60 })),
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                hub.hub.links.connect_for_test(&stored)
            }
        );
        assert!(at.elapsed() < Duration::from_secs(30), "{:?}", at.elapsed());
        assert_eq!(waited["changed"], true, "{waited}");
        assert_eq!(step(&waited, 0)["state"], "done", "{waited}");
        assert_eq!(step(&waited, 0)["fact"], "box is online.");
        assert_eq!(step(&waited, 0)["os"], "docker");
        assert_eq!(step(&waited, 1)["state"], "current");
        assert_eq!(step(&waited, 1)["fact"], "No phone has notifications on yet.");
        // phone: the same link opened on the phone, which never signed in; a wait wakes.
        let refused = hub
            .http
            .post(format!("{}{path}/phone", hub.url))
            .header(header::ORIGIN, "https://evil.example")
            .json(&subscription(5))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 403);
        let at = Instant::now();
        let (waited, subscribed) = tokio::join!(
            structured(&hub, &mut person, "workflow_wait", json!({ "id": id, "timeout_secs": 60 })),
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                turn_on_phone(&hub, &path, NOBODY, 5).await
            }
        );
        assert_eq!(subscribed.status(), 204);
        assert!(set_cookie(&subscribed, "dg_wfb").is_some(), "the phone did a step");
        assert!(at.elapsed() < Duration::from_secs(30), "{:?}", at.elapsed());
        assert_eq!(waited["changed"], true, "{waited}");
        assert_eq!(step(&waited, 1)["state"], "done");
        assert_eq!(step(&waited, 1)["fact"], "Notifications are on for 1 phone or browser.");
        // Keeping the account is optional: done or not, the workflow completes without it.
        assert_eq!(step(&waited, 2)["state"], "pending");
        assert_eq!(step(&waited, 3)["state"], "done");
        assert_eq!(waited["state"], "completed");
        // secure_account is done when the account is secured.
        secure(&hub, &subject, 7).await;
        let status = structured(&hub, &mut person, "workflow_status", json!({ "id": id })).await;
        assert_eq!(step(&status, 2)["state"], "done");
        assert_eq!(step(&status, 2)["fact"], "Your account is secured.");
        // A wait on a finished workflow answers at once.
        let at = Instant::now();
        let waited = structured(&hub, &mut person, "workflow_wait", json!({ "id": id, "timeout_secs": 60 })).await;
        assert!(at.elapsed() < Duration::from_secs(10), "{:?}", at.elapsed());
        assert_eq!(waited["changed"], false);
        assert_eq!(waited["state"], "completed");
        // A wait with nothing moving times out, unchanged; a phone workflow takes no line.
        let open = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-windows" })).await;
        let at = Instant::now();
        let waited = structured(&hub, &mut person, "workflow_wait", json!({ "id": open["id"], "timeout_secs": 1 })).await;
        assert!(at.elapsed() >= Duration::from_secs(1));
        assert_eq!(waited["changed"], false);
        assert_eq!(waited["state"], "open");
        let windows = path_of(open["link"].as_str().unwrap(), &hub);
        assert_eq!(turn_on_phone(&hub, &windows, NOBODY, 6).await.status(), 404, "no phone step there");
        // A phone is on already: a phone workflow is done the moment it starts, and its
        // link grants nothing.
        let phone = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "phone-approvals" })).await;
        assert_eq!(step(&phone, 0)["state"], "done", "{phone}");
        let phone = path_of(phone["link"].as_str().unwrap(), &hub);
        let (pressed, _) = press(&hub, &phone, NOBODY, &[("step", "0"), ("os", "linux"), ("name", "x")]).await;
        assert_eq!(pressed, 404);
        assert_eq!(hub.devices.held(&subject).unwrap(), 1);
    }
}

#[tokio::test]
async fn a_plans_machine_limit_is_said_plainly_to_the_ai_and_on_the_page() {
    for store in stores().await {
        let hub = hosted_with(store.as_deref(), |dir, _| {
            let site = site_dir(dir, |value| {
                value["plans"] = json!([{ "id": "free", "name": "Free", "price": "Free",
                                          "limits": { "machines": 1 } }]);
            });
            json!({
                "downloads": dist(dir).to_string_lossy(),
                "site": { "dir": site.to_string_lossy() },
                "tenancy": { "mode": "multi", "starts_per_hour": 100,
                             "unsecured": { "machines": 10, "clients": 10, "idle_days": 14 } }
            })
        })
        .await;
        let mut person = hub.first_connect("Claude").await;
        let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "connect-linux" })).await;
        assert_eq!(step(&started, 0)["state"], "current");
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        // One machine already: the plan's one.
        let added = hub.call(&mut person, "machine_add", json!({ "name": "first" })).await;
        assert_ne!(added["isError"], true, "{added}");
        let status = structured(&hub, &mut person, "workflow_status", json!({ "id": started["id"] })).await;
        assert_eq!(step(&status, 0)["state"], "unavailable", "{status}");
        let why = step(&status, 0)["why"].as_str().unwrap().to_string();
        assert!(why.contains("Your Free plan holds 1 machine here"), "{why}");
        assert!(status["next"].as_str().unwrap().contains("cannot be done"), "{status}");
        let (_, body) = page(&hub, &path, NOBODY).await;
        assert!(body.contains("data-state=\"unavailable\""), "{body}");
        assert!(body.contains(&escaped(&why)), "{body}");
        assert!(!body.contains("get-command-1"), "{body}");
        let pressed = hub.form(&format!("{path}/command"), NOBODY, &[("step", "0"), ("os", "linux"), ("name", "second")]).await;
        assert_eq!(pressed.status(), 403);
        let (_, body) = Hosted::text(pressed).await;
        assert!(body.contains("Your Free plan holds 1 machine"), "{body}");
        // Starting another machine workflow now says so at once.
        let answer = hub.call(&mut person, "workflow_start", json!({ "workflow": "connect-windows" })).await;
        assert_eq!(answer["isError"], true, "{answer}");
        assert!(answer.to_string().contains("Your Free plan holds 1 machine"), "{answer}");
        // A phone takes no machine: still fine.
        let phone = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "phone-approvals" })).await;
        assert_eq!(step(&phone, 0)["state"], "current");
    }
}

#[tokio::test]
async fn a_waiting_tenant_gets_the_early_access_message() {
    let hub = hosted_with(None, |dir, _| {
        let site = site_dir(dir, |value| value["early_access"] = json!({ "mode": "early_access" }));
        json!({ "downloads": dist(dir).to_string_lossy(), "site": { "dir": site.to_string_lossy() } })
    })
    .await;
    let mut person = hub.first_connect("Claude").await;
    let note = structured(&hub, &mut person, "whoami", json!({})).await["access_note"]
        .as_str()
        .unwrap()
        .to_string();
    for (tool, arguments) in [
        ("workflow_list", json!({})),
        ("workflow_start", json!({ "workflow": "connect-linux" })),
    ] {
        let answer = hub.call(&mut person, tool, arguments).await;
        assert_eq!(answer["isError"], true, "{tool}");
        assert_eq!(answer["content"][0]["text"], note.as_str(), "{tool}");
    }
}

#[tokio::test]
async fn a_neutral_sites_workflow_pages_name_only_that_site() {
    let neutral = crate::site::tests::neutral_dir();
    let hub = hosted_with(None, move |dir, _| {
        let site = dir.join("neutral");
        crate::site::tests::copy(&neutral, &site);
        let path = site.join("site.json");
        let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        value["early_access"] = json!({ "mode": "open" });
        std::fs::write(&path, value.to_string()).unwrap();
        json!({ "downloads": dist(dir).to_string_lossy(), "site": { "dir": site.to_string_lossy() } })
    })
    .await;
    let mut person = hub.first_connect("Claude").await;
    let tenant = who(&hub, &mut person).await.1;
    let mut seen = Vec::new();
    let listed = structured(&hub, &mut person, "workflow_list", json!({})).await;
    seen.push(("workflow_list".to_string(), listed.to_string()));
    for (workflow, name) in [("get-started", "desk"), ("connect-linux", "nas")] {
        let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": workflow })).await;
        seen.push((format!("{workflow} started"), started.to_string()));
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        seen.push((format!("{workflow} page"), page(&hub, &path, NOBODY).await.1));
        let (_, cookies) = press(&hub, &path, NOBODY, &[("step", "0"), ("os", "linux"), ("name", name)]).await;
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(body.contains("/join/dvg-"), "{body}");
        seen.push((format!("{workflow} with its line page"), body));
        let stored = format!("{}-{name}", crate::accounts::tenants::tag(&tenant));
        hub.devices.enroll(&stored, &code_in(&cookies), &"ef".repeat(32), false).unwrap();
        let _link = hub.hub.links.connect_for_test(&stored);
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(body.contains("id=\"keep\""), "{body}");
        seen.push((format!("{workflow} keep page"), body));
        let status: Value = hub.get(&format!("{path}/status"), Some(&cookies)).await.json().await.unwrap();
        seen.push((format!("{workflow} status"), status.to_string()));
    }
    for workflow in ["connect-claude", "connect-chatgpt", "connect-grok", "connect-cursor", "phone-approvals"] {
        let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": workflow })).await;
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        let (_, body) = page(&hub, &path, NOBODY).await;
        seen.push((format!("{workflow} page"), body));
    }
    seen.push(("dead link".into(), page(&hub, "/go/x", NOBODY).await.1));
    let script = hub.get("/assets/workflow.js", NOBODY).await.text().await.unwrap();
    seen.push(("workflow.js".into(), script));
    for (what, text) in seen {
        crate::site::tests::assert_no_brand(&what, &text);
        if what.ends_with(" page") {
            assert!(text.contains("Lumen Relay"), "{what} names its own site: {text}");
        }
    }
}

/// Another AI client connects in a browser holding `cookie` (a session of an account): the
/// consent page names that account, Connect, and the client's tokens.
async fn connect_in(hub: &Hosted, app: &str, cookie: &str) -> Person {
    use crate::{
        accounts::oauth::s256,
        tenant_tests::{query_param, web_encode},
    };
    let redirect = "http://127.0.0.1:9/callback";
    let registered: Value = hub
        .http
        .post(format!("{}/oauth/register", hub.url))
        .json(&json!({ "client_name": app, "redirect_uris": [redirect] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client = registered["client_id"].as_str().unwrap().to_string();
    let verifier = "w".repeat(64);
    let challenge = s256(&verifier);
    let mcp = format!("{}/mcp", hub.url);
    let query = format!(
        "response_type=code&client_id={client}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=s2&resource={}",
        web_encode(redirect),
        web_encode(&mcp)
    );
    let (status, page) = Hosted::text(hub.get(&format!("/oauth/authorize?{query}"), Some(cookie)).await).await;
    assert_eq!(status, 200, "{page}");
    assert!(page.contains(&format!("<h1>Connect {app}?</h1>")), "{page}");
    assert!(!page.contains("your new"), "it joins the account this browser is in: {page}");
    let answer = hub
        .form(
            "/oauth/authorize",
            Some(cookie),
            &[
                ("client_id", &client),
                ("redirect_uri", redirect),
                ("code_challenge", &challenge),
                ("resource", &mcp),
                ("scope", "mcp offline_access"),
                ("state", "s2"),
                ("decision", "approve"),
            ],
        )
        .await;
    assert_eq!(answer.status(), 303);
    let location = answer.headers()[header::LOCATION].to_str().unwrap().to_string();
    let code = query_param(&location, "code").expect("a code");
    let tokens: Value = hub
        .http
        .post(format!("{}/oauth/token", hub.url))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", redirect),
            ("client_id", &client),
            ("code_verifier", &verifier),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    Person {
        cookie: cookie.to_string(),
        token: tokens["access_token"].as_str().unwrap().to_string(),
        session: None,
    }
}

#[tokio::test]
async fn an_ai_client_step_is_done_when_a_client_of_its_family_calls_for_the_tenant() {
    for store in stores().await {
        let hub = hub_on(store.as_deref(), json!({})).await;
        let mut claude = hub.first_connect("Claude").await;
        let (subject, tenant) = who(&hub, &mut claude).await;
        assert_eq!(hub.accounts.client_families(&tenant).await.unwrap(), ["claude"], "Claude's own call counts");
        let started = structured(&hub, &mut claude, "workflow_start", json!({ "workflow": "connect-chatgpt" })).await;
        let id = started["id"].as_str().unwrap().to_string();
        let path = path_of(started["link"].as_str().unwrap(), &hub);
        assert_eq!(step(&started, 0)["kind"], "ai_client");
        assert_eq!(step(&started, 0)["client"], "chatgpt");
        assert_eq!(step(&started, 0)["state"], "current");
        assert_eq!(step(&started, 0)["fact"], "Waiting for ChatGPT to connect.");
        // The page: ChatGPT's own steps, the address and Codex's command with Copy buttons,
        // and the button that joins this browser to the account.
        let (_, body) = page(&hub, &path, NOBODY).await;
        let mcp = format!("{}/mcp", hub.url);
        assert!(body.contains("Developer mode"), "{body}");
        assert!(body.contains(&format!("<code id=\"client-1-0\">{mcp}</code>")), "{body}");
        assert!(body.contains(&format!("<code id=\"client-1-1\">codex mcp add devagotchi --url {mcp}</code>")), "{body}");
        assert!(body.contains("data-copy=\"client-1-1\" hidden"), "{body}");
        assert!(body.contains("id=\"client-here-1\""), "{body}");
        // ChatGPT connected to someone else's account does not count here.
        let mut other = hub.first_connect("ChatGPT").await;
        structured(&hub, &mut other, "whoami", json!({})).await;
        let status = structured(&hub, &mut claude, "workflow_status", json!({ "id": id })).await;
        assert_eq!(step(&status, 0)["state"], "current");
        // From another site: nothing.
        let refused = hub
            .http
            .post(format!("{}{path}/client", hub.url))
            .header(header::ORIGIN, "https://evil.example")
            .form(&[("step", "0")])
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 403);
        // The person presses the button in the browser ChatGPT will open: a session of the
        // account, so ChatGPT's Connect lands there.
        let pressed = hub.form(&format!("{path}/client"), NOBODY, &[("step", "0")]).await;
        assert_eq!(pressed.status(), 303);
        let session = set_cookie(&pressed, "dg_session").expect("a session of the account");
        let browser = set_cookie(&pressed, "dg_wfb").expect("the browser's own cookie");
        let cookies = format!("{session}; {browser}");
        let (_, body) = page(&hub, &path, Some(&cookies)).await;
        assert!(body.contains("id=\"client-ready-1\""), "{body}");
        assert!(!body.contains("id=\"client-here-1\""), "{body}");
        assert!(events(&hub, &tenant).await.iter().any(|e| e == "session.opened"));
        // ChatGPT connects in that browser and makes a call: the step is done, and a wait
        // returns the moment it is.
        let mut chatgpt = connect_in(&hub, "ChatGPT", &session).await;
        let at = Instant::now();
        let (waited, ()) = tokio::join!(
            structured(&hub, &mut claude, "workflow_wait", json!({ "id": id, "timeout_secs": 60 })),
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let me = structured(&hub, &mut chatgpt, "whoami", json!({})).await;
                assert_eq!(me["subject"], subject.as_str(), "ChatGPT is on the same account");
            }
        );
        assert!(at.elapsed() < Duration::from_secs(30), "{:?}", at.elapsed());
        assert_eq!(waited["changed"], true, "{waited}");
        assert_eq!(step(&waited, 0)["state"], "done");
        assert_eq!(step(&waited, 0)["fact"], "ChatGPT is connected.");
        assert_eq!(waited["state"], "completed");
        let mut families = hub.accounts.client_families(&tenant).await.unwrap();
        families.sort();
        assert_eq!(families, ["chatgpt", "claude"]);
        // A client that is already connected: done the moment it starts.
        let again = structured(&hub, &mut claude, "workflow_start", json!({ "workflow": "connect-claude" })).await;
        assert_eq!(step(&again, 0)["state"], "done", "{again}");
        // A secured account signs in on the client's page; the link opens no session.
        secure(&hub, &subject, 51).await;
        let cursor = structured(&hub, &mut claude, "workflow_start", json!({ "workflow": "connect-cursor" })).await;
        let path = path_of(cursor["link"].as_str().unwrap(), &hub);
        let (_, body) = page(&hub, &path, NOBODY).await;
        assert!(body.contains("sign in there as you usually do"), "{body}");
        assert!(!body.contains("id=\"client-here-1\""), "{body}");
        let entry = format!("{{ &quot;mcpServers&quot;: {{ &quot;devagotchi&quot;: {{ &quot;url&quot;: &quot;{mcp}&quot; }} }} }}");
        assert!(body.contains(&entry), "Cursor's mcp.json entry, as is: {body}");
        let pressed = hub.form(&format!("{path}/client"), NOBODY, &[("step", "0")]).await;
        assert_eq!(pressed.status(), 303);
        assert!(set_cookie(&pressed, "dg_session").is_none());
    }
}

#[tokio::test]
async fn the_sites_workflows_name_only_clients_and_kinds_the_hub_runs() {
    let check = |steps: Value| -> Result<(), String> {
        let file: crate::site::WorkflowsFile = serde_json::from_value(json!({ "workflows": [
            { "id": "w", "title": "W", "intro": "", "steps": steps }
        ] }))
        .map_err(|error| error.to_string())?;
        file.check()
    };
    assert_eq!(check(json!([{ "kind": "ai_client", "client": "claude", "title": "C", "text": "" }])), Ok(()));
    for (steps, why) in [
        (json!([{ "kind": "ai_client", "title": "C", "text": "" }]), "names its client"),
        (json!([{ "kind": "ai_client", "client": "copilot", "title": "C", "text": "" }]), "not one the hub connects"),
        (json!([{ "kind": "machine", "client": "claude", "title": "C", "text": "" }]), "takes no client"),
        (json!([{ "kind": "teleport", "title": "C", "text": "" }]), "not one this hub runs"),
        (json!([{ "kind": "machine", "os": "mac", "title": "C", "text": "" }]), "not one the hub installs on"),
    ] {
        let refused = check(steps).unwrap_err();
        assert!(refused.contains(why), "{refused}");
    }
    // Every client this hub connects has its steps in the built-in site, one per family the
    // analytics tell apart.
    let site = crate::site::builtin();
    let clients: Vec<&str> = site.messages.workflow.clients.keys().map(String::as_str).collect();
    let mut known = crate::site::workflows::AI_CLIENTS.to_vec();
    known.sort_unstable();
    assert_eq!(clients, known);
    let mut families: Vec<&str> =
        crate::analytics::funnel::FAMILIES.iter().copied().filter(|f| *f != "other").collect();
    families.sort_unstable();
    assert_eq!(families, known);
    // And the names those clients register with land in their families.
    for (name, family) in [("Claude", "claude"), ("Claude Code", "claude"), ("ChatGPT", "chatgpt"),
                           ("Codex", "chatgpt"), ("Grok", "grok"), ("Cursor", "cursor")] {
        assert_eq!(crate::analytics::funnel::client_family(name), family, "{name}");
    }
}

#[tokio::test]
async fn a_client_the_site_marks_unavailable_is_offered_as_unavailable_with_its_words() {
    let why = "Grok is not offered on this hub yet.";
    let hub = hosted_with(None, move |dir, _| {
        let site = site_dir(dir, |_| {});
        std::fs::create_dir_all(site.join("content")).unwrap();
        let words = json!({ "workflow": { "clients": { "grok": { "unavailable": why } } } });
        std::fs::write(site.join("content/messages.json"), words.to_string()).unwrap();
        json!({ "downloads": dist(dir).to_string_lossy(), "site": { "dir": site.to_string_lossy() } })
    })
    .await;
    let mut person = hub.first_connect("Claude").await;
    let listed = structured(&hub, &mut person, "workflow_list", json!({})).await;
    let offer = |id: &str| listed["workflows"].as_array().unwrap().iter().find(|w| w["id"] == id).unwrap().clone();
    let grok = offer("connect-grok");
    assert_eq!(grok["available"], false, "{grok}");
    assert_eq!(grok["why"], why, "{grok}");
    assert_eq!(offer("connect-cursor")["available"], true);
    let answer = hub.call(&mut person, "workflow_start", json!({ "workflow": "connect-grok" })).await;
    assert_eq!(answer["isError"], true, "{answer}");
    assert!(answer.to_string().contains(why), "{answer}");
}

#[tokio::test]
async fn the_phone_step_shows_its_link_as_a_qr_code() {
    let hub = hub_on(None, json!({})).await;
    let mut person = hub.first_connect("Claude").await;
    let started = structured(&hub, &mut person, "workflow_start", json!({ "workflow": "phone-approvals" })).await;
    let path = path_of(started["link"].as_str().unwrap(), &hub);
    let (_, body) = page(&hub, &path, NOBODY).await;
    assert!(body.contains("<figure class=\"qr\" id=\"qr\"><svg role=\"img\" aria-label=\"This page&#39;s link as a QR code"), "{body}");
    assert!(!body.contains("<?xml"), "{body}");
    let svg = body.split("<figure class=\"qr\"").nth(1).unwrap();
    assert!(svg.contains("<path") && svg.contains("</svg></figure>"), "{svg}");
}
