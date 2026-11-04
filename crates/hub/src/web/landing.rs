//! `/` for someone not signed in: the site's product page, served by every hub. Its words
//! come from the site (`content/landing.json`, `site.rs`); this file only lays them out. A
//! hub nobody owns shares the hero and offers only the claim. A hosted hub (`tenancy.mode
//! multi`) is never claimed: its page gets people started, since their account is made the
//! first time their AI connects. The MCP address on it is always this hub's own
//! `public_url`, never a hosted one.
//!
//! The page is a stack of full-viewport pages under the hub's fixed top bar: each `section`
//! snaps into place as it scrolls in, and a rail beside them shows which one you are on.
//! Which sections show, and in what order, is `site.json`'s `landing.sections`. Technical
//! words carry tooltips (the glossary); the comparison's evidence, how claiming works, the
//! AI's tools and the setup walkthrough open as popups (`compare.rs`, `popups.rs`). All of
//! it is CSS, the Popover API and inline SVG under the plain pages' CSP: no script, no
//! inline style, nothing from another origin.

mod compare;
mod network;
mod popups;

#[cfg(test)]
pub(super) use popups::{OFFER_TOOLS, TOOLS};

use super::pages::escape;
use crate::site::{self, Action, Ctx, Front, Item, Limit, SignUp, Site, Step, Variants};

/// Where Sign in goes from the landing page.
const SIGN_IN: &str = "/auth/login?return_to=/account";

/// The title and body of `/` for this kind of hub, with the site the request is served.
pub fn page(front: &Front, public_url: &str) -> (String, String) {
    let site = super::pages::site();
    let ctx = Ctx::new(&site);
    render(&site, &ctx, *front, public_url)
}

/// The landing page drawn with `ctx` (`site.rs` checks every front this way at start).
pub(crate) fn render(site: &Site, ctx: &Ctx, front: Front, public_url: &str) -> (String, String) {
    let ctx = with_page_vars(ctx, site, front, public_url);
    let landing = &site.landing;
    let title = pick(&landing.title, front)
        .map(|t| ctx.plain(t))
        .unwrap_or_else(|| site.info.product.tagline.clone());
    let body = if front == Front::Unclaimed {
        [
            hero(&ctx, front),
            claim_band(&ctx),
            footer(&ctx),
            popups::all(&ctx),
        ]
        .concat()
    } else {
        let mut parts = vec![rail(&ctx, front), hero(&ctx, front)];
        for id in site.info.sections() {
            parts.push(section(&ctx, front, id));
        }
        parts.push(footer(&ctx));
        parts.push(popups::all(&ctx));
        parts.concat()
    };
    (title, body)
}

/// A context for one page: the placeholders the landing page fills, on top of `ctx`'s
/// (whose recorded problems it shares when checking).
fn with_page_vars<'a>(
    ctx: &'a Ctx<'a>,
    site: &'a Site,
    front: Front,
    public_url: &str,
) -> PageCtx<'a> {
    let mcp = format!("{}/mcp", public_url.trim_end_matches('/'));
    let sign_in = pick(&site.landing.sign_in, front)
        .map(|label| ctx.plain(label))
        .unwrap_or_default();
    PageCtx {
        outer: ctx,
        inner: if ctx.is_checking() {
            Ctx::checking(site)
        } else {
            Ctx::new(site)
        }
        .with("mcp", mcp)
        .with("sign_in", sign_in)
        .with("sign_in_url", SIGN_IN),
    }
}

/// The page's context, handing what it found wrong back to the caller's when it drops.
struct PageCtx<'a> {
    outer: &'a Ctx<'a>,
    inner: Ctx<'a>,
}

impl<'a> std::ops::Deref for PageCtx<'a> {
    type Target = Ctx<'a>;
    fn deref(&self) -> &Ctx<'a> {
        &self.inner
    }
}

impl Drop for PageCtx<'_> {
    fn drop(&mut self) {
        for error in self.inner.errors() {
            self.outer.problem(error);
        }
    }
}

/// The copy for `front`, or `None` where the site gives none.
fn pick<T>(variants: &Variants<T>, front: Front) -> Option<&T> {
    variants.pick(front.key())
}

