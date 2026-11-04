//! The site (`site.rs`): the built-in one checks out and says what the hub always said; a
//! neutral site (`tests/fixtures/neutral-site`) names nothing of the built-in one anywhere;
//! a directory that does not check out stops the start and says what and where; copy can
//! never add markup of its own.

use std::path::{Path, PathBuf};

use super::{
    Ctx, Front, Site, builtin,
    markdown::{DOC_FRONTS, SWITCHES},
};

/// What the neutral site must never show.
pub(crate) const BRANDS: [&str; 3] = ["Devagotchi", "devagotchi.com", "AngryVibes"];

pub(crate) fn neutral_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/neutral-site")
        .canonicalize()
        .expect("tests/fixtures/neutral-site")
}

pub(crate) fn neutral() -> Site {
    Site::load(&neutral_dir()).expect("the neutral site checks out")
}

pub(crate) fn assert_no_brand(what: &str, text: &str) {
    for brand in BRANDS {
        assert!(!text.contains(brand), "{what} names {brand}:\n{text}");
    }
}

/// Every surface a site draws, as text.
fn everything(site: &Site) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for front in [
        Front::Unclaimed,
        Front::Passkeys,
        Front::Nomen,
        Front::Hosted,
    ] {
        let ctx = Ctx::new(site);
        let (title, body) = crate::web::landing_for(site, &ctx, front, "https://hub.example.org");
        out.push((
            format!("landing {}", front.key()),
            format!("{title}\n{body}"),
        ));
    }
    for (name, page) in &site.pages {
        for front in DOC_FRONTS {
            for switch in SWITCHES {
                let ctx = Ctx::new(site)
                    .with("mcp", "https://hub.example.org/mcp")
                    .with("updated", page.updated.clone())
                    .with("retention_days", "90");
                out.push((
                    format!("{name} ({front}, {switch})"),
                    format!("{}\n{}", page.title, page.render(&ctx, &[front, switch])),
                ));
            }
        }
    }
    let (subject, body) = site.sign_in_email("https://hub.example.org/auth/email#t");
    out.push(("email".into(), format!("{subject}\n{body}")));
    let (title, steps) = site.welcome("https://hub.example.org");
    out.push(("welcome".into(), format!("{title}\n{steps}")));
    // What early access and plans say: to an AI, on the account page, in the email.
    let access = &site.messages.access;
    out.push((
        "access".into(),
        format!(
            "{}\n{}\n{}\n{}\n{}",
            site.waiting_message("1 January 2026", "https://hub.example.org/account"),
            access.waiting_title,
            Ctx::new(site).with("since", "x").inline(&access.account_waiting),
            Ctx::new(site)
                .with("since", "x")
                .with("plan", "y")
                .inline(&access.account_admitted),
            access.limit_title,
        ),
    ));
    if let Some(plan) = site.default_plan() {
        out.push((
            "limit".into(),
            site.limit_message(plan, "projects", 1, 1, "https://hub.example.org"),
        ));
    }
    let (subject, body) = site.admitted_email("https://hub.example.org/account");
    out.push(("admitted email".into(), format!("{subject}\n{body}")));
    let info = site.mcp_info();
    out.push((
        "mcp".into(),
        format!(
            "{} {:?}\n{}",
            info.name,
            info.title,
            site.mcp_instructions("https://hub.example.org")
        ),
    ));
    // What search engines and AI agents read (`discovery.rs`).
    let url = "https://hub.example.org";
    for kind in DOC_FRONTS {
        out.push((
            format!("llms.txt ({kind})"),
            super::discovery::llms_txt(site, url, kind),
        ));
        for switch in SWITCHES {
            out.push((
                format!("llms-full.txt ({kind}, {switch})"),
                super::discovery::llms_full_txt(site, url, kind, switch, 90),
            ));
        }
    }
    out.push(("robots.txt".into(), super::discovery::robots_txt(site, url)));
    out.push((
        "sitemap.xml".into(),
        super::discovery::sitemap_xml(site, url, true),
    ));
    for front in [
        Front::Unclaimed,
        Front::Passkeys,
        Front::Nomen,
        Front::Hosted,
    ] {
        let meta = super::discovery::Meta {
            path: "/",
            title: &site.info.product.tagline,
            description: "",
            noindex: false,
            json_ld: super::discovery::landing_json_ld(site, front, url),
        };
        out.push((
            format!("landing head ({})", front.key()),
            super::discovery::head(site, url, &meta),
        ));
    }
    out.push(("manifest".into(), site.manifest()));
    out.push(("service worker".into(), site.service_worker()));
    out.push(("stylesheet".into(), site.stylesheet().to_string()));
    for asset in site.assets.values().filter(|a| a.extension() == "svg") {
        out.push((asset.name.clone(), asset.text()));
    }
    out
}

