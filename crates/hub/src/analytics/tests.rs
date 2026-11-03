//! What a visit keeps and what is never counted, the day's visitor key, the daily totals and
//! retention, and the tables on both engines. The whole router's half is
//! `crate::analytics_tests`.

use std::net::{IpAddr, Ipv4Addr};

use axum::http::{HeaderMap, HeaderValue, Method, Uri};
use sqlx::Row;

use super::{
    Analytics, DAY, View, day_of,
    funnel::{self, FAMILIES, KINDS},
    visit::{self, DEVICES, PAGES, Skip, UTM},
    writer,
};
use crate::{config::AnalyticsConfig, store::testing};

const PHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
const DESKTOP: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(*name, HeaderValue::from_str(value).unwrap());
    }
    map
}

#[test]
fn a_visit_keeps_the_page_the_links_utm_the_referring_host_and_the_device_only() {
    let uri: Uri = "/docs?utm_source=news%20letter&utm_medium=email&utm_campaign=launch+day&ref=secret&token=abc"
        .parse()
        .unwrap();
    let asked = headers(&[
        ("user-agent", PHONE),
        (
            "referer",
            "https://User:pw@News.Example.NET:8443/story?id=7#top",
        ),
    ]);
    let seen = visit::seen(&Method::GET, &uri, &asked, "hub.example.org").unwrap();
    assert_eq!(seen.path, "/docs");
    assert_eq!(seen.utm, ["news letter", "email", "launch day"]);
    assert_eq!(seen.referrer, "news.example.net");
    assert_eq!(seen.device, "mobile");
    assert_eq!(seen.agent, PHONE, "only to make the day's visitor id");
    // This hub's own pages are no referrer; nor is anything but an http(s) URL.
    for referer in [
        "https://hub.example.org/privacy",
        "HTTPS://HUB.EXAMPLE.ORG:443/",
        "android-app://com.example",
        "not a url",
        "https://bad host/",
    ] {
        let asked = headers(&[("user-agent", DESKTOP), ("referer", referer)]);
        let seen = visit::seen(&Method::GET, &uri, &asked, "hub.example.org").unwrap();
        assert_eq!(seen.referrer, "", "{referer}");
    }
    // Long and odd utm values are cut and cleaned; other parameters never reach it.
    let long = format!("/?utm_source={}%0A%7Cx&utm_campaign=", "a".repeat(300));
    let desktop = headers(&[("user-agent", DESKTOP)]);
    let seen = visit::seen(
        &Method::GET,
        &long.parse().unwrap(),
        &desktop,
        "hub.example.org",
    )
    .unwrap();
    assert_eq!(seen.utm[0], "a".repeat(100));
    assert_eq!(seen.utm[1..], ["", ""]);
    assert_eq!(UTM, ["utm_source", "utm_medium", "utm_campaign"]);
    // Every public page counts; its path is kept as the page, never as asked.
    for page in PAGES {
        let seen = visit::seen(&Method::GET, &page.parse().unwrap(), &desktop, "").unwrap();
        assert_eq!(seen.path, page);
    }
}

