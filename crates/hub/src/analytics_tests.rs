//! First-party analytics on a hosted hub end to end over HTTP, the whole router as it serves
//! (`analytics.rs`): a visit to a public page is counted by the server with exactly what
//! it keeps; opted-out, robot, asset, health, API and the operator's own requests are not;
//! each tenant's milestones are written once, where each happens; a hub that does not
//! count keeps nothing and its privacy page says so. On SQLite, and on Postgres when the
//! test server is set (CI).

use serde_json::{Value, json};
use sqlx::Row;

use super::*;
use crate::{
    accounts::identities::Proven,
    tenant_tests::{Hosted, hosted_with, refused},
};

const PHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
const DESKTOP: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

async fn stores() -> Vec<Option<String>> {
    let mut out = vec![None];
    if let Some(url) = store::testing::empty_postgres().await {
        out.push(Some(url));
    }
    out
}

/// A hosted hub run by `github:42`, with `more` merged into its config.
async fn hub_on(store: Option<&str>, more: Value) -> Hosted {
    hosted_with(store, move |_, _| {
        let mut config = json!({ "operators": ["github:42"] });
        for (key, value) in more.as_object().unwrap() {
            config[key] = value.clone();
        }
        config
    })
    .await
}

/// Every counted visit, as its columns.
async fn views(hub: &Hosted) -> Vec<Vec<String>> {
    hub.hub.analytics.as_ref().unwrap().flush().await;
    sqlx::query(
        "SELECT CAST(day AS TEXT), CAST(hour AS TEXT), path, utm_source, utm_medium, utm_campaign,
           referrer, device, visitor FROM hub_analytics_views ORDER BY path, visitor",
    )
    .fetch_all(hub.accounts.store().admin())
    .await
    .unwrap()
    .iter()
    .map(|row| (0..9).map(|i| row.get::<String, _>(i)).collect())
    .collect()
}