#[test]
fn the_built_in_site_checks_out_and_says_what_the_hub_always_said() {
    let site = builtin();
    assert!(site.dir.is_none());
    assert_eq!(site.name(), "Devagotchi");
    assert_eq!(site.info.operator.name, "AngryVibes LLC");
    assert_eq!(site.mcp_info().name, "devagotchi-hub");
    assert!(site.mcp_info().title.is_none());
    assert!(
        site.mcp_instructions("https://hub.example.org")
            .starts_with("Devagotchi gives you the user's own machines (servers, PCs and devspaces) through this hub at https://hub.example.org. Docs: https://hub.example.org/docs; for AI agents: https://hub.example.org/llms.txt.")
    );
    assert_eq!(site.hosted_hub(), Some("https://app.devagotchi.com"));
    let (subject, body) = site.sign_in_email("https://hub.example.org/auth/email#t");
    assert_eq!(subject, "Your Devagotchi sign-in link");
    assert!(body.starts_with(
        "Open this link to sign in to Devagotchi:\n\nhttps://hub.example.org/auth/email#t\n\n"
    ));
    // The Plans section is there for the operator to turn on: the Free plan only, no price
    // anyone made up, and off.
    let plans: Vec<&str> = site.info.plans.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(plans, ["Free"]);
    assert_eq!(site.info.plans[0].price, "Free");
    assert!(
        site.info
            .landing
            .sections
            .iter()
            .any(|s| s.id == "plans" && !s.on)
    );
    let ctx = Ctx::new(&site);
    let (_, page) =
        crate::web::landing_for(&site, &ctx, Front::Passkeys, "https://hub.example.org");
    assert!(!page.contains("id=\"plans\""));
    // Every asset it names is served under its hash; nothing else is.
    let names: Vec<&str> = site.assets.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "diagram.svg",
            "icon.svg",
            "logo.svg",
            "pet.svg",
            "turnkey-poster.png",
            "turnkey.mp4"
        ]
    );
    for asset in site.assets.values() {
        assert_eq!(
            asset.url(),
            format!("/assets/{}?v={}", asset.name, asset.version)
        );
    }
}

#[test]
fn a_neutral_site_names_nothing_of_the_built_in_one_anywhere() {
    let site = neutral();
    for (what, text) in everything(&site) {
        assert_no_brand(&what, &text);
    }
    // And it is its own: its name, its operator, its links and its sign-in label.
    let ctx = Ctx::new(&site);
    let (title, page) =
        crate::web::landing_for(&site, &ctx, Front::Nomen, "https://hub.example.org");
    assert_eq!(title, "A relay between your assistant and your computers");
    assert!(page.contains("Log in with Example ID"), "{page}");
    assert!(page.contains("Run by Example Operator Ltd."), "{page}");
    assert!(
        page.contains("href=\"https://git.example.org/lumen\""),
        "{page}"
    );
    // Its own section, its plans and services, in the order site.json gives.
    let at = |needle: &str| page.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("id=\"how\"") < at("id=\"plans\""));
    assert!(at("id=\"plans\"") < at("id=\"services\""));
    assert!(at("id=\"services\"") < at("id=\"why\""));
    assert!(at("id=\"why\"") < at("id=\"features\""));
    assert!(!page.contains("id=\"compare\""), "compare is off");
    assert!(page.contains("<h2 id=\"why-title\">Why Lumen Relay</h2>"));
    assert!(page.contains("<li>2 computers</li>") && page.contains("<li>Every project</li>"));
    assert!(page.contains("<p class=\"price\">Ask us</p>"));
    assert!(page.contains("Sign-up opens in waves."));
    assert!(page.contains("<a href=\"https://relay.lumen.example.org\">Open it</a>"));
    // The rail and its view timelines follow the sections that show.
    for id in [
        "top", "how", "plans", "services", "why", "features", "connect",
    ] {
        assert!(page.contains(&format!("<a href=\"#{id}\">")), "{id}");
        assert!(
            site.stylesheet().contains(&format!(
                "#{id} {{ view-timeline: --p-{id} block, --page block; }}"
            )),
            "{id}"
        );
    }
    assert!(!site.stylesheet().contains("--p-compare"));
    // No video: no Watch it set up and no popup for it.
    assert!(!page.contains("<video") && !page.contains("m-watch"));
    // Its MCP display.
    assert_eq!(site.mcp_info().name, "lumen-relay");
    assert_eq!(site.mcp_info().title.as_deref(), Some("Lumen Relay"));
    // Only the assets it names are served: none of the built-in ones.
    let names: Vec<&str> = site.assets.keys().map(String::as_str).collect();
    assert_eq!(names, ["diagram.svg", "icon.svg", "logo.svg"]);
}