/// A popup: a `popover` with a heading and a Close button that takes focus when it opens.
/// Light dismiss and Escape close it too.
fn modal(id: &str, title: &str, body: &str) -> String {
    format!(
        "<div class=\"modal\" id=\"{id}\" popover role=\"dialog\" aria-labelledby=\"{id}-title\">\
<div class=\"modal-head\"><h2 id=\"{id}-title\">{title}</h2>\
<button type=\"button\" class=\"close\" popovertarget=\"{id}\" popovertargetaction=\"hide\" \
autofocus>Close</button></div><div class=\"modal-body\">{body}</div></div>"
    )
}

/// The site's diagram with its ids under `prefix`, so a page can hold two copies (one shown
/// on phones, one on wide screens) without their markers and labels colliding. A diagram's
/// own ids start with `dg-`.
fn diagram(ctx: &Ctx, prefix: &str) -> Option<String> {
    let svg = ctx
        .site
        .named(ctx.site.info.assets.diagram.as_ref())?
        .text();
    Some(
        svg.replace("id=\"dg-", &format!("id=\"{prefix}-"))
            .replace("url(#dg-", &format!("url(#{prefix}-"))
            .replace("href=\"#dg-", &format!("href=\"#{prefix}-"))
            .replace(
                "aria-labelledby=\"dg-title dg-desc\"",
                &format!("aria-labelledby=\"{prefix}-title {prefix}-desc\""),
            ),
    )
}

/// The setup video: click to play, muted, no download until then. `None` without one.
fn video(ctx: &Ctx) -> Option<String> {
    let site = ctx.site;
    let video = site.named(site.info.assets.video.as_ref())?;
    let poster = site
        .named(site.info.assets.poster.as_ref())
        .map(|poster| format!(" poster=\"{}\"", escape(&poster.url())))
        .unwrap_or_default();
    Some(format!(
        "<video class=\"turnkey\" controls muted playsinline preload=\"none\" width=\"1280\" \
height=\"720\"{poster} aria-label=\"{label}\"><source src=\"{src}\" type=\"{mime}\"></video>",
        label = site::text::attr(&ctx.plain(&site.landing.hero.video_label)),
        src = escape(&video.url()),
        mime = video.mime,
    ))
}

/// Opens the setup walkthrough, when the site has a video.
fn watch_button(ctx: &Ctx) -> String {
    if video(ctx).is_none() {
        return String::new();
    }
    ctx.open("watch");
    format!(
        "<button type=\"button\" class=\"pop watch\" popovertarget=\"m-watch\">\
<svg class=\"play\" viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\">\
<circle cx=\"12\" cy=\"12\" r=\"11\"/><path d=\"M9.8 7.6l6.4 4.4-6.4 4.4z\"/></svg>{}</button>",
        ctx.inline(&ctx.site.landing.popups.watch.button)
    )
}

/// Links that look like buttons.
fn actions(ctx: &Ctx, actions: &[Action]) -> String {
    actions
        .iter()
        .map(|action| {
            let href = ctx.plain(&action.href);
            if !site::safe_href(&href) {
                ctx.problem(format!(
                    "button href {href:?} is not a path, #anchor, https: or mailto:"
                ));
            }
            format!(
                "<a class=\"button{}\" href=\"{}\">{}</a>",
                if action.primary { " primary" } else { "" },
                site::text::attr(&href),
                ctx.inline(&action.label)
            )
        })
        .collect()
}

