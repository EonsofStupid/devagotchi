//! What search engines and AI agents read, over HTTP on a hosted hub with the whole router
//! as it serves (`discovery.rs`, `site/discovery.rs`): each route and its content type;
//! robots keeps every crawler out of the private paths and lets it read the public pages;
//! the sitemap lists those pages at this hub's address; `/llms.txt` carries this hub's own
//! address and the steps an AI follows, in the llmstxt.org shape, naming only tools the
//! hub serves; the landing page's JSON-LD parses and says only what the page shows; every
//! public page has its description, canonical link and cards; the protected-resource
//! metadata and the MCP instructions name the product and its docs; a neutral site shows
//! nothing of the built-in one; and no other product is named anywhere a person or an AI
//! reads.

use axum::http::header;
use serde_json::{Value, json};

use crate::{
    site::{self, Ctx, Front, discovery},
    tenant_tests::{Hosted, hosted_with},
};

async fn hosted() -> Hosted {
    hosted_with(None, |_, _| json!({})).await
}

async fn neutral() -> Hosted {
    let dir = site::tests::neutral_dir();
    hosted_with(None, move |_, _| {
        json!({ "site": { "dir": dir.to_string_lossy() } })
    })
    .await
}

/// The built-in site's marketing domain: where its public pages live.
const SITE: &str = "https://devagotchi.com";

/// `path`'s status, content type and body.
async fn get(hub: &Hosted, path: &str) -> (u16, String, String) {
    get_on(hub, None, path).await
}

/// `path` asked of the hub as `host` (the site's marketing domain reaching it through the
/// edge), or as its own address.
async fn get_on(hub: &Hosted, host: Option<&str>, path: &str) -> (u16, String, String) {
    let mut request = hub.http.get(format!("{}{path}", hub.url));
    if let Some(host) = host {
        request = request.header(header::HOST, host);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let kind = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|value| value.to_str().unwrap().to_string())
        .unwrap_or_default();
    (status, kind, response.text().await.unwrap())
}

/// Whether robots.txt keeps a crawler out of `path`: its longest matching rule wins, and
/// Allow wins a tie (RFC 9309 §2.2.2).
fn disallowed(robots: &str, path: &str) -> bool {
    let mut best: Option<(usize, bool)> = None;
    for line in robots.lines() {
        let (allow, rule) = match line.split_once(':') {
            Some(("Allow", rule)) => (true, rule.trim()),
            Some(("Disallow", rule)) => (false, rule.trim()),
            _ => continue,
        };
        if rule.is_empty() || !path.starts_with(rule) {
            continue;
        }
        let better = match best {
            None => true,
            Some((len, was_allow)) => {
                rule.len() > len || (rule.len() == len && allow && !was_allow)
            }
        };
        if better {
            best = Some((rule.len(), allow));
        }
    }
    best.is_some_and(|(_, allow)| !allow)
}

/// Every `<script type="application/ld+json">` in `html`, parsed.
fn json_ld(html: &str) -> Vec<Value> {
    html.split("<script type=\"application/ld+json\">")
        .skip(1)
        .map(|rest| {
            let data = rest.split("</script>").next().unwrap();
            serde_json::from_str(data).unwrap_or_else(|e| panic!("{e}: {data}"))
        })
        .collect()
}

/// The `content` of `<meta {kind}="{key}">` in `html`.
fn meta<'a>(html: &'a str, kind: &str, key: &str) -> Option<&'a str> {
    html.split(&format!("<meta {kind}=\"{key}\" content=\""))
        .nth(1)
        .and_then(|rest| rest.split('"').next())
}

/// The public pages, by path.
const PUBLIC: [&str; 5] = ["/", "/docs", "/privacy", "/terms", "/support"];