/// A site made from the neutral one, changed by `edit`, that checks out.
pub(crate) fn neutral_with(edit: impl FnOnce(&Path)) -> Site {
    let (_dir, loaded) = variant(edit);
    loaded.expect("the edited neutral site checks out")
}

#[test]
fn plans_say_which_one_a_tenant_starts_on_and_what_each_holds() {
    use super::{Cap, SignUp};
    // The neutral site: two plans, none marked default, early access on.
    let site = neutral();
    assert_eq!(site.sign_up(), SignUp::EarlyAccess);
    assert_eq!(site.default_plan().unwrap().id, "free", "the first plan");
    let team = site.plan("team").unwrap();
    assert_eq!(team.cap("projects"), Cap::Unlimited);
    assert_eq!(team.cap("clients"), Cap::AtMost(10));
    assert_eq!(team.cap("machines"), Cap::Unset);
    // Marked default: that one, whatever its place.
    let site = neutral_with(|d| {
        edit_json(&d.join("site.json"), |v| v["plans"][1]["default"] = true.into())
    });
    assert_eq!(site.default_plan().unwrap().id, "team");
    // A plan id the site no longer lists reads as the default plan, and says it fell back.
    let (plan, fell_back) = site.plan_for(Some("gone"));
    assert_eq!((plan.unwrap().id.as_str(), fell_back), ("team", true));
    let (plan, fell_back) = site.plan_for(Some("free"));
    assert_eq!((plan.unwrap().id.as_str(), fell_back), ("free", false));
    let (plan, fell_back) = site.plan_for(None);
    assert_eq!((plan.unwrap().id.as_str(), fell_back), ("team", false));
    // No plans: nothing holds a tenant.
    let none = neutral_with(|d| edit_json(&d.join("site.json"), |v| v["plans"] = serde_json::json!([])));
    assert!(none.default_plan().is_none() && none.plan_for(Some("free")).0.is_none());
    // The limit message in the site's words: the plan, the limit, the count, the plans.
    let message = neutral().limit_message(
        neutral().plan("free").unwrap(),
        "machines",
        2,
        2,
        "https://hub.example.org/",
    );
    assert!(
        message.contains("Starter")
            && message.contains("2 computers")
            && message.contains("https://hub.example.org/#plans"),
        "{message}"
    );
    // links.plans, when the site names one, is where it points instead.
    let linked = neutral_with(|d| {
        edit_json(&d.join("site.json"), |v| {
            v["links"]["plans"] = "https://lumen.example.org/pricing".into()
        })
    });
    assert_eq!(
        linked.plans_url("https://hub.example.org"),
        "https://lumen.example.org/pricing"
    );
    // Words an earlier site gave a limit the hub no longer counts still load, unused.
    let older = neutral_with(|d| {
        edit_json(&d.join("content/landing.json"), |v| {
            v["plans"]["limits"]["idle_days"] = serde_json::json!({
                "one": "{n} day", "other": "{n} days", "unlimited": "Forever"
            })
        })
    });
    assert!(older.landing.plans.limits.contains_key("idle_days"));
}

#[test]
fn tokens_and_messages_give_only_what_they_change() {
    let site = neutral();
    let css = site.stylesheet();
    // Its accent and one radius; every other token is the built-in one.
    assert!(css.contains("--accent: #7a3cff;"), "{css}");
    assert!(css.contains("--radius-m: 4px;"));
    assert!(css.contains("--bg: #f3f5f1;"));
    assert!(css.contains("--radius-l: 12px;"));
    assert_eq!(site.theme_color(), "#7a3cff");
    // Its email; the page footer it left out is the built-in one, in its words.
    assert_eq!(site.sign_in_email("L").0, "Your Lumen Relay link");
    assert!(
        site.messages
            .page_footer
            .starts_with("Last updated {updated}.")
    );
    assert!(!site.has_fonts());
}

