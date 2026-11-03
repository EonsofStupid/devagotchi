//! Early access and plans on a hosted hub end to end over HTTP, the whole router as it
//! serves (`accounts/access.rs`, `tenant_access.rs`): a person waiting to be let in is
//! answered and not served on every write path, an operator lets them in and their next
//! call works, sign-up opening lets everyone in on their next call, and a plan's limits
//! hold at the boundary. On SQLite, and on Postgres when the test server is set (CI).

use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::tenant_tests::{Hosted, Person, fill, hosted_with, refused};

/// A site directory under `dir`: the built-in site with `edit` applied to its site.json
/// (every other file the built-in one's).
fn site_dir(dir: &Path, name: &str, edit: impl FnOnce(&mut Value)) -> std::path::PathBuf {
    let mut value: Value = serde_json::from_str(include_str!("../site/default/site.json")).unwrap();
    edit(&mut value);
    let site = dir.join(name);
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("site.json"), value.to_string()).unwrap();
    site
}

fn early_access(value: &mut Value) {
    value["early_access"] = json!({ "mode": "early_access" });
}

/// Two plans: Free (2 machines, 1 project, the default as the first) and Pro.
fn plans(value: &mut Value) {
    value["plans"] = json!([
        { "id": "free", "name": "Free", "price": "Free",
          "limits": { "projects": 1, "machines": 2 } },
        { "id": "pro", "name": "Pro", "price": "Ask",
          "limits": { "projects": "unlimited", "machines": 3 } }
    ]);
}

/// A hosted hub on `store` serving the site `edit` makes; an account nobody secured adds
/// ten machines, so the plan's limit is the one reached.
async fn hub_with(store: Option<&str>, edit: fn(&mut Value)) -> Hosted {
    hosted_with(store, move |dir, _| {
        let site = site_dir(dir, "site", edit);
        json!({
            "site": { "dir": site.to_string_lossy() },
            "tenancy": { "mode": "multi", "starts_per_hour": 100,
                         "unsecured": { "machines": 10, "clients": 10, "idle_days": 14 } }
        })
    })
    .await
}

/// Serve the site `edit` makes from now on, as the hub does when its site directory changes.
fn serve_site(hub: &Hosted, name: &str, edit: impl FnOnce(&mut Value)) {
    let dir = site_dir(hub.dir.path(), name, edit);
    let site = crate::site::Site::load(&dir).unwrap();
    hub.config.site_cell().set(std::sync::Arc::new(site));
}

async fn whoami(hub: &Hosted, person: &mut Person) -> Value {
    hub.call(person, "whoami", json!({})).await["structuredContent"].clone()
}

async fn stores() -> Vec<Option<String>> {
    let mut out = vec![None];
    if let Some(url) = store::testing::empty_postgres().await {
        out.push(Some(url));
    }
    out
}

#[tokio::test]
async fn a_waiting_tenant_is_answered_not_served_until_an_operator_lets_it_in() {
    for store in stores().await {
        waiting(hub_with(store.as_deref(), early_access).await).await;
    }
}