#[tokio::test]
async fn every_route_answers_with_its_type_and_no_sign_in() {
    let hub = hosted().await;
    for (path, kind) in [
        ("/llms.txt", "text/plain; charset=utf-8"),
        ("/llms-full.txt", "text/plain; charset=utf-8"),
        ("/robots.txt", "text/plain; charset=utf-8"),
        ("/sitemap.xml", "application/xml; charset=utf-8"),
        ("/", "text/html; charset=utf-8"),
        ("/docs", "text/html; charset=utf-8"),
    ] {
        let (status, got, body) = get(&hub, path).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(got, kind, "{path}");
        assert!(!body.trim().is_empty(), "{path}");
    }
}

#[tokio::test]
async fn robots_lets_every_crawler_read_the_public_pages_and_none_the_private_ones() {
    let hub = hosted().await;
    let (_, _, robots) = get(&hub, "/robots.txt").await;
    // One group for every crawler, AI crawlers included.
    assert_eq!(robots.matches("User-agent:").count(), 1, "{robots}");
    assert!(robots.contains("User-agent: *\n"), "{robots}");
    assert!(
        robots.contains(&format!("\nSitemap: {}/sitemap.xml\n", hub.url)),
        "{robots}"
    );
    for path in PUBLIC.iter().chain(&[
        "/llms.txt",
        "/llms-full.txt",
        "/sitemap.xml",
        "/assets/hub.css",
    ]) {
        assert!(!disallowed(&robots, path), "{path} is kept out:\n{robots}");
    }
    for path in [
        "/account",
        "/operator",
        "/operator/analytics",
        "/go/abc",
        "/oauth/authorize",
        "/api/v1/me",
        "/join/abc",
        "/dist/devagotchi.tar.gz",
        "/auth/login",
        "/approvals",
        "/claim",
        "/mcp",
        "/offers/dgo_abc",
    ] {
        assert!(disallowed(&robots, path), "{path} is let in:\n{robots}");
    }
    for rule in [
        "/account", "/operator", "/go/", "/oauth", "/api", "/join", "/dist",
    ] {
        assert!(robots.contains(&format!("\nDisallow: {rule}\n")), "{rule}");
    }
}

#[tokio::test]
async fn the_sitemap_lists_the_public_pages_at_the_sites_own_address() {
    let hub = hosted().await;
    let (_, _, robots) = get(&hub, "/robots.txt").await;
    let (_, _, sitemap) = get(&hub, "/sitemap.xml").await;
    // The same on the marketing host, where each answers too, with no redirect.
    let (status, kind, there) = get_on(&hub, Some("devagotchi.com"), "/sitemap.xml").await;
    assert_eq!((status, kind.as_str(), there.as_str()), (200, "application/xml; charset=utf-8", sitemap.as_str()));
    assert!(sitemap.starts_with(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">"
    ));
    let locs: Vec<&str> = sitemap
        .split("<loc>")
        .skip(1)
        .map(|rest| rest.split("</loc>").next().unwrap())
        .collect();
    // The site's marketing domain (`site.json` product.domain), never the hub's own.
    let expected: Vec<String> = PUBLIC.iter().map(|path| format!("{SITE}{path}")).collect();
    assert_eq!(locs, expected);
    assert!(!sitemap.contains(&hub.url));
    for loc in &locs {
        let path = loc.strip_prefix(SITE).unwrap();
        assert!(!disallowed(&robots, path), "{path}");
        let (status, _, _) = get_on(&hub, Some("devagotchi.com"), path).await;
        assert_eq!(status, 200, "{path}");
    }
    // The date a page says it changed, as W3C wants it.
    let updated = &site::builtin().pages["docs"].updated;
    let date = discovery::iso_date(updated).expect("docs says when it changed");
    assert!(sitemap.contains(&format!(
        "<loc>{SITE}/docs</loc><lastmod>{date}</lastmod>"
    )));
    assert_eq!(
        discovery::iso_date("1 November 2026").as_deref(),
        Some("2026-11-01")
    );
    assert_eq!(discovery::iso_date("soon"), None);
}