#[test]
fn opted_out_robots_and_anything_but_a_public_page_are_never_counted() {
    let page: Uri = "/".parse().unwrap();
    let skip = |method: &Method, uri: &Uri, pairs: &[(&'static str, &str)]| {
        visit::seen(method, uri, &headers(pairs), "hub.example.org").unwrap_err()
    };
    assert_eq!(
        skip(
            &Method::GET,
            &page,
            &[("user-agent", DESKTOP), ("sec-gpc", "1")]
        ),
        Skip::OptedOut
    );
    assert_eq!(
        skip(
            &Method::GET,
            &page,
            &[("user-agent", DESKTOP), ("dnt", "1")]
        ),
        Skip::OptedOut
    );
    assert_eq!(skip(&Method::GET, &page, &[]), Skip::Robot, "no User-Agent");
    assert_eq!(
        skip(&Method::GET, &page, &[("user-agent", " ")]),
        Skip::Robot
    );
    for robot in [
        "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
        "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2)",
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 HeadlessChrome/141.0.0.0 Safari/537.36",
        "curl/8.5.0",
        "python-requests/2.32",
        "facebookexternalhit/1.1",
        "Mozilla/5.0 (compatible; UptimeRobot/2.0)",
        "Mozilla/5.0 (compatible; ClaudeBot/1.0; +claudebot@anthropic.com)",
    ] {
        assert!(visit::robot(robot), "{robot}");
        assert_eq!(
            skip(&Method::GET, &page, &[("user-agent", robot)]),
            Skip::Robot,
            "{robot}"
        );
    }
    for person in [PHONE, DESKTOP] {
        assert!(!visit::robot(person), "{person}");
    }
    for path in [
        "/assets/hub.css",
        "/assets/logo.svg",
        "/healthz",
        "/api/v1/me",
        "/mcp",
        "/account",
        "/operator",
        "/oauth/authorize",
        "/docs/",
        "/nope",
    ] {
        assert_eq!(
            skip(
                &Method::GET,
                &path.parse().unwrap(),
                &[("user-agent", DESKTOP)]
            ),
            Skip::NotAPage,
            "{path}"
        );
    }
    for method in [Method::HEAD, Method::POST] {
        assert_eq!(
            skip(&method, &page, &[("user-agent", DESKTOP)]),
            Skip::NotAPage
        );
    }
}

#[test]
fn a_phone_a_tablet_and_a_computer_are_told_apart() {
    for (agent, device) in [
        (PHONE, "mobile"),
        (
            "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 Chrome/141.0 Mobile Safari/537.36",
            "mobile",
        ),
        (
            "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148",
            "tablet",
        ),
        (
            "Mozilla/5.0 (Linux; Android 14; SM-X710) AppleWebKit/537.36 Chrome/141.0 Safari/537.36",
            "tablet",
        ),
        (DESKTOP, "desktop"),
        (
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_0) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15",
            "desktop",
        ),
    ] {
        assert_eq!(visit::device(agent), device, "{agent}");
        assert!(DEVICES.contains(&device));
    }
}

#[test]
fn the_ai_client_family_comes_from_its_oauth_client_name() {
    for (name, family) in [
        ("Claude", "claude"),
        ("claude.ai", "claude"),
        ("Claude Code", "claude"),
        ("ChatGPT", "chatgpt"),
        ("OpenAI Codex", "chatgpt"),
        ("codex-cli", "chatgpt"),
        ("Grok", "grok"),
        ("Cursor", "cursor"),
        ("a terminal", "other"),
        ("", "other"),
    ] {
        assert_eq!(funnel::client_family(name), family, "{name}");
        assert!(FAMILIES.contains(&family));
    }
    assert_eq!(KINDS.len(), 7);
}

#[tokio::test]
async fn the_visitor_key_lives_one_day_so_a_visitor_on_two_days_has_two_ids() {
    let store = testing::sqlite().await;
    let analytics = Analytics::start(
        store.store.clone(),
        AnalyticsConfig::default(),
        "https://hub.example.org",
    );
    let monday = day_of(1_762_128_000);
    let address = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
    let first = analytics.visitor(monday, address, PHONE);
    assert_eq!(first.len(), 32);
    assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(
        analytics.visitor(monday, address, PHONE),
        first,
        "one id all day"
    );
    // An IPv4 address seen as IPv6-mapped is the same visitor.
    let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
    assert_eq!(analytics.visitor(monday, mapped, PHONE), first);
    assert_ne!(
        analytics.visitor(monday, address, DESKTOP),
        first,
        "another browser"
    );
    let tuesday = analytics.visitor(monday + DAY, address, PHONE);
    assert_ne!(tuesday, first, "another day, another id");
    // Monday's key is gone with Monday: asked again, it is a new key, so nothing made on
    // one day can be matched to another.
    assert_ne!(analytics.visitor(monday, address, PHONE), first);
    assert_ne!(analytics.visitor(monday, address, PHONE), tuesday);
    // Nothing about it reaches the store.
    analytics.flush().await;
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hub_analytics_views")
        .fetch_one(store.store.pool())
        .await
        .unwrap();
    assert_eq!(kept, 0);
}

