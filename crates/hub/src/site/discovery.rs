//! What search engines and AI agents read about a hub, made from its site and its public
//! URL, never written into the code: `/llms.txt` (the llmstxt.org format: `content/llms.md`)
//! and `/llms-full.txt` (that, with every public page inlined as Markdown), `/robots.txt`,
//! `/sitemap.xml`, and the head of every public page: its description, its canonical link,
//! its Open Graph and Twitter cards and, on the landing page, its JSON-LD
//! (`SoftwareApplication`, `FAQPage` for the questions it shows, `HowTo` for its steps).
//! Every address in them is this hub's own `public_url`.

use serde_json::{Value, json};

use super::{Front, Site, text::Ctx, text::attr};

/// The public pages besides `/`, in the order `/llms-full.txt` inlines them.
pub const PAGES: [&str; 4] = ["docs", "support", "privacy", "terms"];

/// What no crawler reads: accounts, the operator, setup links, sign-in and OAuth, the API,
/// joining and downloads, and the MCP and link endpoints.
pub const PRIVATE: [&str; 19] = [
    "/account",
    "/operator",
    "/go/",
    "/oauth",
    "/api",
    "/join",
    "/dist",
    "/auth",
    "/approvals",
    "/claim",
    "/device",
    "/owner",
    "/passkey",
    "/machines",
    "/push",
    "/signed-out",
    "/mcp",
    "/link",
    "/enroll",
];

fn base(public_url: &str) -> &str {
    public_url.trim_end_matches('/')
}

/// `ctx` with what `content/llms.md` names: this hub's address, its MCP address, and the
/// one line that joins a machine on each system (with `<code>` where `machine_add`'s code
/// goes).
pub fn llms_ctx<'a>(ctx: Ctx<'a>, public_url: &str) -> Ctx<'a> {
    let hub = base(public_url);
    let join = |os: &str| crate::hub::join_line(hub, os, "<code>");
    ctx.with("hub", hub)
        .with("mcp", format!("{hub}/mcp"))
        .with("join_linux", join("linux"))
        .with("join_windows", join("windows"))
        .with("join_docker", join("docker"))
}

/// `ctx` with what the MCP instructions name: this hub, its docs and its `/llms.txt`.
pub fn instructions_ctx<'a>(ctx: Ctx<'a>, public_url: &str) -> Ctx<'a> {
    let hub = base(public_url);
    ctx.with("hub", hub)
        .with("docs", format!("{hub}/docs"))
        .with("llms", format!("{hub}/llms.txt"))
}

/// `/llms.txt` on a hub of `kind` (`passkeys`, `hosted`, `nomen` or `other`).
pub fn llms_txt(site: &Site, public_url: &str, kind: &str) -> String {
    site.llms
        .markdown(&llms_ctx(Ctx::new(site), public_url), &[kind])
}

/// `/llms-full.txt`: `/llms.txt`, then each public page as this hub shows it (`switch`:
/// `analytics` or `no-analytics`), under its own address.
pub fn llms_full_txt(
    site: &Site,
    public_url: &str,
    kind: &str,
    switch: &str,
    retention_days: u32,
) -> String {
    let hub = base(public_url);
    let mut out = llms_txt(site, public_url, kind);
    for name in PAGES {
        let Some(page) = site.pages.get(name) else {
            continue;
        };
        let ctx = Ctx::new(site)
            .with("mcp", format!("{hub}/mcp"))
            .with("updated", page.updated.clone())
            .with("retention_days", retention_days.to_string());
        out.push_str(&format!("\n---\n\n<{hub}/{name}>\n\n"));
        out.push_str(&page.markdown(&ctx, &[kind, switch]));
    }
    out
}

/// `/robots.txt`: every crawler, AI crawlers included, may read the public pages; none
/// reads the private paths. It names the sitemap.
pub fn robots_txt(site: &Site, public_url: &str) -> String {
    let hub = base(public_url);
    let mut out = format!("# {}: {hub}\n# {hub}/llms.txt\n\nUser-agent: *\nAllow: /\n", site.name());
    for path in PRIVATE {
        out.push_str(&format!("Disallow: {path}\n"));
    }
    out.push_str(&format!("\nSitemap: {hub}/sitemap.xml\n"));
    out
}