#[tokio::test]
async fn llms_txt_carries_this_hubs_address_and_the_steps_an_ai_follows() {
    let hub = hosted().await;
    let url = &hub.url;
    let (_, _, llms) = get(&hub, "/llms.txt").await;
    // The llmstxt.org shape: the name, a summary, details without headings, then file
    // lists under H2s, the last one Optional.
    assert!(llms.starts_with("# Devagotchi\n\n> "), "{llms}");
    let headings: Vec<&str> = llms.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(headings, ["# Devagotchi", "## Docs", "## Optional"], "{llms}");
    for line in llms.split("## Docs").nth(1).unwrap().lines() {
        assert!(
            line.is_empty() || line.starts_with("## ") || line.starts_with("- ["),
            "{line}"
        );
    }
    // The steps, in order, at this hub's own address: connect, start, the one line, confirm.
    let at = |needle: &str| {
        llms.find(needle)
            .unwrap_or_else(|| panic!("{needle}:\n{llms}"))
    };
    assert!(
        at(&format!(
            "1. Connect to this hub's MCP server at `{url}/mcp`"
        )) < at("2. Call `workflow_list`, then `workflow_start`")
    );
    assert!(at("2. Call") < at("3. The user runs the one line"));
    assert!(at("3. The user") < at("4. Confirm it worked"));
    assert!(llms.contains(&format!("`curl -fsSL {url}/join/<code> | sudo sh`")));
    assert!(llms.contains(&format!("`irm {url}/join/<code>.ps1 | iex`")));
    for tool in ["`machine_add`", "`machines`", "`workflow_status`"] {
        assert!(llms.contains(tool), "{tool}");
    }
    // A hosted hub: the account is made on the first connect. Nothing for other hubs.
    assert!(llms.contains("the account is made the moment it connects"));
    assert!(!llms.contains("passkey") && !llms.contains("Shippin"));
    // Its page links at the site's own address; the MCP and join steps at this hub's.
    assert!(llms.contains(&format!("- [Docs]({SITE}/docs)")));
    assert!(llms.contains(&format!("({SITE}/llms-full.txt)")));
    assert!(!llms.contains(&format!("({url}/")), "{llms}");
    // Every tool it names is one the hub serves; every placeholder is filled.
    let served = crate::hub::tool_names();
    for name in llms.split('`').skip(1).step_by(2) {
        if name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            assert!(served.iter().any(|s| s == name), "{name} is not a tool");
        }
    }
    assert!(!llms.contains('{') && !llms.contains('}'), "{llms}");
    assert!(!llms.contains("app.devagotchi.com") && !llms.contains("hub.example.org"));

    // The full file: the same, then each public page under its address.
    let (_, _, full) = get(&hub, "/llms-full.txt").await;
    assert!(full.starts_with(&llms), "{full}");
    let mut last = 0;
    for page in discovery::PAGES {
        let at = full
            .find(&format!("\n---\n\n<{SITE}/{page}>\n\n"))
            .unwrap_or_else(|| panic!("{page}"));
        assert!(at > last, "{page} out of order");
        last = at;
    }
    assert!(
        full.contains(&format!("`{url}/mcp`")),
        "the docs at this hub's address"
    );
    assert!(full.contains("# Privacy policy") && full.contains("# Terms of service"));
    // The docs name only tools the hub serves.
    let docs = full
        .split(&format!("<{SITE}/docs>"))
        .nth(1)
        .and_then(|rest| rest.split("\n---\n").next())
        .unwrap();
    let mut named = 0;
    for name in docs.split('`').skip(1).step_by(2) {
        if name.contains('_') && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            assert!(served.iter().any(|s| s == name), "the docs name {name}");
            named += 1;
        }
    }
    assert!(named > 20, "{docs}");
    for unfilled in ["{product}", "{mcp}", ":::"] {
        assert!(!full.contains(unfilled), "{unfilled}");
    }
}

