//! The operator's analytics page (`/operator/analytics`) on a hosted hub, the whole router
//! as it serves: only the operator sees it or its CSV (anyone else gets an unknown path's
//! 404), the operator page links to it, and its counts and funnel move as people visit and
//! connect. On SQLite, and on Postgres when the test server is set (CI).

use serde_json::json;

use super::operator_tests::{hub_on, operator_cookie};
use crate::{
    site::tests::assert_no_brand,
    store,
    tenant_tests::{Hosted, hosted_with},
};

const DESKTOP: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

async fn get(hub: &Hosted, path: &str, cookie: Option<&str>) -> (u16, String) {
    if let Some(analytics) = &hub.hub.analytics {
        analytics.flush().await;
    }
    Hosted::text(hub.get(path, cookie).await).await
}

async fn visit(hub: &Hosted, path: &str, referer: Option<&str>) {
    let mut request = hub
        .http
        .get(format!("{}{path}", hub.url))
        .header("user-agent", DESKTOP);
    if let Some(referer) = referer {
        request = request.header("referer", referer);
    }
    assert_eq!(request.send().await.unwrap().status(), 200, "{path}");
}

/// The funnel row named `step`: its count and its rate from the step before.
fn step(html: &str, step: &str) -> (String, String) {
    let row = html
        .split("<tr>")
        .find(|row| row.starts_with(&format!("<th scope=\"row\">{step}</th>")))
        .unwrap_or_else(|| panic!("{step} in {html}"));
    let cells: Vec<&str> = row
        .split("<td class=\"n\">")
        .skip(1)
        .map(|cell| cell.split('<').next().unwrap())
        .collect();
    (cells[0].to_string(), cells[1].to_string())
}

#[tokio::test]
async fn the_operator_sees_the_counts_and_the_funnel_move_and_nobody_else_sees_anything() {
    let mut stores = vec![None];
    if let Some(url) = store::testing::empty_postgres().await {
        stores.push(Some(url));
    }
    for store in stores {
        dashboard(hub_on(store.as_deref(), None).await).await;
    }
}