/// `tenant`'s milestones, as `kind:detail`.
async fn milestones(hub: &Hosted, tenant: &str) -> Vec<String> {
    if let Some(analytics) = &hub.hub.analytics {
        analytics.flush().await;
    }
    let mut kept: Vec<String> =
        sqlx::query("SELECT kind, detail FROM hub_analytics_events WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_all(hub.accounts.store().admin())
            .await
            .unwrap()
            .iter()
            .map(|row| format!("{}:{}", row.get::<String, _>(0), row.get::<String, _>(1)))
            .collect();
    kept.sort();
    kept
}

async fn visit(hub: &Hosted, path: &str, headers: &[(&str, &str)]) -> u16 {
    let mut request = hub.http.get(format!("{}{path}", hub.url));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.unwrap().status().as_u16()
}

/// The operator signed in, as their browser's cookie.
async fn operator_cookie(hub: &Hosted) -> String {
    let operator = hub
        .accounts
        .sign_in_identity(&Proven {
            provider: "github".into(),
            subject: "42".into(),
            verified_email: None,
            name: Some("Operator".into()),
        })
        .await
        .unwrap();
    hub.config.learn_owner(&operator.id).unwrap();
    let token = hub.accounts.open_session(&operator.id, None).await.unwrap();
    format!("dg_session={token}")
}

#[tokio::test]
async fn a_public_page_visit_is_counted_by_the_server_with_exactly_what_it_keeps() {
    for store in stores().await {
        counted(hub_on(store.as_deref(), json!({})).await).await;
    }
}

async fn counted(hub: Hosted) {
    let status = visit(
        &hub,
        "/docs?utm_source=newsletter&utm_medium=email&utm_campaign=launch&token=secret",
        &[
            ("user-agent", PHONE),
            ("referer", "https://news.example.net/story?id=7"),
            // Not a trusted proxy's: the address that connected is the client's.
            ("x-forwarded-for", "198.51.100.9"),
        ],
    )
    .await;
    assert_eq!(status, 200);
    let kept = views(&hub).await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    let row = &kept[0];
    let now = devices::now_secs();
    let day = analytics::day_of(now);
    assert_eq!(row[0], day.to_string());
    let hour: i64 = row[1].parse().unwrap();
    assert!((0..24).contains(&hour));
    assert_eq!(
        row[2..8],
        [
            "/docs",
            "newsletter",
            "email",
            "launch",
            "news.example.net",
            "mobile"
        ]
    );
    let visitor = &row[8];
    assert!(
        visitor.len() == 32 && visitor.bytes().all(|b| b.is_ascii_hexdigit()),
        "{visitor}"
    );
    // The id is the day's HMAC of the address that connected and the User-Agent: neither is
    // anywhere in what was kept.
    let analytics = hub.hub.analytics.clone().unwrap();
    assert_eq!(
        &analytics.visitor(day, "127.0.0.1".parse().unwrap(), PHONE),
        visitor
    );
    for row in &kept {
        for column in row {
            for never in [
                "127.0.0.1",
                "198.51.100.9",
                "iPhone",
                "Mozilla",
                "secret",
                "token",
            ] {
                assert!(!column.contains(never), "{never} in {row:?}");
            }
        }
    }
    // The same person on another page: another visit, the same visitor today.
    assert_eq!(visit(&hub, "/privacy", &[("user-agent", PHONE)]).await, 200);
    let kept = views(&hub).await;
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[1][2], "/privacy");
    assert_eq!(&kept[1][8], visitor);
    assert_eq!(kept[1][3..7], ["", "", "", ""], "no link, no referrer");

    // Not counted: opted out, robots, no User-Agent, anything but a public page, a page that
    // was not served, and the operator's own visits.
    let desktop = ("user-agent", DESKTOP);
    for (path, headers) in [
        ("/", vec![desktop, ("sec-gpc", "1")]),
        ("/docs", vec![desktop, ("dnt", "1")]),
        (
            "/",
            vec![("user-agent", "Mozilla/5.0 (compatible; Googlebot/2.1)")],
        ),
        ("/terms", vec![("user-agent", "curl/8.5.0")]),
        ("/support", vec![]),
        ("/assets/hub.css", vec![desktop]),
        ("/assets/logo.svg", vec![desktop]),
        ("/healthz", vec![desktop]),
        ("/api/v1/me", vec![desktop]),
        ("/.well-known/oauth-protected-resource", vec![desktop]),
        ("/account", vec![desktop]),
        ("/operator", vec![desktop]),
        ("/nothing-here", vec![desktop]),
    ] {
        visit(&hub, path, &headers).await;
    }
    let operator = operator_cookie(&hub).await;
    for page in ["/docs", "/privacy", "/terms", "/support"] {
        let status = visit(&hub, page, &[desktop, ("cookie", &operator)]).await;
        assert_eq!(status, 200, "{page}");
    }
    assert_eq!(views(&hub).await.len(), 2, "nothing more was counted");
    // Someone signed in who is not the operator is counted like anyone.
    let person = hub.first_connect("Claude").await;
    assert_eq!(
        visit(&hub, "/docs", &[desktop, ("cookie", &person.cookie)]).await,
        200
    );
    assert_eq!(views(&hub).await.len(), 3);

    // The privacy page says what this hub counts, for as long as its config keeps it.
    let (status, privacy) = Hosted::text(hub.get("/privacy", None).await).await;
    assert_eq!(status, 200);
    for said in [
        "no cookie",
        "changes every day",
        "Global Privacy Control",
        "90 days",
    ] {
        assert!(privacy.contains(said), "{said}: {privacy}");
    }
    assert!(!privacy.contains("no analytics"), "{privacy}");
}

#[tokio::test]
async fn a_hub_that_does_not_count_keeps_nothing_and_says_so() {
    let hub = hub_on(None, json!({ "analytics": { "enabled": false } })).await;
    assert!(hub.hub.analytics.is_none());
    assert_eq!(visit(&hub, "/docs", &[("user-agent", DESKTOP)]).await, 200);
    let mut person = hub.first_connect("Claude").await;
    let who = hub.call(&mut person, "whoami", json!({})).await;
    let tenant = who["structuredContent"]["tenant"]
        .as_str()
        .unwrap()
        .to_string();
    for table in ["hub_analytics_views", "hub_analytics_events"] {
        let kept: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(hub.accounts.store().admin())
                .await
                .unwrap();
        assert_eq!(kept, 0, "{table}");
    }
    assert!(milestones(&hub, &tenant).await.is_empty());
    let (_, privacy) = Hosted::text(hub.get("/privacy", None).await).await;
    assert!(privacy.contains("no analytics"), "{privacy}");
    assert!(!privacy.contains("Global Privacy Control"), "{privacy}");
    // And a longer retention is what the page says, where the hub counts.
    let kept = hub_on(None, json!({ "analytics": { "retention_days": 30 } })).await;
    let (_, privacy) = Hosted::text(kept.get("/privacy", None).await).await;
    assert!(privacy.contains("30 days"), "{privacy}");
}