#[tokio::test]
async fn the_landing_pages_json_ld_parses_and_says_only_what_the_page_shows() {
    let hub = hosted().await;
    let url = &hub.url;
    let (_, _, html) = get(&hub, "/").await;
    let data = json_ld(&html);
    let types: Vec<&str> = data
        .iter()
        .map(|d| d["@type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["SoftwareApplication", "HowTo", "FAQPage"]);
    for item in &data {
        assert_eq!(item["@context"], "https://schema.org");
    }
    let app = &data[0];
    assert_eq!(app["name"], "Devagotchi");
    // Its addresses are the site's; its picture is the hub's own asset.
    assert_eq!(app["url"], format!("{SITE}/"));
    assert_eq!(app["applicationCategory"], "DeveloperApplication");
    assert_eq!(app["operatingSystem"], "Linux, Windows");
    assert_eq!(
        app["offers"],
        json!({"@type": "Offer", "price": "0", "priceCurrency": "USD"})
    );
    assert_eq!(app["softwareHelp"]["url"], format!("{SITE}/docs"));
    assert!(
        app["image"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{url}/assets/turnkey-poster.png?v="))
    );
    // The steps and the questions are the ones on the page, as plain words.
    let steps = data[1]["step"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    for (i, step) in steps.iter().enumerate() {
        assert_eq!(step["position"], i + 1);
        assert_eq!(step["url"], format!("{SITE}/#how"));
        let name = step["name"].as_str().unwrap();
        assert!(html.contains(&format!("<h3>{name}</h3>")), "{name}");
        assert!(!step["text"].as_str().unwrap().is_empty());
    }
    let questions = data[2]["mainEntity"].as_array().unwrap();
    assert_eq!(questions.len(), html.matches("<details>").count());
    for q in questions {
        let name = q["name"].as_str().unwrap();
        assert!(
            html.contains(&format!("<summary>{name}</summary>")),
            "{name}"
        );
        let answer = q["acceptedAnswer"]["text"].as_str().unwrap();
        assert!(
            !answer.contains("**") && !answer.contains('`') && !answer.contains("]("),
            "{answer}"
        );
    }
    // A hub nobody owns yet shows only its claim: the software, nothing else, and it asks
    // not to be indexed.
    let site = site::builtin();
    assert_eq!(
        discovery::landing_json_ld(&site, Front::Unclaimed, url).len(),
        1
    );
    let new = super::pages::home(Front::Unclaimed, url);
    let new = axum::body::to_bytes(new.into_body(), usize::MAX)
        .await
        .unwrap();
    let new = String::from_utf8(new.to_vec()).unwrap();
    assert_eq!(meta(&new, "name", "robots"), Some("noindex"));
    assert_eq!(meta(&html, "name", "robots"), None);
}

#[tokio::test]
async fn every_public_page_says_what_it_is_where_it_lives_and_how_it_previews() {
    let hub = hosted().await;
    let url = &hub.url;
    for path in PUBLIC {
        let (_, _, html) = get(&hub, path).await;
        let head = html.split("</head>").next().unwrap();
        // At the site's own address, whichever host served it.
        let canonical = format!("{SITE}{path}");
        let (_, _, there) = get_on(&hub, Some("devagotchi.com"), path).await;
        assert!(
            there.contains(&format!("<link rel=\"canonical\" href=\"{canonical}\">")),
            "{path} on the marketing host"
        );
        assert!(
            head.contains(&format!("<link rel=\"canonical\" href=\"{canonical}\">")),
            "{path}"
        );
        assert_eq!(
            meta(head, "property", "og:url"),
            Some(canonical.as_str()),
            "{path}"
        );
        let description = meta(head, "name", "description").unwrap_or_else(|| panic!("{path}"));
        assert!(!description.is_empty());
        assert_eq!(meta(head, "property", "og:description"), Some(description));
        assert_eq!(meta(head, "name", "twitter:description"), Some(description));
        assert_eq!(meta(head, "property", "og:site_name"), Some("Devagotchi"));
        let title = head
            .split("<title>")
            .nth(1)
            .unwrap()
            .split("</title>")
            .next()
            .unwrap();
        assert_eq!(meta(head, "property", "og:title"), Some(title), "{path}");
        assert_eq!(meta(head, "name", "twitter:title"), Some(title), "{path}");
        assert_eq!(
            meta(head, "name", "twitter:card"),
            Some("summary_large_image")
        );
        let image = meta(head, "property", "og:image").unwrap();
        assert!(
            image.starts_with(&format!("{url}/assets/turnkey-poster.png?v=")),
            "{image}"
        );
        assert_eq!(meta(head, "name", "twitter:image"), Some(image));
        assert!(meta(head, "property", "og:image:width").is_some());
        // The picture is one this hub serves.
        let (status, kind, _) = get(&hub, image.strip_prefix(url.as_str()).unwrap()).await;
        assert_eq!((status, kind.as_str()), (200, "image/png"));
        // JSON-LD on the landing page only; no other script anywhere.
        let scripts = html.matches("<script").count();
        let data = html
            .matches("<script type=\"application/ld+json\">")
            .count();
        assert_eq!(scripts, data, "{path}");
        assert_eq!(data > 0, path == "/", "{path}");
    }
    // A page's own description, where its front matter gives one.
    let (_, _, privacy) = get(&hub, "/privacy").await;
    let site = site::builtin();
    let own = &site.pages["privacy"].description;
    assert!(!own.is_empty());
    assert!(own.contains("{product}"));
    assert_eq!(
        meta(&privacy, "name", "description"),
        Some(site::text::attr(&Ctx::new(&site).plain(own)).as_str())
    );
}

#[tokio::test]
async fn the_resource_metadata_and_the_mcp_instructions_name_the_product_and_its_docs() {
    let hub = hosted().await;
    let url = &hub.url;
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let (status, _, body) = get(&hub, path).await;
        assert_eq!(status, 200);
        let metadata: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(metadata["resource_name"], "Devagotchi", "{path}");
        assert_eq!(metadata["resource_documentation"], format!("{url}/docs"));
        assert_eq!(metadata["resource_policy_uri"], format!("{url}/privacy"));
        assert_eq!(metadata["resource_tos_uri"], format!("{url}/terms"));
    }
    let info = rmcp::ServerHandler::get_info(&hub.hub);
    let instructions = info.instructions.unwrap();
    assert!(
        instructions.starts_with("Devagotchi gives you"),
        "{instructions}"
    );
    assert!(instructions.contains(&format!(
        "Docs: {url}/docs; for AI agents: {url}/llms.txt."
    )));
    assert_eq!(info.server_info.name, "devagotchi-hub");
}

/// The ways another product's name could be written.
const OTHERS: [&str; 3] = ["desktop commander", "desktopcommander", "desktop-commander"];

fn assert_names_no_other_product(what: &str, text: &str) {
    let lower = text.to_lowercase();
    for name in OTHERS {
        assert!(!lower.contains(name), "{what} names {name}");
    }
}

#[tokio::test]
async fn no_other_product_is_named_in_the_site_any_page_or_the_mcp_surface() {
    // Every file of the built-in site, as written.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site/default");
    let mut files = vec![dir];
    while let Some(path) = files.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                files.push(entry.unwrap().path());
            }
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            assert_names_no_other_product(&path.display().to_string(), &text);
        }
    }
    // Every landing page the hub can draw.
    let site = site::builtin();
    for front in [
        Front::Unclaimed,
        Front::Passkeys,
        Front::Nomen,
        Front::Hosted,
    ] {
        let ctx = Ctx::new(&site);
        let (title, body) = super::landing_for(&site, &ctx, front, "https://hub.example.org");
        assert_names_no_other_product(front.key(), &format!("{title}{body}"));
    }
    // Every page and file a hosted hub serves without sign-in.
    let hub = hosted().await;
    for path in [
        "/",
        "/docs",
        "/privacy",
        "/terms",
        "/support",
        "/llms.txt",
        "/llms-full.txt",
        "/robots.txt",
        "/sitemap.xml",
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-authorization-server",
        "/manifest.webmanifest",
        "/assets/hub.css",
        "/sw.js",
    ] {
        let (_, _, body) = get(&hub, path).await;
        assert_names_no_other_product(path, &body);
    }
    // What an AI reads: the instructions, and every tool as tools/list describes it.
    let info = rmcp::ServerHandler::get_info(&hub.hub);
    assert_names_no_other_product("instructions", &info.instructions.unwrap_or_default());
    for tool in crate::hub::tools() {
        assert_names_no_other_product(&tool.name, &serde_json::to_string(&tool).unwrap());
    }
}