async fn waiting(hub: Hosted) {
    let mut person = hub.first_connect("Claude").await;
    let who = whoami(&hub, &mut person).await;
    assert_eq!(who["access"], "waiting", "{who}");
    assert!(who["waiting_since"].as_u64().is_some(), "{who}");
    let note = who["access_note"].as_str().unwrap().to_string();
    assert!(
        note.contains("early-access list")
            && note.contains("email when you're in")
            && note.contains(&format!("{}/account", hub.url)),
        "{note}"
    );
    let tenant = who["tenant"].as_str().unwrap().to_string();
    let subject = who["subject"].as_str().unwrap().to_string();

    // Every other tool answers with that one message instead of acting: an error result
    // (nothing ran) in plain words, with no structured content a client could misread.
    let listed = hub.mcp(&mut person, "tools/list", json!({})).await;
    let tools = listed["tools"].as_array().unwrap().clone();
    assert!(tools.len() >= 30, "{} tools", tools.len());
    let ids = json!({ "broker": "laptop", "machine": "laptop", "project": "mine",
                      "name": "laptop", "command": "id", "path": "/etc/hostname" });
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        if name == "whoami" {
            continue;
        }
        let arguments = fill(&tool["inputSchema"], &tool["inputSchema"], &ids);
        let answer = hub.call(&mut person, name, arguments).await;
        assert_eq!(answer["isError"], true, "{name}: {answer}");
        assert!(
            answer.get("structuredContent").is_none(),
            "{name}: {answer}"
        );
        assert_eq!(answer["content"][0]["text"], note.as_str(), "{name}");
    }

    // Adding a machine, from the app's API and from the page, is refused the same.
    let added = hub
        .http
        .post(format!("{}/api/v1/machines", hub.url))
        .bearer_auth(&person.token)
        .json(&json!({ "name": "laptop" }))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 403);
    let added: Value = added.json().await.unwrap();
    assert_eq!(added["error"], "early_access", "{added}");
    let (status, page) = Hosted::text(
        hub.form("/machines/add", Some(&person.cookie), &[("name", "laptop")])
            .await,
    )
    .await;
    assert_eq!(status, 403);
    assert!(page.contains("on the early-access list"), "{page}");
    // A code for it, however it came to be, enrolls nothing while it waits.
    let stored = format!("{}-laptop", crate::accounts::tenants::tag(&tenant));
    let code = hub
        .devices
        .issue_owned_code(&subject, &stored, [7; 16], 16)
        .unwrap();
    let enrolled = hub
        .http
        .post(format!("{}/enroll", hub.url))
        .json(&json!({ "name": stored, "code": code, "public_key": "laptop-key" }))
        .send()
        .await
        .unwrap();
    assert_eq!(enrolled.status(), 403);
    let enrolled: Value = enrolled.json().await.unwrap();
    assert_eq!(enrolled["error"], "early_access");
    assert!(
        hub.devices.get(&stored).unwrap().is_none(),
        "nothing joined"
    );
    // Its pages say where it stands.
    let (_, page) = Hosted::text(hub.get("/account", Some(&person.cookie)).await).await;
    assert!(
        page.contains("id=\"access\"") && page.contains("on the early-access list"),
        "{page}"
    );
    assert!(page.contains(" UTC"), "when it joined: {page}");
    let me: Value = hub
        .http
        .get(format!("{}/api/v1/me", hub.url))
        .bearer_auth(&person.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["access"]["state"], "waiting", "{me}");

    // The operator lets it in: its next call works.
    let admitted = hub
        .accounts
        .admit(std::slice::from_ref(&tenant), None, "acct_operator")
        .await
        .unwrap();
    assert_eq!(admitted.len(), 1);
    let machines = hub.call(&mut person, "machines", json!({})).await;
    assert!(!refused(&machines), "{machines}");
    let added = hub
        .call(&mut person, "machine_add", json!({ "name": "desk" }))
        .await;
    assert!(!refused(&added), "{added}");
    let enrolled = hub
        .http
        .post(format!("{}/enroll", hub.url))
        .json(&json!({ "name": stored, "code": code, "public_key": "laptop-key" }))
        .send()
        .await
        .unwrap();
    assert_eq!(enrolled.status(), 201, "its code works once it is in");
    let who = whoami(&hub, &mut person).await;
    assert_eq!(who["access"], "admitted");
    assert!(who.get("access_note").is_none() && who.get("waiting_since").is_none());
    assert_eq!(who["plan"], "free");
    let (_, page) = Hosted::text(hub.get("/account", Some(&person.cookie)).await).await;
    assert!(
        page.contains("You're in since") && page.contains("Free"),
        "{page}"
    );
}

#[tokio::test]
async fn opening_sign_up_lets_everyone_waiting_in_and_closing_it_puts_nobody_back() {
    let hub = hub_with(None, early_access).await;
    let mut early = hub.first_connect("Claude").await;
    assert_eq!(whoami(&hub, &mut early).await["access"], "waiting");
    let refusal = hub.call(&mut early, "machines", json!({})).await;
    assert!(refused(&refusal), "{refusal}");

    // site.json says open now: the next call lets it in, as open sign-up.
    serve_site(&hub, "open", |_| {});
    let who = whoami(&hub, &mut early).await;
    assert_eq!(who["access"], "admitted", "{who}");
    let machines = hub.call(&mut early, "machines", json!({})).await;
    assert!(!refused(&machines), "{machines}");
    let tenant = who["tenant"].as_str().unwrap();
    let chain = hub.accounts.events(tenant, 20).await.unwrap();
    assert!(
        chain
            .iter()
            .any(|e| e.event == "tenant.admitted" && e.detail == "open sign-up"),
        "{chain:?}"
    );
    let home = hub.accounts.home_tenant().await.unwrap();
    assert!(
        hub.accounts
            .events(&home, 20)
            .await
            .unwrap()
            .iter()
            .any(|e| e.event == "tenant.admitted" && e.detail.starts_with(tenant))
    );
    let mut open = hub.first_connect("Codex").await;
    assert_eq!(whoami(&hub, &mut open).await["access"], "admitted");

    // Early access again: nobody already in waits again; someone new does.
    serve_site(&hub, "early", early_access);
    assert_eq!(whoami(&hub, &mut early).await["access"], "admitted");
    assert_eq!(whoami(&hub, &mut open).await["access"], "admitted");
    let mut late = hub.first_connect("Gemini").await;
    assert_eq!(whoami(&hub, &mut late).await["access"], "waiting");
}

#[tokio::test]
async fn a_plans_limits_hold_at_the_boundary_and_name_the_plan() {
    for store in stores().await {
        limits(hub_with(store.as_deref(), plans).await).await;
    }
}