/// The hero: on a phone the diagram follows the words; on a wide screen the setup video
/// sits beside them.
fn hero(ctx: &Ctx, front: Front) -> String {
    let hero = &ctx.site.landing.hero;
    let status = pick(&hero.status, front)
        .filter(|s| !s.is_empty())
        .map(|s| {
            format!(
                "<p class=\"state\"><span class=\"dot\"></span>{}</p>",
                ctx.inline(s)
            )
        })
        .unwrap_or_default();
    let lead = pick(&hero.lead, front)
        .map(|lead| format!("<p class=\"lead\">{}</p>", ctx.inline(lead)))
        .unwrap_or_default();
    let buttons = pick(&hero.actions, front)
        .map(|list| actions(ctx, list))
        .unwrap_or_default();
    let note = pick(&hero.note, front)
        .filter(|n| !n.is_empty())
        .map(|note| format!("<p class=\"meta\">{}</p>", ctx.inline(note)))
        .unwrap_or_default();
    let title = ctx.inline(&hero.title);
    let cta = format!("<p class=\"cta\">{buttons}{}</p>", watch_button(ctx));
    let picture = diagram(ctx, "dg")
        .map(|svg| format!("<div class=\"diagram\">{svg}</div>"))
        .unwrap_or_default();
    let film = video(ctx)
        .map(|video| {
            format!(
                "<figure class=\"hero-video\">{video}<figcaption>{}</figcaption></figure>",
                ctx.inline(&hero.video_caption)
            )
        })
        .unwrap_or_default();
    let visual = if picture.is_empty() && film.is_empty() {
        String::new()
    } else {
        format!("<div class=\"hero-visual\">{picture}{film}</div>")
    };
    format!(
        "<section class=\"hero\" id=\"top\"><div class=\"hero-text\">{status}<h1>{title}</h1>\
{lead}{cta}{note}</div>{visual}</section>"
    )
}

/// Where you are on the page: one dot per section, the current one lit (the stylesheet
/// has a view timeline per section this site shows).
fn rail(ctx: &Ctx, front: Front) -> String {
    let site = ctx.site;
    let links: String = std::iter::once("top")
        .chain(site.info.sections())
        .filter(|id| *id != "network" || front != Front::Nomen)
        .map(|id| {
            format!(
                "<a href=\"#{id}\"><span>{}</span></a>",
                ctx.inline(rail_name(site, id))
            )
        })
        .collect();
    format!(
        "<nav class=\"rail\" aria-label=\"{}\">{links}</nav>",
        site::text::attr(&ctx.plain(&site.landing.rail_label))
    )
}

fn rail_name<'a>(site: &'a Site, id: &'a str) -> &'a str {
    let landing = &site.landing;
    match id {
        "top" => &landing.hero.rail,
        "how" => &landing.how.rail,
        "compare" => &landing.compare.rail,
        "features" => &landing.features.rail,
        "network" => site
            .info
            .offers()
            .map(|offers| offers.rail.as_str())
            .unwrap_or(id),
        "plans" => &landing.plans.rail,
        "services" => &landing.services.rail,
        "connect" => &landing.connect.rail,
        "self-host" => &landing.self_host.rail,
        "next" => &landing.next.rail,
        "faq" => &landing.faq.rail,
        "get-started" => &landing.closing.rail,
        custom => landing
            .custom
            .get(custom)
            .map(|b| b.rail.as_str())
            .unwrap_or(custom),
    }
}

fn section(ctx: &Ctx, front: Front, id: &str) -> String {
    match id {
        "how" => how(ctx, front),
        "compare" => compare::section(ctx),
        "features" => features(ctx, front),
        "network" => network::section(ctx, front),
        "plans" => plans(ctx),
        "services" => services(ctx),
        "connect" => connect(ctx, front),
        "self-host" => self_host(ctx),
        "next" => next(ctx),
        "faq" => faq(ctx),
        "get-started" => closing(ctx, front),
        custom => match ctx.site.landing.custom.get(custom) {
            Some(band) => plain_band(ctx, custom, band, "items"),
            None => String::new(),
        },
    }
}

/// Lines shown as a terminal: a `#` at the start of a line or after a space begins a
/// comment, drawn dimmed.
fn terminal(ctx: &Ctx, lines: &[String]) -> String {
    let lines: Vec<String> = lines
        .iter()
        .map(|line| {
            let line = ctx.plain(line);
            let comment = line
                .char_indices()
                .find(|(i, c)| *c == '#' && (*i == 0 || line[..*i].ends_with(' ')))
                .map(|(i, _)| i);
            match comment {
                Some(at) => format!(
                    "{}<span class=\"c\">{}</span>",
                    site::text::escape_text(&line[..at]),
                    site::text::escape_text(&line[at..])
                ),
                None => site::text::escape_text(&line),
            }
        })
        .collect();
    format!("<pre class=\"terminal\">{}</pre>", lines.join("\n"))
}

