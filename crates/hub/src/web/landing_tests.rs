//! The landing page (`landing.rs`): every section is there for a claimed hub, a new hub
//! offers only the claim, the MCP address is this hub's own, the comparison cites its
//! sources, planned work says so, and nothing in it needs more than the plain pages' CSP.

use axum::{body::to_bytes, http::header, response::Response};

use super::{
    landing::page,
    pages::{Front, home},
};

const URL: &str = "https://hub.example.org";

fn body(front: Front) -> String {
    page(&front, URL).1
}

async fn served(response: Response) -> (String, String) {
    let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    let html = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (csp, String::from_utf8(html.to_vec()).unwrap())
}

#[test]
fn a_claimed_hub_shows_the_whole_product_page() {
    let html = body(Front::Passkeys);
    for (what, needle) in [
        (
            "hero",
            "<h1>One hub between your AI and every machine you have.</h1>",
        ),
        (
            "sign-in",
            "href=\"/auth/login?return_to=/account\">Sign in</a>",
        ),
        ("how it works", "id=\"how\""),
        ("comparison", "id=\"compare\""),
        ("features", "id=\"features\""),
        ("connect", "id=\"connect\""),
        ("self-host", "id=\"self-host\""),
        ("the one command", "docker compose up -d"),
        ("kept current", "<code>install.sh</code>"),
        ("FAQ", "id=\"faq\""),
        ("closing", "Give your AI the whole space. Keep the keys."),
        ("footer", "<a href=\"/privacy\">Privacy</a>"),
    ] {
        assert!(html.contains(needle), "{what}: {needle} missing");
    }
    // Three steps, a sequence; eight questions that open without script.
    let how = section(&html, "id=\"how\"");
    assert_eq!(how.matches("<li>").count(), 3, "{how}");
    assert_eq!(html.matches("<details>").count(), 8);
    // The order the page reads in.
    let order: Vec<usize> = [
        "<h1>",
        "id=\"how\"",
        "id=\"compare\"",
        "id=\"features\"",
        "id=\"connect\"",
        "id=\"self-host\"",
        "id=\"faq\"",
        "class=\"band closing\"",
    ]
    .iter()
    .map(|needle| html.find(needle).unwrap())
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    // A hub with its own accounts never names another sign-in, and offers no claim.
    assert!(!html.contains("Shippin") && !html.contains("/claim"));
}

#[test]
fn ai_apps_connect_at_this_hubs_own_address_and_nothing_else_is_hard_coded() {
    for front in [Front::Passkeys, Front::Shippin] {
        let html = body(front);
        assert!(html.contains(&format!("<code>{URL}/mcp</code>")), "{html}");
        assert!(!html.contains("app.devagotchi.com"));
        assert_eq!(html.matches("/mcp").count(), 1, "one address, this hub's");
    }
    // The address is escaped like every other value.
    let (_, odd) = page(&Front::Passkeys, "https://hub.example.org/a\"<b>/");
    assert!(odd.contains("<code>https://hub.example.org/a&quot;&lt;b&gt;/mcp</code>"));
}

#[test]
fn the_comparison_cites_desktop_commanders_source_on_every_row() {
    let html = body(Front::Passkeys);
    let compare = section(&html, "id=\"compare\"");
    let rows: Vec<&str> = compare.split("<tr><th scope=\"row\">").skip(1).collect();
    assert_eq!(rows.len(), 11);
    for row in rows {
        let cite = row
            .split("<span class=\"cite\">")
            .nth(1)
            .and_then(|rest| rest.split("</span>").next())
            .unwrap_or_else(|| panic!("no citation: {row}"));
        assert!(cite.starts_with('§') || cite == "plan §14", "{cite}");
        assert!(row.contains("data-label=\"Devagotchi\""), "{row}");
    }
    assert!(compare.contains("commit c2b8795"));
    assert!(compare.contains("/blob/main/docs/desktop-commander-teardown.md"));
    assert!(compare.contains("Desktop Commander is #1."));
}

#[test]
fn what_is_planned_says_it_is_coming() {
    let html = body(Front::Passkeys);
    let coming = html.find("Coming, and not built yet").expect("the label");
    // The mesh is the one planned feature named; it appears only under the label.
    assert!(html.find("Iroh").unwrap() > coming);
    assert_eq!(html.matches("Iroh").count(), 1);
    assert!(html.contains("macOS is coming"));
    assert!(html.contains("invitations, which are coming"));
}

#[test]
fn a_new_hub_leads_with_the_claim_and_offers_nothing_else() {
    let html = body(Front::Unclaimed);
    assert!(html.contains("Nobody owns this hub yet"));
    assert!(html.contains("<h1>One hub between your AI and every machine you have.</h1>"));
    // The first thing that reads "claim this hub" is the link to /claim (a browser test
    // clicks the first match), and it is the only button.
    let first = html.to_lowercase().find("claim this hub").unwrap();
    let link = html
        .find("<a class=\"button primary\" href=\"/claim\">Claim this hub</a>")
        .unwrap();
    assert_eq!(
        first,
        link + "<a class=\"button primary\" href=\"/claim\">".len()
    );
    assert_eq!(html.matches("class=\"button").count(), 1);
    for absent in [
        "/auth/login",
        "Shippin",
        "id=\"compare\"",
        "id=\"connect\"",
        "/mcp",
    ] {
        assert!(!html.contains(absent), "{absent}");
    }
    assert!(html.contains("docker compose logs hub"));
}

#[test]
fn shippin_hubs_sign_in_with_shippin() {
    let html = body(Front::Shippin);
    assert!(html.contains("href=\"/auth/login?return_to=/account\">Sign in with Shippin</a>"));
    assert!(html.contains("A Shippin account is free."));
    assert!(!html.contains("/claim"));
}

#[tokio::test]
async fn the_page_needs_nothing_the_plain_csp_refuses() {
    for front in [Front::Unclaimed, Front::Passkeys, Front::Shippin] {
        let (csp, html) = served(home(front, URL)).await;
        assert!(
            csp.starts_with("default-src 'none'; style-src 'self';") && !csp.contains("script-src"),
            "{csp}"
        );
        assert!(html.contains("<body class=\"landing\">"));
        assert!(html.contains("<link rel=\"stylesheet\" href=\"/assets/hub.css\">"));
        // No script, no inline style, nothing loaded from anywhere: inline SVG only.
        for refused in ["<script", "<style", " style=", " src=", "<img"] {
            assert!(!html.contains(refused), "{refused}");
        }
        // The diagram's arrowheads are markers in the same SVG, never a fetch.
        assert!(html.split("url(").skip(1).all(|rest| rest.starts_with('#')));
        // Links leave only for the source on GitHub.
        for href in html.split("href=\"").skip(1) {
            let href = href.split('"').next().unwrap();
            assert!(
                href.starts_with('/')
                    || href.starts_with('#')
                    || href.starts_with("https://github.com/EonsofStupid/devagotchi"),
                "{href}"
            );
        }
    }
}

/// The text of the `<section>` that carries `marker`.
fn section<'a>(html: &'a str, marker: &str) -> &'a str {
    let start = html.find(marker).expect(marker);
    let end = html[start..].find("</section>").unwrap();
    &html[start..start + end]
}