/// `/sitemap.xml`: the public pages, with the date each was last changed where it says.
/// `landing`: whether this hub serves a landing page at `/` (one with no web sign-in sends
/// `/` to its docs).
pub fn sitemap_xml(site: &Site, public_url: &str, landing: bool) -> String {
    let hub = base(public_url);
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    if landing {
        out.push_str(&format!("  <url><loc>{}/</loc></url>\n", xml(hub)));
    }
    for name in super::PAGES {
        let Some(page) = site.pages.get(name) else {
            continue;
        };
        let lastmod = iso_date(&page.updated)
            .map(|date| format!("<lastmod>{date}</lastmod>"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  <url><loc>{}/{name}</loc>{lastmod}</url>\n",
            xml(hub)
        ));
    }
    out.push_str("</urlset>\n");
    out
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `1 November 2026` as `2026-11-01`; `None` for anything else.
pub fn iso_date(text: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let mut words = text.split_whitespace();
    let day: u32 = words.next()?.parse().ok()?;
    let month = words.next()?.to_lowercase();
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    let year: u32 = words.next()?.parse().ok()?;
    if words.next().is_some() || !(1..=31).contains(&day) || !(1000..=9999).contains(&year) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// What one public page tells search engines and link previews about itself.
pub struct Meta<'a> {
    /// Its path on the hub: `/`, `/docs`…
    pub path: &'a str,
    pub title: &'a str,
    /// Its own description, or empty for the product's.
    pub description: &'a str,
    /// Keep it out of search results (a hub nobody owns yet).
    pub noindex: bool,
    pub json_ld: Vec<Value>,
}

/// The head of a public page: its description, canonical link, Open Graph and Twitter
/// cards (with the site's poster as their picture, when it has one) and its JSON-LD. The
/// JSON-LD is data, never run: the pages' CSP has no `script-src` and needs none for it.
pub fn head(site: &Site, public_url: &str, meta: &Meta) -> String {
    let hub = base(public_url);
    let url = format!("{hub}{}", meta.path);
    let title = format!("{} · {}", meta.title, site.name());
    let description = if meta.description.is_empty() {
        site.info.product.description.as_str()
    } else {
        meta.description
    };
    let mut out = String::new();
    let mut tag = |kind: &str, key: &str, value: &str| {
        if !value.is_empty() {
            out.push_str(&format!(
                "<meta {kind}=\"{key}\" content=\"{}\">",
                attr(value)
            ));
        }
    };
    tag("name", "description", description);
    if meta.noindex {
        tag("name", "robots", "noindex");
    }
    tag("property", "og:type", "website");
    tag("property", "og:site_name", site.name());
    tag("property", "og:title", &title);
    tag("property", "og:description", description);
    tag("property", "og:url", &url);
    let picture = site.named(site.info.assets.poster.as_ref());
    if let Some(poster) = picture {
        tag("property", "og:image", &format!("{hub}{}", poster.url()));
        tag("property", "og:image:type", poster.mime);
        if let Some((width, height)) = png_size(&poster.bytes) {
            tag("property", "og:image:width", &width.to_string());
            tag("property", "og:image:height", &height.to_string());
        }
    }
    tag(
        "name",
        "twitter:card",
        if picture.is_some() {
            "summary_large_image"
        } else {
            "summary"
        },
    );
    tag("name", "twitter:title", &title);
    tag("name", "twitter:description", description);
    if let Some(poster) = picture {
        tag("name", "twitter:image", &format!("{hub}{}", poster.url()));
    }
    out.push_str(&format!("<link rel=\"canonical\" href=\"{}\">", attr(&url)));
    for data in &meta.json_ld {
        out.push_str(&format!(
            "<script type=\"application/ld+json\">{}</script>",
            json_in_html(data)
        ));
    }
    out
}

/// JSON safe inside `<script>`: no `<`, `>` or `&` that could end it or open a comment.
fn json_in_html(value: &Value) -> String {
    value
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// A PNG's width and height, from its header.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let word = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((word(16), word(20)))
}

/// The landing page's JSON-LD for `front`: the software always; its questions and its
/// steps only where the page shows them (a new hub shows only the claim).
pub fn landing_json_ld(site: &Site, front: Front, public_url: &str) -> Vec<Value> {
    let hub = base(public_url);
    let ctx = Ctx::new(site).with("mcp", format!("{hub}/mcp"));
    let info = &site.info;
    let mut software = json!({
        "@context": "https://schema.org",
        "@type": "SoftwareApplication",
        "name": info.product.name,
        "description": info.product.description,
        "url": format!("{hub}/"),
        "publisher": { "@type": "Organization", "name": info.operator.name },
    });
    if let Some(category) = &info.software.category {
        software["applicationCategory"] = category.clone().into();
    }
    if !info.software.operating_systems.is_empty() {
        software["operatingSystem"] = info.software.operating_systems.join(", ").into();
    }
    if let Some(offer) = &info.software.offer {
        software["offers"] = json!({
            "@type": "Offer",
            "price": offer.price,
            "priceCurrency": offer.currency,
        });
    }
    if let Some(poster) = site.named(info.assets.poster.as_ref()) {
        software["image"] = format!("{hub}{}", poster.url()).into();
    }
    if site.pages.contains_key("docs") {
        software["softwareHelp"] = json!({ "@type": "CreativeWork", "url": format!("{hub}/docs") });
    }
    let mut out = vec![software];
    if front == Front::Unclaimed {
        return out;
    }
    let shown: Vec<&str> = info.landing.on().collect();
    let landing = &site.landing;
    if shown.contains(&"how") && !landing.how.steps.is_empty() {
        let steps: Vec<Value> = landing
            .how
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                json!({
                    "@type": "HowToStep",
                    "position": i + 1,
                    "name": words(&ctx, &step.title),
                    "text": step.text.pick(front.key()).map(|t| words(&ctx, t)).unwrap_or_default(),
                    "url": format!("{hub}/#how"),
                })
            })
            .collect();
        out.push(json!({
            "@context": "https://schema.org",
            "@type": "HowTo",
            "name": words(&ctx, &landing.how.title),
            "step": steps,
        }));
    }
    if shown.contains(&"faq") && !landing.faq.questions.is_empty() {
        let questions: Vec<Value> = landing
            .faq
            .questions
            .iter()
            .map(|q| {
                json!({
                    "@type": "Question",
                    "name": words(&ctx, &q.q),
                    "acceptedAnswer": { "@type": "Answer", "text": words(&ctx, &q.a) },
                })
            })
            .collect();
        out.push(json!({
            "@context": "https://schema.org",
            "@type": "FAQPage",
            "mainEntity": questions,
        }));
    }
    out
}

/// Copy as plain words: placeholders filled, and `**`, `*`, `` ` `` and `[label](href)`
/// dropped to their text.
pub fn words(ctx: &Ctx, text: &str) -> String {
    let plain = ctx.plain(text);
    let mut out = String::with_capacity(plain.len());
    let mut rest = plain.as_str();
    while let Some(c) = rest.chars().next() {
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest[close..].find(')')
        {
            out.push_str(&rest[1..close]);
            rest = &rest[close + end + 1..];
            continue;
        }
        if !matches!(c, '*' | '`') {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}