fn steps(ctx: &Ctx, front: Front, steps: &[Step]) -> String {
    let items: String = steps
        .iter()
        .map(|step| {
            let text = pick(&step.text, front)
                .map(|text| format!("<p>{}</p>", ctx.inline(text)))
                .unwrap_or_default();
            let shell = if step.terminal.is_empty() {
                String::new()
            } else {
                terminal(ctx, &step.terminal)
            };
            format!("<li><h3>{}</h3>{text}{shell}</li>", ctx.inline(&step.title))
        })
        .collect();
    format!("<ol class=\"steps\">{items}</ol>")
}

/// A new hub: how to find its claim link. Nothing to sign in to yet.
fn claim_band(ctx: &Ctx) -> String {
    let band = &ctx.site.landing.claim;
    let meta = if band.meta.is_empty() {
        String::new()
    } else {
        format!("<p class=\"meta\">{}</p>", ctx.inline(&band.meta))
    };
    format!(
        "<section class=\"band\" aria-labelledby=\"claim-steps\">\
<h2 id=\"claim-steps\">{}</h2>{}{meta}</section>",
        ctx.inline(&band.title),
        steps(ctx, Front::Unclaimed, &band.steps),
    )
}

fn how(ctx: &Ctx, front: Front) -> String {
    let band = &ctx.site.landing.how;
    let picture = diagram(ctx, "dgw")
        .map(|svg| format!("<div class=\"diagram how-diagram\">{svg}</div>"))
        .unwrap_or_default();
    format!(
        "<section class=\"band\" id=\"how\" aria-labelledby=\"how-title\">\
<h2 id=\"how-title\">{}</h2>{}{picture}</section>",
        ctx.inline(&band.title),
        steps(ctx, front, &band.steps),
    )
}

/// `<dt>`/`<dd>` pairs.
fn items(ctx: &Ctx, front: Front, items: &[Item]) -> String {
    items
        .iter()
        .map(|item| {
            format!(
                "<dt>{}</dt><dd>{}</dd>",
                ctx.inline(&item.term),
                pick(&item.text, front)
                    .map(|text| ctx.inline(text))
                    .unwrap_or_default()
            )
        })
        .collect()
}

fn features(ctx: &Ctx, front: Front) -> String {
    let copy = &ctx.site.landing.features;
    let pillars: String = copy
        .pillars
        .iter()
        .map(|pillar| {
            format!(
                "<div class=\"pillar\"><h3>{}</h3><dl>{}</dl></div>",
                ctx.inline(&pillar.title),
                items(ctx, front, &pillar.items)
            )
        })
        .collect();
    let phone = copy
        .phone
        .as_ref()
        .map(|phone| {
            format!(
                "<figure class=\"phone-figure\"><div class=\"phone\" aria-hidden=\"true\"><div class=\"approval\">\
<p class=\"asker\">{}</p><p class=\"ask-title\">{}</p>\
<pre class=\"what\">{}</pre>\
<p class=\"decide\"><span class=\"yes\">{}</span><span class=\"no\">{}</span></p></div></div>\
<figcaption>{}</figcaption></figure>",
                ctx.inline(&phone.asker),
                ctx.inline(&phone.title),
                site::text::escape_text(&ctx.plain(&phone.what)),
                ctx.inline(&phone.approve),
                ctx.inline(&phone.deny),
                ctx.inline(&phone.caption),
            )
        })
        .unwrap_or_default();
    format!(
        "<section class=\"band\" id=\"features\" aria-labelledby=\"features-title\">\
<h2 id=\"features-title\">{}</h2><div class=\"pillars\">{pillars}{phone}</div></section>",
        ctx.inline(&copy.title)
    )
}

