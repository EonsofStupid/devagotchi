//! The operator page (`/operator`) on a hosted hub, the whole router as it serves: only the
//! operator sees it (anyone else gets an unknown path's 404), it lists who waits and who is
//! in, lets people in on a plan with one press and changes plans, takes posts from this hub
//! only, and a person let in has their next MCP call work. On SQLite, and on Postgres when
//! the test server is set (CI).

use std::path::Path;

use serde_json::{Value, json};

use crate::{
    accounts::identities::Proven,
    site::tests::assert_no_brand,
    store,
    tenant_tests::{Hosted, hosted_with, refused},
};

/// A site directory under `dir`: the built-in site, or `base`'s, with `edit` applied to its
/// site.json.
fn site_dir(dir: &Path, base: Option<&Path>, edit: impl FnOnce(&mut Value)) -> std::path::PathBuf {
    let site = dir.join("site");
    std::fs::create_dir_all(&site).unwrap();
    let mut value: Value = match base {
        Some(base) => {
            copy(base, &site);
            serde_json::from_str(&std::fs::read_to_string(base.join("site.json")).unwrap()).unwrap()
        }
        None => serde_json::from_str(include_str!("../../site/default/site.json")).unwrap(),
    };
    edit(&mut value);
    std::fs::write(site.join("site.json"), value.to_string()).unwrap();
    site
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn early_access_with_plans(value: &mut Value) {
    value["early_access"] = json!({ "mode": "early_access" });
    value["plans"] = json!([
        { "id": "free", "name": "Free", "price": "Free", "default": true,
          "limits": { "projects": 1, "machines": 2 } },
        { "id": "pro", "name": "Pro", "price": "Ask",
          "limits": { "projects": "unlimited", "machines": 5, "clients": 4 } }
    ]);
}

pub(super) async fn hub_on(store: Option<&str>, base: Option<&'static Path>) -> Hosted {
    hosted_with(store, move |dir, _| {
        let site = site_dir(dir, base, early_access_with_plans);
        json!({
            "site": { "dir": site.to_string_lossy() },
            "operators": ["github:42"]
        })
    })
    .await
}

/// The operator signed in, as their browser's cookie.
pub(super) async fn operator_cookie(hub: &Hosted) -> String {
    let operator = hub
        .accounts
        .sign_in_identity(&Proven {
            provider: "github".into(),
            subject: "42".into(),
            verified_email: None,
            name: Some("Jesse".into()),
        })
        .await
        .unwrap();
    assert!(operator.owner);
    hub.config.learn_owner(&operator.id).unwrap();
    let token = hub.accounts.open_session(&operator.id, None).await.unwrap();
    format!("dg_session={token}")
}

async fn page(hub: &Hosted, cookie: Option<&str>) -> (u16, String) {
    Hosted::text(hub.get("/operator", cookie).await).await
}

#[tokio::test]
async fn the_operator_lets_a_waiting_person_in_and_their_next_call_works() {
    let mut stores = vec![None];
    if let Some(url) = store::testing::empty_postgres().await {
        stores.push(Some(url));
    }
    for store in stores {
        operator_page(hub_on(store.as_deref(), None).await).await;
    }
}

async fn operator_page(hub: Hosted) {
    let mut person = hub.first_connect("Claude").await;
    let who = hub.call(&mut person, "whoami", json!({})).await["structuredContent"].clone();
    assert_eq!(who["access"], "waiting");
    let tenant = who["tenant"].as_str().unwrap().to_string();
    let waiting = hub.call(&mut person, "machines", json!({})).await;
    assert!(refused(&waiting), "{waiting}");

    // Nobody but the operator finds the page: the same answer as a path that is not there.
    let (missing_status, missing) = Hosted::text(hub.get("/no-such-page", None).await).await;
    assert_eq!(missing_status, 404);
    for cookie in [None, Some(person.cookie.as_str())] {
        assert_eq!(
            page(&hub, cookie).await,
            (404, missing.clone()),
            "{cookie:?}"
        );
        let (status, body) = Hosted::text(
            hub.form(
                "/operator/admit",
                cookie,
                &[
                    ("tenant", tenant.as_str()),
                    (&format!("plan.{tenant}"), "pro"),
                ],
            )
            .await,
        )
        .await;
        assert_eq!((status, body), (404, missing.clone()));
        let (status, _) = Hosted::text(
            hub.form(
                "/operator/plan",
                cookie,
                &[("tenant", tenant.as_str()), ("plan", "pro")],
            )
            .await,
        )
        .await;
        assert_eq!(status, 404);
    }
    let (_, account) = Hosted::text(hub.get("/account", Some(&person.cookie)).await).await;
    assert!(!account.contains("/operator"), "{account}");

    // The operator's page: the mode and how to change it, and the person waiting.
    let operator = operator_cookie(&hub).await;
    let (_, account) = Hosted::text(hub.get("/account", Some(&operator)).await).await;
    assert!(account.contains("href=\"/operator\""), "{account}");
    let (status, html) = page(&hub, Some(&operator)).await;
    assert_eq!(status, 200, "{html}");
    for expected in [
        "<h1>Operator</h1>",
        "Sign-up is <strong>early access</strong>",
        "<code>hub-up.sh --site DIR --site-only</code>",
        "Waiting to be let in (1)",
        &format!("<code>{tenant}</code>"),
        &format!("name=\"tenant\" value=\"{tenant}\""),
        &format!("aria-label=\"Let in {tenant}\""),
        &format!("name=\"plan.{tenant}\""),
        "<option value=\"free\" selected>Free</option><option value=\"pro\">Pro</option>",
        "Not secured",
        "<td>Claude</td>",
        "None yet",
        "action=\"/operator/admit\"",
        "Nobody has been let in yet.",
    ] {
        assert!(html.contains(expected), "{expected:?} in {html}");
    }
    assert!(!html.contains("<script"), "no script: {html}");

    // A post from another site changes nothing.
    let foreign = hub
        .http
        .post(format!("{}/operator/admit", hub.url))
        .header(axum::http::header::ORIGIN, "https://elsewhere.example")
        .header(axum::http::header::COOKIE, &operator)
        .form(&[("tenant", tenant.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    assert!(
        hub.accounts
            .access_of(&tenant)
            .await
            .unwrap()
            .unwrap()
            .waiting
    );
    // Nobody checked: nothing happens, and the page says why.
    let none = hub.form("/operator/admit", Some(&operator), &[]).await;
    assert_eq!(none.status(), 303);
    assert_eq!(none.headers()["location"], "/operator?done=none#waiting");

    // Checked, on the Pro plan: one press lets them in.
    let plan_field = format!("plan.{tenant}");
    let admitted = hub
        .form(
            "/operator/admit",
            Some(&operator),
            &[("tenant", tenant.as_str()), (&plan_field, "pro")],
        )
        .await;
    assert_eq!(admitted.status(), 303);
    assert_eq!(
        admitted.headers()["location"],
        "/operator?done=admitted&n=1#waiting"
    );
    let machines = hub.call(&mut person, "machines", json!({})).await;
    assert!(!refused(&machines), "their next call works: {machines}");
    let who = hub.call(&mut person, "whoami", json!({})).await["structuredContent"].clone();
    assert_eq!(
        (who["access"].as_str(), who["plan"].as_str()),
        (Some("admitted"), Some("pro"))
    );
    let (_, html) = Hosted::text(
        hub.get("/operator?done=admitted&n=1", Some(&operator))
            .await,
    )
    .await;
    for expected in [
        "Let in: 1.",
        "Nobody is waiting.",
        "Let in (1)",
        "<option value=\"pro\" selected>Pro</option>",
        "action=\"/operator/plan\"",
        "0 of 5 machines",
        "0 projects, no limit",
        "1 of 4 AI apps at a time",
        ", by acct_",
    ] {
        assert!(html.contains(expected), "{expected:?} in {html}");
    }

    // A plan change, from the row.
    let changed = hub
        .form(
            "/operator/plan",
            Some(&operator),
            &[("tenant", tenant.as_str()), ("plan", "free")],
        )
        .await;
    assert_eq!(
        changed.headers()["location"],
        "/operator?done=plan#admitted"
    );
    let who = hub.call(&mut person, "whoami", json!({})).await["structuredContent"].clone();
    assert_eq!(who["plan"], "free");
    let (_, html) = Hosted::text(hub.get("/operator?done=plan", Some(&operator)).await).await;
    assert!(
        html.contains("Plan changed.") && html.contains("0 of 2 machines"),
        "{html}"
    );
    let (status, html) = Hosted::text(
        hub.form(
            "/operator/plan",
            Some(&operator),
            &[("tenant", tenant.as_str()), ("plan", "gold")],
        )
        .await,
    )
    .await;
    assert_eq!(status, 400);
    assert!(html.contains("no plan"), "{html}");

    // On the record, the operator as actor, in the person's chain and the home tenant's.
    let home = hub.accounts.home_tenant().await.unwrap();
    for chain in [&tenant, &home] {
        let events = hub.accounts.events(chain, 50).await.unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event == "tenant.admitted" && e.actor.starts_with("acct_"))
        );
        assert!(events.iter().any(|e| e.event == "tenant.plan"));
    }
}

#[tokio::test]
async fn a_neutral_sites_operator_page_names_only_that_site() {
    static NEUTRAL: std::sync::LazyLock<std::path::PathBuf> =
        std::sync::LazyLock::new(crate::site::tests::neutral_dir);
    let hub = hub_on(None, Some(NEUTRAL.as_path())).await;
    // A person waiting, made as a first connect makes them.
    hub.accounts
        .start_unsecured(std::net::IpAddr::from([198, 51, 100, 9]), "Claude")
        .await
        .unwrap();
    let operator = operator_cookie(&hub).await;
    let (status, html) = page(&hub, Some(&operator)).await;
    assert_eq!(status, 200);
    assert!(html.contains(" · Lumen Relay</title>"), "{html}");
    assert_no_brand("/operator", &html);
}