async fn limits(hub: Hosted) {
    let mut person = hub.first_connect("Claude").await;
    let who = whoami(&hub, &mut person).await;
    assert_eq!(
        (who["access"].as_str(), who["plan"].as_str()),
        (Some("admitted"), Some("free"))
    );
    let tenant = who["tenant"].as_str().unwrap().to_string();
    let plans = format!("{}/#plans", hub.url);
    let limited = |answer: &Value, words: &[&str]| {
        assert_eq!(
            answer["structuredContent"]["code"], "plan_limit",
            "{answer}"
        );
        let message = answer["structuredContent"]["message"].as_str().unwrap();
        for word in words.iter().chain([&plans.as_str()]) {
            assert!(message.contains(word), "{word:?} in {message}");
        }
    };

    // Free: two machines, one project. The second of each is the boundary.
    for name in ["one", "two"] {
        let added = hub
            .call(&mut person, "machine_add", json!({ "name": name }))
            .await;
        assert!(!refused(&added), "{added}");
    }
    let third = hub
        .call(&mut person, "machine_add", json!({ "name": "three" }))
        .await;
    limited(&third, &["Free", "2 machines"]);
    let api = hub
        .http
        .post(format!("{}/api/v1/machines", hub.url))
        .bearer_auth(&person.token)
        .json(&json!({ "name": "three" }))
        .send()
        .await
        .unwrap();
    assert_eq!(api.status(), 403);
    assert_eq!(api.json::<Value>().await.unwrap()["error"], "plan_limit");
    let bound = hub
        .call(&mut person, "board_bind", json!({ "project": "first" }))
        .await;
    assert!(!refused(&bound), "{bound}");
    let again = hub
        .call(&mut person, "board_bind", json!({ "project": "first" }))
        .await;
    assert!(
        !refused(&again),
        "binding the same one again is no new project: {again}"
    );
    let second = hub
        .call(&mut person, "board_bind", json!({ "project": "second" }))
        .await;
    limited(&second, &["Free", "1 project"]);

    // Pro: projects unlimited, three machines.
    hub.accounts
        .set_plan(&tenant, "pro", "acct_operator")
        .await
        .unwrap();
    assert_eq!(whoami(&hub, &mut person).await["plan"], "pro");
    let second = hub
        .call(&mut person, "board_bind", json!({ "project": "second" }))
        .await;
    assert!(!refused(&second), "{second}");
    let third = hub
        .call(&mut person, "machine_add", json!({ "name": "three" }))
        .await;
    assert!(!refused(&third), "{third}");
    let fourth = hub
        .call(&mut person, "machine_add", json!({ "name": "four" }))
        .await;
    limited(&fourth, &["Pro", "3 machines"]);

    // A plan site.json no longer lists is the default plan.
    sqlx::query("UPDATE tenancy_access SET plan = 'gone' WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(hub.accounts.store().admin())
        .await
        .unwrap();
    assert_eq!(whoami(&hub, &mut person).await["plan"], "free");
    let refused_now = hub
        .call(&mut person, "board_bind", json!({ "project": "third" }))
        .await;
    limited(&refused_now, &["Free"]);
}

#[tokio::test]
async fn a_tenant_let_in_hears_so_by_email_when_it_has_a_verified_address() {
    let mail = crate::mail::stub::start("hub").await;
    let mail_url = mail.url.clone();
    let hub = hosted_with(None, |dir, _| {
        let site = site_dir(dir, "site", early_access);
        let password = dir.join("smtp-password");
        std::fs::write(&password, "mail-pass\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        json!({
            "site": { "dir": site.to_string_lossy() },
            "auth": { "email": { "smtp_url": mail_url, "password_file": password.to_string_lossy(),
                                 "from": "Hub <hub@hub.test>" } }
        })
    })
    .await;
    let mailer = web::Providers::from_config(&hub.config)
        .unwrap()
        .email
        .expect("a mail server");
    hub.accounts
        .on_admitted(web::announce_admissions(mailer, hub.config.clone()));
    let mut with = hub.first_connect("Claude").await;
    let mut without = hub.first_connect("Codex").await;
    let who = whoami(&hub, &mut with).await;
    hub.accounts
        .link_identity(
            who["subject"].as_str().unwrap(),
            &crate::accounts::identities::Proven {
                provider: "github".into(),
                subject: "77".into(),
                verified_email: Some("sam@example.org".into()),
                name: None,
            },
        )
        .await
        .unwrap();
    let tenants = [
        who["tenant"].as_str().unwrap().to_string(),
        whoami(&hub, &mut without).await["tenant"]
            .as_str()
            .unwrap()
            .to_string(),
    ];
    hub.accounts
        .admit(&tenants, None, "acct_operator")
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while mail.received.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let received = mail.received.lock().unwrap().clone();
    assert_eq!(
        received.len(),
        1,
        "one email, to the one address: {received:?}"
    );
    assert_eq!(received[0].to, "sam@example.org");
    assert!(
        received[0].data.contains("You're in")
            && received[0].data.contains(&format!("{}/account", hub.url)),
        "{}",
        received[0].data
    );
}