/// The plans `site.json` lists, with their limits in the words of `landing.json`.
fn plans(ctx: &Ctx) -> String {
    let site = ctx.site;
    let copy = &site.landing.plans;
    let cards: String = site
        .info
        .plans
        .iter()
        .map(|plan| {
            let limits: String = plan
                .limits
                .iter()
                .filter_map(|(key, limit)| {
                    let words = copy.limits.get(key).or_else(|| {
                        ctx.problem(format!("plans: content/landing.json plans.limits has no words for {key}"));
                        None
                    })?;
                    let text = match limit {
                        Limit::Count(1) => words.one.replace("{n}", "1"),
                        Limit::Count(n) => words.other.replace("{n}", &n.to_string()),
                        Limit::Word(_) => words.unlimited.clone(),
                    };
                    Some(format!("<li>{}</li>", ctx.inline(&text)))
                })
                .collect();
            let limits = if limits.is_empty() {
                limits
            } else {
                format!("<ul class=\"limits\">{limits}</ul>")
            };
            let features: String = plan
                .features
                .iter()
                .map(|f| format!("<li>{}</li>", ctx.inline(f)))
                .collect();
            let features = if features.is_empty() {
                features
            } else {
                format!("<ul class=\"features\">{features}</ul>")
            };
            let cta = plan
                .cta
                .as_ref()
                .map(|cta| format!("<p class=\"cta\">{}</p>", actions(ctx, std::slice::from_ref(cta))))
                .unwrap_or_default();
            format!(
                "<li class=\"plan\"><h3>{}</h3><p class=\"price\">{}</p>{limits}{features}{cta}</li>",
                ctx.inline(&plan.name),
                ctx.inline(&plan.price)
            )
        })
        .collect();
    let early = &site.info.early_access;
    let early = if early.mode == SignUp::EarlyAccess {
        let cta = early
            .cta
            .as_ref()
            .map(|cta| actions(ctx, std::slice::from_ref(cta)))
            .unwrap_or_default();
        format!("<p class=\"early\">{} {cta}</p>", ctx.inline(&early.note))
    } else {
        String::new()
    };
    format!(
        "<section class=\"band plans\" id=\"plans\" aria-labelledby=\"plans-title\">\
<h2 id=\"plans-title\">{}</h2>{}<ul class=\"plan-list\">{cards}</ul>{early}</section>",
        ctx.inline(&copy.title),
        intro(ctx, &copy.intro),
    )
}

fn intro(ctx: &Ctx, text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("<p class=\"intro\">{}</p>", ctx.inline(text))
    }
}

/// The services `site.json` lists.
fn services(ctx: &Ctx) -> String {
    let copy = &ctx.site.landing.services;
    let list: String = ctx
        .site
        .info
        .services
        .iter()
        .map(|service| {
            let link = match &service.href {
                Some(href) => format!(
                    "<p><a href=\"{}\">{}</a></p>",
                    site::text::attr(&ctx.plain(href)),
                    ctx.inline(service.label.as_deref().unwrap_or(&service.name))
                ),
                None => String::new(),
            };
            format!(
                "<li><h3>{}</h3><p>{}</p>{link}</li>",
                ctx.inline(&service.name),
                ctx.inline(&service.text)
            )
        })
        .collect();
    format!(
        "<section class=\"band services\" id=\"services\" aria-labelledby=\"services-title\">\
<h2 id=\"services-title\">{}</h2>{}<ul class=\"service-list\">{list}</ul></section>",
        ctx.inline(&copy.title),
        intro(ctx, &copy.intro),
    )
}

fn connect(ctx: &Ctx, front: Front) -> String {
    let copy = &ctx.site.landing.connect;
    let intro = pick(&copy.intro, front)
        .map(|text| ctx.inline(text))
        .unwrap_or_default();
    let clients = pick(&copy.clients, front)
        .map(|list| items(ctx, front, list))
        .unwrap_or_default();
    let mcp = ctx.plain("{mcp}");
    format!(
        "<section class=\"band connect\" id=\"connect\" aria-labelledby=\"connect-title\">\
<div><h2 id=\"connect-title\">{}</h2><p>{intro}</p>\
<p class=\"address\"><code>{}</code></p></div>\
<div><dl class=\"clients\">{clients}</dl><p class=\"meta\">{}</p></div></section>",
        ctx.inline(&copy.title),
        escape(&mcp),
        ctx.inline(&copy.meta),
    )
}