fn view(day: i64, path: &'static str, device: &'static str, visitor: &str) -> View {
    View {
        day,
        hour: 9,
        path,
        utm: ["news".into(), "email".into(), "launch".into()],
        referrer: "news.example.net".into(),
        device,
        visitor: visitor.into(),
    }
}

/// The daily totals of `day` on `store`, as `metric/item=views/visitors`, sorted.
async fn daily(store: &crate::store::Store, day: i64) -> Vec<String> {
    let mut rows: Vec<String> =
        sqlx::query("SELECT metric, item, views, visitors FROM hub_analytics_daily WHERE day = $1")
            .bind(day)
            .fetch_all(store.pool())
            .await
            .unwrap()
            .iter()
            .map(|row| {
                format!(
                    "{}/{}={}/{}",
                    row.get::<String, _>(0),
                    row.get::<String, _>(1),
                    row.get::<i64, _>(2),
                    row.get::<i64, _>(3)
                )
            })
            .collect();
    rows.sort();
    rows
}

async fn raw_days(store: &crate::store::Store) -> Vec<i64> {
    let mut days: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT day FROM hub_analytics_views")
        .fetch_all(store.pool())
        .await
        .unwrap();
    days.sort();
    days
}

/// A raw visit, as the writer writes one, straight into the store.
async fn insert(store: &crate::store::Store, view: View) {
    sqlx::query(
        "INSERT INTO hub_analytics_views
           (day, hour, path, utm_source, utm_medium, utm_campaign, referrer, device, visitor)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(view.day)
    .bind(view.hour)
    .bind(view.path)
    .bind(&view.utm[0])
    .bind(&view.utm[1])
    .bind(&view.utm[2])
    .bind(&view.referrer)
    .bind(view.device)
    .bind(&view.visitor)
    .execute(store.pool())
    .await
    .unwrap();
}

/// Noon today, and today.
fn noon() -> (u64, i64) {
    let today = day_of(crate::devices::now_secs());
    (u64::try_from(today + DAY / 2).unwrap(), today)
}

#[tokio::test]
async fn finished_days_roll_into_daily_totals_and_raw_visits_go_after_the_retention() {
    for test in testing::stores().await {
        let store = &test.store;
        let (now, today) = noon();
        // Four days ago, three days ago, yesterday and today.
        for (day, path, device, visitor) in [
            (today - 4 * DAY, "/", "desktop", "a"),
            (today - 3 * DAY, "/", "mobile", "a"),
            (today - DAY, "/docs", "mobile", "b"),
            (today - DAY, "/docs", "mobile", "b"),
            (today - DAY, "/", "desktop", "c"),
            (today, "/", "tablet", "d"),
        ] {
            insert(store, view(day, path, device, visitor)).await;
        }
        // A tenant's milestones, yesterday: made by Claude, secured with GitHub.
        let tenant = crate::accounts::tenants::new_tenant_id([5; 16]);
        sqlx::query("INSERT INTO tenancy_tenants (id, created_at) VALUES ($1, $2)")
            .bind(&tenant)
            .bind(today - DAY)
            .execute(store.admin())
            .await
            .unwrap();
        let mut connection = store.pool().acquire().await.unwrap();
        let yesterday = u64::try_from(today - DAY + 3600).unwrap();
        funnel::note_in(&mut connection, &tenant, funnel::MADE, "claude", yesterday)
            .await
            .unwrap();
        funnel::note_in(&mut connection, &tenant, funnel::SECURED, "github", yesterday)
            .await
            .unwrap();
        drop(connection);
        writer::maintain(store, 2, now).await.unwrap();
        assert_eq!(
            daily(store, today - DAY).await,
            [
                "campaign/news|email|launch=3/2",
                "client/claude=1/0",
                "device/desktop=1/1",
                "device/mobile=2/1",
                "event/made=1/0",
                "event/secured=1/0",
                "path//=1/1",
                "path//docs=2/1",
                "referrer/news.example.net=3/2",
                "sign_in/github=1/0",
                "total/=3/2",
            ]
        );
        let four_days_ago = [
            "campaign/news|email|launch=1/1",
            "device/desktop=1/1",
            "path//=1/1",
            "referrer/news.example.net=1/1",
            "total/=1/1",
        ];
        assert_eq!(daily(store, today - 4 * DAY).await, four_days_ago);
        // A day with nothing has its zero; today is not finished, so it has none yet.
        assert_eq!(daily(store, today - 2 * DAY).await, ["total/=0/0"]);
        assert!(daily(store, today).await.is_empty());
        // Raw visits two days old and older are gone; their totals stay.
        assert_eq!(raw_days(store).await, [today - DAY, today]);
        // Again, and from two instances at once: the same totals, nothing doubled.
        let (one, two) = tokio::join!(
            writer::maintain(store, 2, now),
            writer::maintain(store, 2, now)
        );
        one.unwrap();
        two.unwrap();
        assert_eq!(daily(store, today - 4 * DAY).await, four_days_ago);
        assert_eq!(daily(store, today - DAY).await.len(), 11);
        // An instance whose clock still has four days ago inside the retention finds its
        // visits gone: their totals stay as they were, never zero.
        writer::maintain(store, 5, now).await.unwrap();
        assert_eq!(daily(store, today - 4 * DAY).await, four_days_ago);
        assert_eq!(daily(store, today - DAY).await.len(), 11);
        // The next day, once it has settled: yesterday's today is rolled up too.
        writer::maintain(store, 2, now + DAY as u64 - 3 * 3600)
            .await
            .unwrap();
        assert_eq!(
            daily(store, today).await,
            [
                "campaign/news|email|launch=1/1",
                "device/tablet=1/1",
                "path//=1/1",
                "referrer/news.example.net=1/1",
                "total/=1/1"
            ]
        );
        assert_eq!(raw_days(store).await, [today - DAY, today]);
    }
}

#[tokio::test]
async fn a_visit_written_after_its_day_was_rolled_up_is_counted_the_next_pass() {
    // A visit still queued when its day was first rolled up (a backlog, a slow store,
    // another hub instance's queue) is counted by the next pass; one older than the
    // retention is never written.
    for test in testing::stores().await {
        let store = &test.store;
        let (now, today) = noon();
        let analytics = Analytics::start(
            store.clone(),
            AnalyticsConfig {
                enabled: true,
                retention_days: 2,
            },
            "https://hub.example.org",
        );
        // Yesterday has a milestone, so it is rolled up, before its visit is written.
        let tenant = crate::accounts::tenants::new_tenant_id([6; 16]);
        sqlx::query("INSERT INTO tenancy_tenants (id, created_at) VALUES ($1, 1)")
            .bind(&tenant)
            .execute(store.admin())
            .await
            .unwrap();
        let mut connection = store.pool().acquire().await.unwrap();
        let yesterday = u64::try_from(today - DAY + 60).unwrap();
        funnel::note_in(&mut connection, &tenant, funnel::MADE, "cursor", yesterday)
            .await
            .unwrap();
        drop(connection);
        writer::maintain(store, 2, now).await.unwrap();
        assert_eq!(
            daily(store, today - DAY).await,
            ["client/cursor=1/0", "event/made=1/0", "total/=0/0"]
        );
        analytics.page_view(view(today - DAY, "/docs", "mobile", "late"));
        analytics.page_view(view(today - 3 * DAY, "/docs", "mobile", "too-old"));
        analytics.flush().await;
        writer::maintain(store, 2, now).await.unwrap();
        assert_eq!(
            daily(store, today - DAY).await,
            [
                "campaign/news|email|launch=1/1",
                "client/cursor=1/0",
                "device/mobile=1/1",
                "event/made=1/0",
                "path//docs=1/1",
                "referrer/news.example.net=1/1",
                "total/=1/1",
            ]
        );
        assert_eq!(raw_days(store).await, [today - DAY], "the too-old one never kept");
    }
}

#[tokio::test]
async fn the_tables_are_the_hubs_own_on_both_engines_and_a_milestone_is_kept_once() {
    // As the hub's role (on Postgres, held to row-level security): it writes and reads
    // them, and they hold no tenant's rows of the kind a tenant policy guards.
    for test in testing::stores().await {
        let store = &test.store;
        let accounts = crate::accounts::Accounts::open(store.clone())
            .await
            .unwrap();
        let home = accounts.home_tenant().await.unwrap();
        let tenant = crate::accounts::tenants::new_tenant_id([9; 16]);
        sqlx::query("INSERT INTO tenancy_tenants (id, created_at) VALUES ($1, 1)")
            .bind(&tenant)
            .execute(store.admin())
            .await
            .unwrap();
        let mut connection = store.pool().acquire().await.unwrap();
        for at in [100, 200] {
            funnel::note_in(&mut connection, &tenant, funnel::MACHINE, "", at)
                .await
                .unwrap();
            funnel::note_in(&mut connection, &tenant, funnel::OPERATION, "", at)
                .await
                .unwrap();
            // Never the home tenant's, nor one the hub does not have.
            funnel::note_in(&mut connection, &home, funnel::OPERATION, "", at)
                .await
                .unwrap();
            funnel::note_in(&mut connection, "t-nobody", funnel::MADE, "claude", at)
                .await
                .unwrap();
        }
        let rows = sqlx::query(
            "SELECT tenant_id, kind, at, last_at FROM hub_analytics_events ORDER BY kind",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        let rows: Vec<(String, String, i64, i64)> = rows
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
            .collect();
        assert_eq!(
            rows,
            [
                (tenant.clone(), "machine".to_string(), 100, 100),
                (tenant.clone(), "operation".to_string(), 100, 200),
            ]
        );
    }
}

#[tokio::test]
async fn the_report_reads_finished_days_from_the_totals_and_today_from_the_visits() {
    for test in testing::stores().await {
        let store = &test.store;
        let now = crate::devices::now_secs();
        let today = day_of(now);
        let analytics = Analytics::start(
            store.clone(),
            AnalyticsConfig::default(),
            "https://hub.example.org",
        );
        analytics.page_view(view(today - DAY, "/docs", "mobile", "a"));
        analytics.page_view(view(today - DAY, "/docs", "mobile", "b"));
        analytics.page_view(view(today, "/", "desktop", "c"));
        analytics.flush().await;
        writer::maintain(store, 90, u64::try_from(today + DAY / 2).unwrap())
            .await
            .unwrap();
        // Yesterday's visits gone: its totals are what the report reads.
        sqlx::query("DELETE FROM hub_analytics_views WHERE day < $1")
            .bind(today)
            .execute(store.admin())
            .await
            .unwrap();
        let report = super::report::report(store, 7, now).await.unwrap();
        assert_eq!(report.days.len(), 7);
        assert_eq!(report.days[0].0, today - 6 * DAY);
        assert_eq!(report.days[5], (today - DAY, 2, 2));
        assert_eq!(report.days[6], (today, 1, 1));
        let pages: Vec<(String, i64, i64)> = report
            .pages
            .iter()
            .map(|row| (row.item.clone(), row.views, row.visitors))
            .collect();
        assert_eq!(pages, [("/docs".into(), 2, 2), ("/".into(), 1, 1)]);
        let devices: Vec<(&str, i64)> = report
            .devices
            .iter()
            .map(|row| (row.item.as_str(), row.views))
            .collect();
        assert_eq!(devices, [("mobile", 2), ("desktop", 1)]);
        assert_eq!(report.funnel.visitors, 3);
        assert_eq!(report.referrers[0].views, 3);
        let csv = super::report::csv(store, 7, now).await.unwrap();
        assert!(csv.contains(&format!(
            "{},total,,2,2\r\n",
            super::report::iso_day(today - DAY)
        )));
    }
}