#[tokio::test]
async fn a_neutral_site_shows_none_of_the_built_in_one_to_crawlers_or_agents() {
    let hub = neutral().await;
    let url = &hub.url;
    let (_, _, llms) = get(&hub, "/llms.txt").await;
    assert!(
        llms.starts_with("# Lumen Relay\n\n> A relay between your assistant and your computers\n"),
        "{llms}"
    );
    assert!(llms.contains(&format!("Connect at `{url}/mcp`")));
    let builtin = site::builtin();
    let defaults = discovery::llms_txt(&builtin, url, "hosted");
    let mut surfaces = vec![("/llms.txt".to_string(), llms)];
    for path in [
        "/llms-full.txt",
        "/robots.txt",
        "/sitemap.xml",
        "/",
        "/docs",
        "/privacy",
    ] {
        let (status, _, body) = get(&hub, path).await;
        assert_eq!(status, 200, "{path}");
        surfaces.push((path.to_string(), body));
    }
    for (what, text) in &surfaces {
        site::tests::assert_no_brand(what, text);
        // None of the built-in llms.txt's own lines, its software facts or its poster.
        for line in defaults.lines().filter(|l| l.len() > 30) {
            assert!(!text.contains(line), "{what} shows the built-in {line:?}");
        }
        for default in ["DeveloperApplication", "turnkey-poster", "priceCurrency"] {
            assert!(!text.contains(default), "{what} shows {default}");
        }
    }
    // Its own name in its cards and its JSON-LD; no picture, so a small card.
    let (_, _, landing) = get(&hub, "/").await;
    assert_eq!(
        meta(&landing, "property", "og:site_name"),
        Some("Lumen Relay")
    );
    assert_eq!(meta(&landing, "name", "twitter:card"), Some("summary"));
    assert_eq!(meta(&landing, "property", "og:image"), None);
    let data = json_ld(&landing);
    assert_eq!(data[0]["name"], "Lumen Relay");
    assert!(data[0].get("offers").is_none() && data[0].get("applicationCategory").is_none());
}