fn self_host(ctx: &Ctx) -> String {
    let copy = &ctx.site.landing.self_host;
    let facts: String = copy
        .facts
        .iter()
        .map(|fact| format!("<li>{}</li>", ctx.inline(fact)))
        .collect();
    let hosted = if copy.hosted.is_empty() {
        String::new()
    } else {
        format!("<p class=\"hosted\">{}</p>", ctx.inline(&copy.hosted))
    };
    format!(
        "<section class=\"band self-host\" id=\"self-host\" aria-labelledby=\"self-host-title\">\
<h2 id=\"self-host-title\">{}</h2>{}\
<div class=\"split\">{}<div><ul class=\"facts\">{facts}</ul>{hosted}</div></div></section>",
        ctx.inline(&copy.title),
        intro(ctx, &copy.intro),
        terminal(ctx, &copy.terminal),
    )
}

fn next(ctx: &Ctx) -> String {
    plain_band(ctx, "next", &ctx.site.landing.next, "coming")
}

/// A title, an intro and a list: Where it's going, and a site's own sections.
fn plain_band(ctx: &Ctx, id: &str, band: &site::Band, list: &str) -> String {
    let items: String = band
        .items
        .iter()
        .map(|item| format!("<li>{}</li>", ctx.inline(item)))
        .collect();
    let items = if items.is_empty() {
        items
    } else {
        format!("<ul class=\"{list}\">{items}</ul>")
    };
    format!(
        "<section class=\"band {id}\" id=\"{id}\" aria-labelledby=\"{id}-title\"><h2 id=\"{id}-title\">\
{}</h2>{}{items}</section>",
        ctx.inline(&band.title),
        intro(ctx, &band.intro),
    )
}

fn faq(ctx: &Ctx) -> String {
    let copy = &ctx.site.landing.faq;
    let questions: String = copy
        .questions
        .iter()
        .map(|q| {
            format!(
                "<details><summary>{}</summary><p>{}</p></details>",
                ctx.inline(&q.q),
                ctx.inline(&q.a)
            )
        })
        .collect();
    let meta = if copy.meta.is_empty() {
        String::new()
    } else {
        format!("<p class=\"meta\">{}</p>", ctx.inline(&copy.meta))
    };
    format!(
        "<section class=\"band faq\" id=\"faq\" aria-labelledby=\"faq-title\">\
<h2 id=\"faq-title\">{}</h2>{questions}{meta}</section>",
        ctx.inline(&copy.title)
    )
}

fn closing(ctx: &Ctx, front: Front) -> String {
    let copy = &ctx.site.landing.closing;
    let lead = pick(&copy.lead, front)
        .map(|lead| ctx.inline(lead))
        .unwrap_or_default();
    let buttons = pick(&copy.actions, front)
        .map(|list| actions(ctx, list))
        .unwrap_or_default();
    let mascot = ctx
        .site
        .named(ctx.site.info.assets.mascot.as_ref())
        .map(|svg| svg.text())
        .unwrap_or_default();
    format!(
        "<section class=\"band closing\" id=\"get-started\" aria-labelledby=\"closing-title\">\
<div class=\"closing-card\"><div>\
<h2 id=\"closing-title\">{}</h2>\
<p>{lead}</p><p class=\"cta\">{buttons}</p></div>{mascot}</div></section>",
        ctx.inline(&copy.title)
    )
}

fn footer(ctx: &Ctx) -> String {
    let footer = &ctx.site.landing.footer;
    let links: String = footer
        .links
        .iter()
        .map(|link| {
            format!(
                "<a href=\"{}\">{}</a>",
                site::text::attr(&ctx.plain(&link.href)),
                ctx.inline(&link.label)
            )
        })
        .collect();
    format!(
        "<footer class=\"foot\"><nav aria-label=\"{}\">{links}</nav></footer>",
        site::text::attr(&ctx.plain(&footer.label))
    )
}