/// A site directory made from the neutral one, changed by `edit`.
fn variant(edit: impl FnOnce(&Path)) -> (tempfile::TempDir, Result<Site, String>) {
    let dir = tempfile::tempdir().unwrap();
    copy(&neutral_dir(), dir.path());
    edit(dir.path());
    let loaded = Site::load(dir.path());
    (dir, loaded)
}

pub(crate) fn copy(from: &Path, to: &Path) {
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

fn edit_json(path: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    change(&mut value);
    std::fs::write(path, value.to_string()).unwrap();
}

type Edit = Box<dyn FnOnce(&Path)>;

#[test]
fn a_site_that_does_not_check_out_says_what_and_where() {
    let cases: Vec<(Edit, &[&str])> = vec![
        (
            Box::new(|d| std::fs::remove_file(d.join("site.json")).unwrap()),
            &["has no site.json"],
        ),
        (
            Box::new(|d| std::fs::write(d.join("site.json"), "{ not json").unwrap()),
            &["site.json:"],
        ),
        (
            Box::new(|d| edit_json(&d.join("site.json"), |v| v["colour"] = "red".into())),
            &["site.json:", "unknown field `colour`"],
        ),
        (
            Box::new(|d| edit_json(&d.join("site.json"), |v| v["product"]["name"] = "".into())),
            &["site.json product.name"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("site.json"), |v| {
                    v["product"]["domain"] = "https://lumen.example.org/".into()
                })
            }),
            &["site.json product.domain", "a host name"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("site.json"), |v| {
                    v["links"]["source"] = "javascript:alert(1)".into()
                })
            }),
            &["site.json links.source"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("site.json"), |v| {
                    v["landing"]["sections"][0]["id"] = "nowhere".into()
                })
            }),
            &["landing.sections", "nowhere"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("site.json"), |v| {
                    v["plans"][0]["limits"]["seats"] = 3.into()
                })
            }),
            &[
                "plans free.limits.seats",
                "the hub enforces only projects, machines, clients",
            ],
        ),
        (
            // A limit the hub does not count may not be claimed, though it once was.
            Box::new(|d| {
                edit_json(&d.join("site.json"), |v| {
                    v["plans"][0]["limits"]["idle_days"] = 14.into()
                })
            }),
            &["plans free.limits.idle_days", "the hub enforces only"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("site.json"), |v| {
                    v["plans"][0]["default"] = true.into();
                    v["plans"][1]["default"] = true.into();
                })
            }),
            &["site.json plans", "at most one plan is marked"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("content/messages.json"), |v| {
                    v["access"] = serde_json::json!({ "waiting": "Wait for {nobody}" })
                })
            }),
            &["content/messages.json access.waiting", "nothing fills {nobody}"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("site.json"), |v| {
                    v["assets"]["video"] = "film.mp4".into()
                })
            }),
            &["assets.video", "no assets/film.mp4"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("content/landing.json"), |v| {
                    v["hero"]["title"] = "Hello {nobody}".into()
                })
            }),
            &["content/landing.json", "nothing fills {nobody}"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("content/landing.json"), |v| {
                    v["faq"]["questions"][0]["a"] = "[click](javascript:alert(1))".into()
                })
            }),
            &["content/landing.json", "javascript:alert(1)"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("content/landing.json"), |v| {
                    v["hero"]["note"]["default"] = "{term:nope}".into()
                })
            }),
            &["nothing fills {term:nope}"],
        ),
        (
            Box::new(|d| std::fs::write(d.join("content/terms.md"), "# Terms\n").unwrap()),
            &["content/terms.md", "front matter"],
        ),
        (
            Box::new(|d| {
                std::fs::write(
                    d.join("content/docs.md"),
                    "---\ntitle: Docs\nupdated: x\n---\n::: everyone\nhi\n:::\n",
                )
                .unwrap()
            }),
            &["content/docs.md", "::: everyone"],
        ),
        (
            Box::new(|d| std::fs::write(d.join("content/privacy.markdown"), "x").unwrap()),
            &["content/privacy.markdown"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("tokens.json"), |v| {
                    v["colors"]["acent"] = "red".into()
                })
            }),
            &["tokens.json colors.acent", "no such token"],
        ),
        (
            Box::new(|d| {
                edit_json(&d.join("tokens.json"), |v| {
                    v["colors"]["accent"] = "red; } body { display: none".into()
                })
            }),
            &["tokens.json colors.accent", "not a plain CSS value"],
        ),
        (
            Box::new(|d| {
                std::fs::write(
                    d.join("assets/logo.svg"),
                    "<svg onload=\"alert(1)\"><circle r=\"1\"/></svg>",
                )
                .unwrap()
            }),
            &["assets/logo.svg", "event handlers"],
        ),
        (
            Box::new(|d| {
                std::fs::write(
                    d.join("assets/logo.svg"),
                    "<svg><image href=\"https://elsewhere.example/x.png\"/></svg>",
                )
                .unwrap()
            }),
            &["assets/logo.svg", "never to another file"],
        ),
        (
            Box::new(|d| {
                std::fs::write(
                    d.join("assets/logo.svg"),
                    "<svg><style>circle{fill:red}</style></svg>",
                )
                .unwrap()
            }),
            &["assets/logo.svg", "inline styles"],
        ),
        (
            Box::new(|d| std::fs::write(d.join("assets/page.html"), "<p>hi</p>").unwrap()),
            &["assets/page.html", "an asset is one of"],
        ),
        (
            Box::new(|d| std::fs::write(d.join("assets/hub.css"), "body{}").unwrap()),
            &["assets/hub.css"],
        ),
        (
            Box::new(|d| std::fs::write(d.join("assets/icon.svg"), "not an svg").unwrap()),
            &["assets/icon.svg"],
        ),
    ];
    for (edit, says) in cases {
        let (_dir, loaded) = variant(edit);
        let why = loaded.err().expect("refused");
        for part in says {
            assert!(why.contains(part), "{part:?} not in {why:?}");
        }
    }
    // A relative path, and no directory at all.
    assert!(
        Site::load(Path::new("relative/site"))
            .unwrap_err()
            .contains("not an absolute path")
    );
    assert!(Site::load(Path::new("/nonexistent/site/dir")).is_err());
}