#[tokio::test]
async fn each_file_answers_on_both_hosts_naming_its_own_and_the_app_steps_stay_on_the_hub() {
    let hub = hosted().await;
    let url = &hub.url;
    for (host, own) in [
        (None, url.to_string()),
        (Some("devagotchi.com"), SITE.to_string()),
        // A host in another case is the same host.
        (Some("DevAgotchi.COM"), SITE.to_string()),
        // Any other host is answered as the hub's own; it is never written back.
        (Some("elsewhere.example"), url.to_string()),
    ] {
        for (path, kind) in [
            ("/llms.txt", "text/plain; charset=utf-8"),
            ("/llms-full.txt", "text/plain; charset=utf-8"),
            ("/robots.txt", "text/plain; charset=utf-8"),
            ("/sitemap.xml", "application/xml; charset=utf-8"),
        ] {
            let (status, got, body) = get_on(&hub, host, path).await;
            assert_eq!((status, got.as_str()), (200, kind), "{path} on {host:?}");
            assert!(!body.contains("elsewhere.example"), "{path}");
        }
        let (_, _, robots) = get_on(&hub, host, "/robots.txt").await;
        assert!(robots.contains(&format!("\nSitemap: {own}/sitemap.xml\n")), "{host:?}: {robots}");
        assert!(robots.contains(&format!("# {own}/llms.txt\n")), "{host:?}: {robots}");
        // llms.txt is the same on either host: the pages at the site's address, and what
        // only the hub has (its MCP address, the one line per machine) at the hub's.
        let (_, _, llms) = get_on(&hub, host, "/llms.txt").await;
        assert!(llms.contains(&format!("MCP server at `{url}/mcp`")), "{llms}");
        assert!(llms.contains(&format!("`curl -fsSL {url}/join/<code> | sudo sh`")));
        assert!(llms.contains(&format!("- [Docs]({SITE}/docs)")));
        assert!(!llms.contains(&format!("{SITE}/mcp")) && !llms.contains(&format!("{SITE}/join")));
    }
}