#[tokio::test]
async fn each_milestone_is_written_once_where_it_happens() {
    for store in stores().await {
        let hub = hosted_with(store.as_deref(), |dir, _| {
            let mut site: Value =
                serde_json::from_str(include_str!("../site/default/site.json")).unwrap();
            site["early_access"] = json!({ "mode": "early_access" });
            let path = dir.join("site");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("site.json"), site.to_string()).unwrap();
            json!({ "site": { "dir": path.to_string_lossy() }, "operators": ["github:42"] })
        })
        .await;
        milestones_once(hub).await;
    }
}

async fn milestones_once(hub: Hosted) {
    // Made on first connect by Claude, and waiting.
    let mut person = hub.first_connect("Claude Desktop").await;
    let who = hub.call(&mut person, "whoami", json!({})).await["structuredContent"].clone();
    let tenant = who["tenant"].as_str().unwrap().to_string();
    let subject = who["subject"].as_str().unwrap().to_string();
    assert_eq!(milestones(&hub, &tenant).await, ["made:claude", "waiting:"]);

    // Secured with GitHub; a second sign-in linked later changes nothing.
    let github = Proven {
        provider: "github".into(),
        subject: "7".into(),
        verified_email: None,
        name: None,
    };
    hub.accounts.link_identity(&subject, &github).await.unwrap();
    let google = Proven {
        provider: "google".into(),
        subject: "g-7".into(),
        ..github.clone()
    };
    hub.accounts.link_identity(&subject, &google).await.unwrap();
    assert_eq!(
        milestones(&hub, &tenant).await,
        ["made:claude", "secured:github", "waiting:"]
    );

    // Let in by the operator, once.
    hub.accounts
        .admit(std::slice::from_ref(&tenant), None, "acct_operator")
        .await
        .unwrap();
    hub.accounts
        .admit(std::slice::from_ref(&tenant), None, "acct_operator")
        .await
        .unwrap();

    // Its first machine, by a code of its own; a second one adds nothing.
    for (n, name) in ["laptop", "desk"].into_iter().enumerate() {
        let stored = format!("{}-{name}", accounts::tenants::tag(&tenant));
        let code = hub
            .devices
            .issue_owned_code(&subject, &stored, [n as u8 + 1; 16], 16)
            .unwrap();
        let enrolled = hub
            .http
            .post(format!("{}/enroll", hub.url))
            .json(&json!({ "name": stored, "code": code, "public_key": format!("{name}-key") }))
            .send()
            .await
            .unwrap();
        assert_eq!(enrolled.status(), 201);
    }

    // Its first board; binding it again adds nothing.
    for project in ["first", "first"] {
        let bound = hub
            .call(&mut person, "board_bind", json!({ "project": project }))
            .await;
        assert!(!refused(&bound), "{bound}");
    }

    // Its first operation: only one that started counts, once, its latest kept.
    let account = hub.accounts.get(&subject).await.unwrap().unwrap();
    let caller = hub.accounts.subject(&account, "oauth");
    assert!(
        hub.hub
            .record(&caller, "operation_start", "laptop", "", "broker_offline")
            .is_ok()
    );
    assert!(
        !milestones(&hub, &tenant)
            .await
            .iter()
            .any(|m| m.starts_with("operation"))
    );
    for (tool, outcome) in [("operation_start", "ok"), ("run", "running"), ("run", "ok")] {
        assert!(
            hub.hub
                .record(&caller, tool, "laptop", "op-1", outcome)
                .is_ok()
        );
    }
    assert_eq!(
        milestones(&hub, &tenant).await,
        [
            "admitted:",
            "board:",
            "machine:",
            "made:claude",
            "operation:",
            "secured:github",
            "waiting:"
        ]
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM hub_analytics_events WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(hub.accounts.store().admin())
            .await
            .unwrap();
    assert_eq!(rows, 7, "one row for each, never two");

    // A person who signs up on the web with a sign-in: made by no AI, secured at once.
    let web = hub
        .accounts
        .sign_in_identity(&Proven {
            provider: "google".into(),
            subject: "g-new".into(),
            verified_email: Some("new@example.net".into()),
            name: Some("New".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        milestones(&hub, &web.tenant).await,
        ["made:other", "secured:google", "waiting:"]
    );

    // The operator's own tenant never has any, whatever it does.
    let operator = operator_cookie(&hub).await;
    assert!(!operator.is_empty());
    let home = hub.accounts.home_tenant().await.unwrap();
    let owner = hub.accounts.owner().await.unwrap().unwrap();
    let owner = hub.accounts.subject(&owner, "oauth");
    assert!(
        hub.hub
            .record(&owner, "run", "laptop", "op-2", "ok")
            .is_ok()
    );
    assert!(milestones(&hub, &home).await.is_empty());
}