#[test]
fn a_site_may_serve_its_own_fonts_from_this_hub() {
    let (_dir, loaded) = variant(|d| {
        let mut font = b"wOF2".to_vec();
        font.extend_from_slice(&[0; 60]);
        std::fs::write(d.join("assets/body.woff2"), font).unwrap();
        edit_json(&d.join("tokens.json"), |v| {
            v["font_files"] = serde_json::json!([
                { "family": "Body Sans", "file": "body.woff2", "weight": "100 900" }
            ]);
            v["fonts"] = serde_json::json!({ "ui": "\"Body Sans\", system-ui, sans-serif" });
        });
    });
    let site = loaded.unwrap();
    assert!(site.has_fonts());
    let font = &site.assets["body.woff2"];
    assert_eq!(font.mime, "font/woff2");
    let css = site.stylesheet();
    assert!(
        css.contains(&format!(
            "@font-face {{ font-family: \"Body Sans\"; src: url(/assets/body.woff2?v={}) format(\"woff2\");",
            font.version
        )),
        "{css}"
    );
    assert!(css.contains("--font-ui: \"Body Sans\", system-ui, sans-serif;"));
    // A font that is not one, or names a file the site does not have, is refused.
    let (_dir, loaded) = variant(|d| {
        std::fs::write(d.join("assets/body.woff2"), b"not a font").unwrap();
    });
    assert!(loaded.unwrap_err().contains("assets/body.woff2"));
    let (_dir, loaded) = variant(|d| {
        edit_json(&d.join("tokens.json"), |v| {
            v["font_files"] = serde_json::json!([{ "family": "X", "file": "gone.woff2" }]);
        });
    });
    assert!(loaded.unwrap_err().contains("no assets/gone.woff2"));
}

