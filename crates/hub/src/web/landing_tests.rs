//! The landing page (`landing.rs`): every section is there for a claimed hub, a new hub
//! offers only the claim, the MCP address is this hub's own, the comparison cites its
//! sources, planned work says so, and nothing in it needs more than the plain pages' CSP.
//! The pages snap under a bar that stays, every term has its tooltip, every popup opens from
//! a button and names itself, and the AI's tool list is the hub's own.

use std::collections::BTreeSet;

use axum::{body::to_bytes, http::header, response::Response};

use super::{
    landing::{TOOLS, page},
    pages::{CSS, Front, home},
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
    for front in [Front::Passkeys, Front::Nomen] {
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
    // The hosted hub makes accounts on first connect: no "coming" left about sign-up.
    assert!(html.contains("your account there is made the first time your AI connects"));
}

#[test]
fn a_hosted_hub_gets_people_started_and_never_offers_a_claim() {
    let html = body(Front::Hosted);
    // Its first button gets you started at the address your AI connects to.
    let first = html.find("class=\"button primary\"").unwrap();
    assert!(html[first..].starts_with("class=\"button primary\" href=\"#connect\">Get started</a>"));
    assert!(html.contains("your account is made the moment your AI connects"));
    assert!(html.contains("Your account is made when it first connects"));
    assert!(html.contains(&format!("<code>{URL}/mcp</code>")));
    // Sign in for those who have an account, and nothing to claim.
    assert!(html.contains("href=\"/auth/login?return_to=/account\">Sign in</a>"));
    for absent in ["href=\"/claim\"", "Claim this hub", "Waiting for its owner"] {
        assert!(!html.contains(absent), "{absent}");
    }
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
fn nomen_hubs_sign_in_with_their_providers_label() {
    let html = body(Front::Nomen);
    assert!(html.contains("href=\"/auth/login?return_to=/account\">Sign in with Shippin</a>"));
    assert!(html.contains("A Shippin account is free."));
    assert!(!html.contains("/claim"));
}

#[tokio::test]
async fn the_page_needs_nothing_the_plain_csp_refuses() {
    for front in [Front::Unclaimed, Front::Passkeys, Front::Nomen, Front::Hosted] {
        let (csp, html) = served(home(front, URL)).await;
        assert!(
            csp.starts_with("default-src 'none'; style-src 'self';") && !csp.contains("script-src"),
            "{csp}"
        );
        assert!(html.contains("<body class=\"landing\">"));
        assert!(html.contains("<link rel=\"stylesheet\" href=\"/assets/hub.css\">"));
        // No script, no inline style, nothing loaded from anywhere but this hub: inline
        // SVG, and the setup video and its poster from /assets/.
        for refused in ["<script", "<style", " style=", "<img", "<iframe"] {
            assert!(!html.contains(refused), "{refused}");
        }
        for src in html.split(" src=\"").skip(1) {
            assert!(src.starts_with("/assets/turnkey.mp4?v="), "{src}");
        }
        for poster in html.split(" poster=\"").skip(1) {
            assert!(
                poster.starts_with("/assets/turnkey-poster.png?v="),
                "{poster}"
            );
        }
        assert!(
            csp.contains("media-src 'self'") && csp.contains("img-src 'self'"),
            "{csp}"
        );
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

#[test]
fn every_term_carries_its_tooltip_and_every_tooltip_is_named_once() {
    for front in [Front::Unclaimed, Front::Passkeys, Front::Nomen, Front::Hosted] {
        let html = body(front);
        let terms: Vec<&str> = html.split("<span class=\"term\" ").skip(1).collect();
        assert!(!terms.is_empty());
        let mut ids = BTreeSet::new();
        for term in terms {
            // Focusable, described by the tooltip that follows it, and that tooltip says so.
            let id = attribute(term, "aria-describedby").unwrap_or_else(|| panic!("{term}"));
            assert!(term.starts_with("tabindex=\"0\""), "{term}");
            assert!(
                term.contains(&format!(
                    "<span class=\"tip\" role=\"tooltip\" id=\"{id}\">"
                )),
                "{term}"
            );
            assert!(ids.insert(id.to_string()), "{id} is used twice");
        }
    }
    // Every glossary word is marked somewhere on the full page.
    let html = body(Front::Passkeys);
    for key in crate::site::builtin().landing.glossary.keys() {
        assert!(
            html.contains(&format!("aria-describedby=\"tip-{key}-")),
            "{key}"
        );
    }
    // The new hub explains its claim code and the passkey, and still opens with the claim.
    let new = body(Front::Unclaimed);
    assert!(new.contains("tip-claim-code-1") && new.contains("tip-passkey-1"));
}

#[test]
fn every_popup_opens_from_a_button_names_itself_and_closes() {
    for front in [Front::Unclaimed, Front::Passkeys, Front::Nomen, Front::Hosted] {
        let html = body(front);
        let popups: Vec<&str> = html.split("<div class=\"modal\" ").skip(1).collect();
        let mut ids = BTreeSet::new();
        for popup in &popups {
            let id = attribute(popup, "id").unwrap();
            assert!(popup.contains(" popover role=\"dialog\""), "{id}");
            assert_eq!(
                attribute(popup, "aria-labelledby"),
                Some(format!("{id}-title").as_str())
            );
            assert!(popup.contains(&format!("<h2 id=\"{id}-title\">")), "{id}");
            assert!(
                popup.contains(&format!(
                    "popovertarget=\"{id}\" popovertargetaction=\"hide\" autofocus>Close</button>"
                )),
                "{id}"
            );
            ids.insert(id.to_string());
        }
        // Each opener points at a popup on the page, and each popup has an opener besides
        // its own Close.
        let targets: BTreeSet<String> = html
            .split("popovertarget=\"")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap().to_string())
            .collect();
        assert_eq!(targets, ids);
        for id in &ids {
            assert!(
                html.contains(&format!("popovertarget=\"{id}\">")),
                "{id} has no opener"
            );
        }
        assert!(
            ids.contains("m-claim") && ids.contains("m-watch"),
            "{ids:?}"
        );
    }
    let full = body(Front::Passkeys);
    assert!(full.contains("id=\"m-tools\""));
    // How claiming works never names the claim page on a hub that has an owner.
    assert!(!full.contains("/claim"));
}

#[test]
fn each_comparison_row_opens_its_evidence_from_the_teardown() {
    let html = body(Front::Passkeys);
    let compare = section(&html, "id=\"compare\"");
    for n in 1..=11 {
        let opener =
            format!("<button type=\"button\" class=\"evidence\" popovertarget=\"ev-{n}\">");
        assert!(compare.contains(&opener), "row {n} opens no evidence");
        let popup = html
            .split(&format!("<div class=\"modal\" id=\"ev-{n}\""))
            .nth(1)
            .and_then(|rest| rest.split("<div class=\"modal\" ").next())
            .unwrap_or_else(|| panic!("no evidence popup {n}"));
        // A file and line of Desktop Commander (the price row cites its pricing page), and
        // the section it is read from.
        let cited = popup.split("<code>").skip(1).any(|code| {
            let code = code.split("</code>").next().unwrap();
            code.rsplit_once(':')
                .is_some_and(|(_, lines)| lines.starts_with(|c: char| c.is_ascii_digit()))
        });
        assert!(cited || popup.contains("pricing page"), "{popup}");
        assert!(popup.contains("c2b8795"), "{popup}");
        assert!(
            popup.contains("/blob/main/docs/desktop-commander-teardown.md#")
                || popup.contains("/blob/main/docs/product-and-platform-plan.md#14-"),
            "{popup}"
        );
    }
    assert!(!html.contains("id=\"ev-12\""));
}

#[test]
fn the_tool_list_is_the_tools_the_hub_serves() {
    let listed: Vec<&str> = TOOLS
        .iter()
        .flat_map(|(_, tools)| tools.iter().flat_map(|(names, _)| names.iter().copied()))
        .collect();
    let unique: BTreeSet<&str> = listed.iter().copied().collect();
    assert_eq!(unique.len(), listed.len(), "a tool is listed twice");
    let served: BTreeSet<String> = crate::hub::tool_names().into_iter().collect();
    let listed: BTreeSet<String> = unique.into_iter().map(str::to_string).collect();
    assert_eq!(listed, served);
    let html = body(Front::Passkeys);
    for name in &served {
        assert!(html.contains(&format!("<code>{name}</code>")), "{name}");
    }
}

#[test]
fn the_rail_names_every_page_in_the_order_they_read() {
    let html = body(Front::Passkeys);
    let rail = html
        .split("<nav class=\"rail\" aria-label=\"On this page\">")
        .nth(1)
        .and_then(|rest| rest.split("</nav>").next())
        .expect("the rail");
    let targets: Vec<&str> = rail
        .split("href=\"#")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap())
        .collect();
    assert_eq!(targets.len(), 9, "{targets:?}");
    let at: Vec<usize> = targets
        .iter()
        .map(|id| {
            html.find(&format!(" id=\"{id}\""))
                .unwrap_or_else(|| panic!("#{id}"))
        })
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{targets:?}");
    // Each page the rail names is lit by its own view timeline in the stylesheet.
    for id in targets {
        assert!(
            CSS.contains(&format!(
                "#{id} {{ view-timeline: --p-{id} block, --page block; }}"
            )),
            "{id}"
        );
        assert!(
            CSS.contains(&format!(
                ".rail a[href=\"#{id}\"] {{ animation-timeline: --p-{id}; }}"
            )),
            "{id}"
        );
    }
    // A new hub has two pages and no rail.
    assert!(!body(Front::Unclaimed).contains("class=\"rail\""));
}

#[test]
fn the_shell_stays_and_motion_is_optional_in_the_stylesheet() {
    // The bar stays at the top of one viewport and keeps still between pages.
    assert!(CSS.contains(".bar { position: sticky; top: 0;"));
    assert!(CSS.contains("view-transition-name: shell-bar;"));
    assert!(CSS.contains("@view-transition { navigation: auto; }"));
    // The landing page's sections snap, and a section taller than the screen still scrolls.
    assert!(CSS.contains("scroll-snap-type: y proximity;"));
    assert!(CSS.contains("scroll-snap-align: start;"));
    assert!(CSS.contains("min-height: calc(100svh - var(--bar-h));"));
    // Page transitions and entrances run only for no-preference, behind feature checks.
    let motion = CSS
        .split("@media (prefers-reduced-motion: no-preference) {")
        .skip(1);
    let gated: String = motion.collect();
    assert!(gated.contains("animation-timeline: --page;"));
    // Page transitions also only with scripting on: in Chrome a page reached by a
    // cross-document view transition with JavaScript off never finishes it, and its layer
    // takes every click.
    let transitions = CSS
        .split("@media (prefers-reduced-motion: no-preference) and (scripting: enabled) {")
        .nth(1)
        .expect("page transitions gated on motion and scripting");
    // (Line ends are the checkout's: CRLF on Windows.)
    assert!(
        transitions
            .trim_start()
            .starts_with("@view-transition { navigation: auto; }")
    );
    assert_eq!(CSS.matches("@view-transition").count(), 1);
    // Smooth scrolling is the landing page's, never an app page's.
    assert!(CSS.contains("html:has(> body.landing) { scroll-behavior: smooth; }"));
    assert!(!CSS.contains("html { scroll-behavior"));
    assert!(CSS.contains("@supports (animation-timeline: view())"));
    // Nothing in it reaches another origin.
    assert!(CSS.split("url(").skip(1).all(|rest| rest.starts_with('#')));
    assert!(!CSS.contains("@import") && !CSS.contains("http"));
}

#[test]
fn the_video_plays_only_when_asked_and_every_id_is_unique() {
    for front in [Front::Unclaimed, Front::Passkeys, Front::Nomen, Front::Hosted] {
        let html = body(front);
        // In the hero (wide screens) and in the Watch it set up popup; click to play.
        let videos: Vec<&str> = html.split("<video ").skip(1).collect();
        assert_eq!(videos.len(), 2);
        for video in videos {
            let tag = video.split('>').next().unwrap();
            for needed in [
                "controls",
                "muted",
                "playsinline",
                "preload=\"none\"",
                "poster=\"/assets/",
            ] {
                assert!(tag.contains(needed), "{needed}: {tag}");
            }
            assert!(!tag.contains("autoplay"), "{tag}");
            assert!(
                video.contains("<source src=\"/assets/turnkey.mp4?v="),
                "{video}"
            );
        }
        assert!(html.contains("<figure class=\"hero-video\">"));
        // The diagram is on the page twice (the hero on a phone, How it works on a wide
        // screen), so its ids are prefixed: no id is used twice.
        let ids: Vec<&str> = html
            .split(" id=\"")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap())
            .collect();
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert!(html.split("url(#").skip(1).all(|rest| {
            let id = rest.split(')').next().unwrap();
            unique.contains(id)
        }));
    }
}

/// The value of `name="…"` in the first tag of `html`.
fn attribute<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let tag = html.split('>').next()?;
    let rest = tag.split(&format!("{name}=\"")).nth(1)?;
    rest.split('"').next()
}

/// The text of the `<section>` that carries `marker`.
fn section<'a>(html: &'a str, marker: &str) -> &'a str {
    let start = html.find(marker).expect(marker);
    let end = html[start..].find("</section>").unwrap();
    &html[start..start + end]
}