#[tokio::test]
async fn a_site_with_no_marketing_domain_keeps_everything_at_the_hubs_address() {
    let hub = hosted_with(None, |dir, _| {
        let site = crate::offers_tests::builtin_with(dir, |v| v["product"]["domain"] = "".into());
        json!({ "site": { "dir": site.to_string_lossy() } })
    })
    .await;
    let url = &hub.url;
    let (_, _, landing) = get(&hub, "/").await;
    assert!(landing.contains(&format!("<link rel=\"canonical\" href=\"{url}/\">")));
    assert_eq!(json_ld(&landing)[0]["url"], format!("{url}/"));
    let (_, _, sitemap) = get(&hub, "/sitemap.xml").await;
    assert!(sitemap.contains(&format!("<loc>{url}/docs</loc>")), "{sitemap}");
    let (_, _, llms) = get(&hub, "/llms.txt").await;
    assert!(llms.contains(&format!("- [Docs]({url}/docs)")), "{llms}");
    for (_, _, body) in [landing, sitemap, llms].map(|b| (0, String::new(), b)) {
        assert!(!body.contains("devagotchi.com/"), "{body}");
    }
}

#[tokio::test]
async fn a_neutral_site_is_at_its_own_domain_on_both_hosts() {
    let hub = neutral().await;
    let url = &hub.url;
    let site = "https://lumen.example.org";
    let (_, _, docs) = get_on(&hub, Some("lumen.example.org"), "/docs").await;
    assert!(docs.contains(&format!("<link rel=\"canonical\" href=\"{site}/docs\">")), "{docs}");
    assert_eq!(meta(&docs, "property", "og:url"), Some(format!("{site}/docs").as_str()));
    let (_, _, landing) = get(&hub, "/").await;
    assert_eq!(json_ld(&landing)[0]["url"], format!("{site}/"));
    let (_, _, sitemap) = get_on(&hub, Some("lumen.example.org"), "/sitemap.xml").await;
    assert!(sitemap.contains(&format!("<loc>{site}/privacy</loc>")), "{sitemap}");
    let (_, _, robots) = get_on(&hub, Some("lumen.example.org"), "/robots.txt").await;
    assert!(robots.contains(&format!("\nSitemap: {site}/sitemap.xml\n")), "{robots}");
    let (_, _, robots) = get(&hub, "/robots.txt").await;
    assert!(robots.contains(&format!("\nSitemap: {url}/sitemap.xml\n")), "{robots}");
    let (_, _, llms) = get_on(&hub, Some("lumen.example.org"), "/llms.txt").await;
    assert!(llms.contains(&format!("[Help]({site}/docs)")), "{llms}");
    assert!(llms.contains(&format!("Connect at `{url}/mcp`")), "{llms}");
    // Nothing of the built-in site, its domain included, on either host.
    for host in [None, Some("lumen.example.org"), Some("devagotchi.com")] {
        for path in ["/", "/docs", "/llms.txt", "/llms-full.txt", "/robots.txt", "/sitemap.xml"] {
            let (_, _, body) = get_on(&hub, host, path).await;
            site::tests::assert_no_brand(&format!("{path} on {host:?}"), &body);
        }
    }
}