#[test]
fn copy_carries_its_markup_and_never_any_html_of_its_own() {
    let site = builtin();
    let ctx = Ctx::new(&site).with("mcp", "https://hub.example.org/mcp");
    assert_eq!(
        ctx.inline("**Bold** and *em*, `code {mcp}` and [a link](/docs)."),
        "<strong>Bold</strong> and <em>em</em>, <code>code https://hub.example.org/mcp</code> \
and <a href=\"/docs\">a link</a>."
    );
    // HTML in copy is text; an entity stays; a bare & is escaped.
    assert_eq!(
        ctx.inline("<script>alert(1)</script> &quot;x&quot; & y"),
        "&lt;script&gt;alert(1)&lt;/script&gt; &quot;x&quot; &amp; y"
    );
    // A value from site.json is text too, wherever it lands.
    assert_eq!(
        ctx.inline("\\{product} is {product}"),
        "{product} is Devagotchi"
    );
    // A link may only go to this hub, an anchor, https or mail.
    let checking = Ctx::checking(&site);
    assert_eq!(checking.inline("[x](javascript:alert(1))"), "x");
    assert_eq!(checking.errors().len(), 1);
    // Tooltips are numbered per page, so every id is unique.
    let ctx = Ctx::new(&site);
    let twice = ctx.inline("{term:mcp} and {term:mcp|the protocol}");
    assert!(twice.contains("id=\"tip-mcp-1\"") && twice.contains("id=\"tip-mcp-2\""));
    assert!(twice.contains(">the protocol<span class=\"tip\""));
}

#[test]
fn pages_are_a_small_strict_markdown() {
    let page = super::Page::parse(
        "---\ntitle: T\nupdated: today\n---\n# Head {product}\n\nOne line\nand the next.\n\n\
- **a** first\n  more of it\n- second\n\n1. one\n2. two\n\n| A | B |\n|---|---|\n| `x` | y |\n\n\
::: nomen\nOnly on Nomen hubs.\n:::\n::: passkeys hosted\nOnly with passkeys.\n:::\n",
    )
    .unwrap();
    assert_eq!(page.title, "T");
    assert_eq!(page.updated, "today");
    let site = builtin();
    let ctx = Ctx::new(&site);
    let html = page.render(&ctx, &["passkeys"]);
    assert_eq!(
        html,
        "<h1>Head Devagotchi</h1><p>One line and the next.</p>\
<ul><li><strong>a</strong> first more of it</li><li>second</li></ul>\
<ol><li>one</li><li>two</li></ol>\
<table><thead><tr><th>A</th><th>B</th></tr></thead><tbody><tr><td><code>x</code></td><td>y</td></tr></tbody></table>\
<p>Only with passkeys.</p>"
    );
    assert!(
        page.render(&ctx, &["nomen"])
            .contains("<p>Only on Nomen hubs.</p>")
    );
    assert!(!page.render(&ctx, &["other"]).contains("Only"));
}

#[test]
fn a_page_says_what_the_hub_counts_only_where_it_counts() {
    let page = super::Page::parse(
        "---\ntitle: T\nupdated: today\n---\n::: analytics\nCounted, {retention_days} days.\n:::\n\n\
::: no-analytics\nNothing counted.\n:::\n\n::: hosted\n::: analytics\nHosted and counted.\n:::\n:::\n",
    )
    .unwrap();
    let site = builtin();
    let ctx = Ctx::new(&site).with("retention_days", "30");
    assert_eq!(
        page.render(&ctx, &["hosted", "analytics"]),
        "<p>Counted, 30 days.</p><p>Hosted and counted.</p>"
    );
    assert_eq!(page.render(&ctx, &["hosted", "no-analytics"]), "<p>Nothing counted.</p>");
    assert_eq!(page.render(&ctx, &["passkeys", "analytics"]), "<p>Counted, 30 days.</p>");
    let unknown = super::Page::parse("---\ntitle: T\n---\n::: counted\nx\n:::\n").unwrap_err();
    assert!(unknown.contains("analytics, no-analytics"), "{unknown}");
    // The built-in privacy page says exactly what a counting hub counts, and a hub that
    // does not count says it counts nothing.
    let privacy = &site.pages["privacy"];
    let ctx = Ctx::new(&site)
        .with("retention_days", "45")
        .with("updated", privacy.updated.clone());
    let counting = privacy.render(&ctx, &["hosted", "analytics"]);
    for said in [
        "no cookie",
        "no script",
        "<code>utm_source</code>",
        "changes every day",
        "never kept",
        "Global Privacy Control",
        "Do Not Track",
        "45 days",
        "once for each account",
    ] {
        assert!(counting.contains(said), "{said}: {counting}");
    }
    assert!(!counting.contains("no analytics"), "{counting}");
    let quiet = privacy.render(&ctx, &["hosted", "no-analytics"]);
    assert!(quiet.contains("no analytics"), "{quiet}");
    assert!(!quiet.contains("45 days"), "{quiet}");
}