async fn dashboard(hub: Hosted) {
    let operator = operator_cookie(&hub).await;
    // Nobody but the operator finds it, or its CSV.
    for path in [
        "/operator/analytics",
        "/operator/analytics.csv",
        "/operator/analytics?days=7",
    ] {
        assert_eq!(get(&hub, path, None).await, (404, String::new()), "{path}");
    }
    // The operator page leads there; nothing is counted yet.
    let (_, page) = get(&hub, "/operator", Some(&operator)).await;
    assert!(page.contains("href=\"/operator/analytics\""), "{page}");
    let (status, html) = get(&hub, "/operator/analytics", Some(&operator)).await;
    assert_eq!(status, 200, "{html}");
    assert!(html.contains("<h1>Analytics</h1>"), "{html}");
    assert_eq!(step(&html, "Visitors"), ("0".into(), "—".into()));
    assert_eq!(step(&html, "Accounts made"), ("0".into(), "—".into()));
    assert!(html.contains("Nothing yet."), "{html}");

    // Three visits by one person from a link, one more from another site.
    visit(
        &hub,
        "/?utm_source=hn&utm_medium=social&utm_campaign=launch",
        None,
    )
    .await;
    visit(&hub, "/docs", Some("https://news.example.net/item")).await;
    visit(&hub, "/privacy", None).await;
    // A person connects Claude: an account made, waiting for early access.
    let person = hub.first_connect("Claude").await;
    for path in ["/operator/analytics", "/operator/analytics.csv"] {
        assert_eq!(
            get(&hub, path, Some(&person.cookie)).await,
            (404, String::new())
        );
    }
    let (status, html) = get(&hub, "/operator/analytics?days=7", Some(&operator)).await;
    assert_eq!(status, 200);
    // The operator's own visits to it, and to the public pages, are not counted.
    visit_as_operator(&hub, &operator).await;
    let (_, html_again) = get(&hub, "/operator/analytics?days=7", Some(&operator)).await;
    assert!(
        html_again.contains("3 page views and 1 visitors over 7 days."),
        "{html_again}"
    );
    for want in [
        "aria-current=\"page\">7 days</a>",
        "href=\"/operator/analytics?days=30\"",
        "href=\"/operator/analytics?days=90\"",
        "href=\"/operator/analytics.csv?days=7\"",
        "role=\"img\"",
        "3 page views and 1 visitors over 7 days.",
        "<code>/docs</code>",
        "<th scope=\"row\">news.example.net</th>",
        "<th scope=\"row\">hn</th><td class=\"n\">social</td><td class=\"n\">launch</td>",
        "<th scope=\"row\">Computer</th><td class=\"n\">3</td><td class=\"n\">100.0%</td>",
        "<th scope=\"row\">Claude</th><td class=\"n\">1</td>",
        "Waiting for early access now: <strong>1</strong>.",
        "<strong>0</strong> in the last 7 days",
        "Raw visits are kept 90 days",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert_eq!(step(&html, "Visitors"), ("1".into(), "—".into()));
    assert_eq!(step(&html, "Accounts made"), ("1".into(), "100.0%".into()));
    assert_eq!(step(&html, "Secured"), ("0".into(), "0.0%".into()));
    assert_eq!(step(&html, "Let in"), ("0".into(), "—".into()));
    // Today's tooltip has today's numbers.
    let today =
        crate::analytics::report::iso_day(crate::analytics::day_of(crate::devices::now_secs()));
    assert!(
        html.contains(&format!("<title>{today}: 3 page views, 1 visitors</title>")),
        "{html}"
    );
    // Let in: the funnel moves.
    let tenant = hub.accounts.tenants_in("waiting", 10).await.unwrap();
    hub.accounts
        .admit(&tenant, None, "acct_operator")
        .await
        .unwrap();
    let (_, html) = get(&hub, "/operator/analytics", Some(&operator)).await;
    assert_eq!(step(&html, "Let in"), ("1".into(), "—".into()));
    assert!(
        html.contains("aria-current=\"page\">30 days</a>"),
        "the default range"
    );
    assert!(html.contains("Waiting for early access now: <strong>0</strong>."));
    // Any other range is the default one.
    let (_, other) = get(&hub, "/operator/analytics?days=13", Some(&operator)).await;
    assert!(other.contains("aria-current=\"page\">30 days</a>"));

    // The CSV: the daily totals, today's among them, as a file.
    let csv = hub
        .http
        .get(format!("{}/operator/analytics.csv?days=7", hub.url))
        .header("cookie", &operator)
        .send()
        .await
        .unwrap();
    assert_eq!(csv.status(), 200);
    assert_eq!(csv.headers()["content-type"], "text/csv; charset=utf-8");
    assert!(
        csv.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .ends_with(&format!("-to-{today}.csv\"")),
    );
    let text = csv.text().await.unwrap();
    assert!(
        text.starts_with("day,metric,item,views,visitors\r\n"),
        "{text}"
    );
    for row in [
        format!("{today},total,,3,1\r\n"),
        format!("{today},path,/docs,1,1\r\n"),
        format!("{today},campaign,hn|social|launch,1,1\r\n"),
        format!("{today},client,claude,1,0\r\n"),
        format!("{today},event,made,1,0\r\n"),
    ] {
        assert!(text.contains(&row), "{row} in {text}");
    }
}

async fn visit_as_operator(hub: &Hosted, operator: &str) {
    for path in ["/", "/docs"] {
        let answer = hub
            .http
            .get(format!("{}{path}", hub.url))
            .header("user-agent", DESKTOP)
            .header("cookie", operator)
            .send()
            .await
            .unwrap();
        assert!(answer.status().is_success() || answer.status().is_redirection());
    }
}

#[tokio::test]
async fn a_hub_that_counts_nothing_says_so_to_its_operator() {
    let hub = hosted_with(
        None,
        |_, _| json!({ "operators": ["github:42"], "analytics": { "enabled": false } }),
    )
    .await;
    let operator = operator_cookie(&hub).await;
    let (status, html) = get(&hub, "/operator/analytics", Some(&operator)).await;
    assert_eq!(status, 200);
    assert!(
        html.contains("id=\"off\"") && html.contains("analytics.enabled"),
        "{html}"
    );
    assert!(
        !html.contains("role=\"img\"") && !html.contains("no cookie"),
        "{html}"
    );
}

#[tokio::test]
async fn a_neutral_sites_analytics_page_names_only_that_site() {
    static NEUTRAL: std::sync::LazyLock<std::path::PathBuf> =
        std::sync::LazyLock::new(crate::site::tests::neutral_dir);
    let hub = hub_on(None, Some(NEUTRAL.as_path())).await;
    visit(&hub, "/docs", None).await;
    hub.accounts
        .start_unsecured(std::net::IpAddr::from([198, 51, 100, 9]), "Claude")
        .await
        .unwrap();
    let operator = operator_cookie(&hub).await;
    let (status, html) = get(&hub, "/operator/analytics", Some(&operator)).await;
    assert_eq!(status, 200);
    assert!(html.contains(" · Lumen Relay</title>"), "{html}");
    assert_no_brand("/operator/analytics", &html);
    let (_, csv) = get(&hub, "/operator/analytics.csv", Some(&operator)).await;
    assert_no_brand("/operator/analytics.csv", &csv);
}