#[test]
fn the_hubs_config_names_its_site_and_the_environment_can_too() {
    let dir = neutral_dir();
    let config = |site: serde_json::Value| {
        serde_json::json!({
            "listen": "127.0.0.1:1",
            "public_url": "http://127.0.0.1:1",
            "auth": { "mode": "hub" },
            "brokers": [],
            "grants": [],
            "site": site,
        })
        .to_string()
    };
    let named = crate::config::HubConfig::parse(&config(serde_json::json!({ "dir": dir })))
        .expect("a hub with the neutral site");
    assert_eq!(named.site().name(), "Lumen Relay");
    // No directory: the built-in site.
    let plain = crate::config::HubConfig::parse(&config(serde_json::json!({}))).unwrap();
    assert_eq!(plain.site().name(), "Devagotchi");
    // DEVAGOTCHI_SITE_DIR over the file.
    let env = |name: &str| (name == super::SITE_ENV).then(|| dir.display().to_string());
    let from_env =
        crate::config::HubConfig::parse_with(&config(serde_json::json!({})), env).unwrap();
    assert_eq!(from_env.site().name(), "Lumen Relay");
    // A bad directory stops the start, saying which and why.
    let error =
        crate::config::HubConfig::parse(&config(serde_json::json!({ "dir": "/nonexistent/site" })))
            .unwrap_err();
    assert_eq!(error.code, "invalid_config");
    assert!(
        error.message.starts_with("site.dir /nonexistent/site: "),
        "{}",
        error.message
    );
    let error = crate::config::HubConfig::parse(&config(
        serde_json::json!({ "dir": "/tmp", "theme": "x" }),
    ))
    .unwrap_err();
    assert!(
        error.message.contains("unknown field `theme`"),
        "{}",
        error.message
    );
}

#[test]
fn a_running_hub_follows_its_site_directory_and_keeps_a_good_site_over_a_bad_one() {
    let dir = tempfile::tempdir().unwrap();
    copy(&neutral_dir(), dir.path());
    let cell = std::sync::Arc::new(super::SiteCell::new(std::sync::Arc::new(
        Site::load(dir.path()).unwrap(),
    )));
    super::watch(
        cell.clone(),
        dir.path().to_path_buf(),
        std::time::Duration::from_millis(100),
    );
    let wait_for = |name: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while cell.get().name() != name {
            assert!(
                std::time::Instant::now() < deadline,
                "still {}",
                cell.get().name()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    // A change is served once it checks out.
    edit_json(&dir.path().join("site.json"), |v| {
        v["product"]["name"] = "Lumen Relay Two".into()
    });
    wait_for("Lumen Relay Two");
    // One that does not check out is refused, and the site it had stays.
    std::fs::write(dir.path().join("site.json"), "{ broken").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(cell.get().name(), "Lumen Relay Two");
    // Fixed again, it is served again.
    std::fs::copy(
        neutral_dir().join("site.json"),
        dir.path().join("site.json"),
    )
    .unwrap();
    wait_for("Lumen Relay");
}

#[test]
fn check_site_says_whether_a_directory_checks_out() {
    let dir = neutral_dir();
    let command = crate::parse_args(["check-site".into(), dir.clone().into_os_string()]).unwrap();
    assert!(crate::manage(command).is_ok());
    let bad = tempfile::tempdir().unwrap();
    copy(&dir, bad.path());
    std::fs::write(
        bad.path().join("tokens.json"),
        r#"{"colors": {"acent": "red"}}"#,
    )
    .unwrap();
    let command =
        crate::parse_args(["check-site".into(), bad.path().as_os_str().to_owned()]).unwrap();
    let error = crate::manage(command).unwrap_err();
    assert_eq!(error.code, "invalid_site");
    assert!(
        error.message.contains("tokens.json colors.acent"),
        "{}",
        error.message
    );
    for args in [vec!["check-site"], vec!["check-site", "relative/site"]] {
        let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        assert!(crate::parse_args(args).is_err());
    }
}
