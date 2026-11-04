//! `offers` in `site.json`: the network a site belongs to and the free offerings it links an
//! account to when its person says yes in the chat (`src/offers.rs`). Every word of it is
//! the site's. It shows only where it is on: `enabled` (true unless it says false) and at
//! least one offering. Off, or with no offerings, nothing of it shows anywhere: no landing
//! section, no tool, no word on a page, in `/llms.txt` or in the MCP instructions.
//!
//! ```jsonc
//! "offers": {
//!   "enabled": true,
//!   "brand": "…",            // the network's name: {brand} in the words below
//!   "rail": "…", "title": "…", "intro": "…",   // its landing section (`network`)
//!   "question": "…",         // the one question the AI asks, as it is
//!   "instructions": "…",     // added to the MCP instructions while it is on
//!   "link": { "title": "…", "text": "…", "button": "…", "done": "…", "gone": "…" },
//!   "items": [ { "name": "…", "text": "…", "href": "https://…" } ]
//! }
//! ```
//!
//! An offering's `text` and `href` may be empty: the section shows its name alone.

use serde::Deserialize;

use super::model::safe_href;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offers {
    #[serde(default = "on")]
    pub enabled: bool,
    /// The network's name: `{brand}` in its words.
    pub brand: String,
    /// The landing section's name on the rail, its heading and its intro (inline markup).
    pub rail: String,
    pub title: String,
    pub intro: String,
    /// The one question the AI asks its person, as it is (`offers`).
    pub question: String,
    /// What the MCP instructions add while offers are on: when to offer, and how.
    pub instructions: String,
    /// The page the one-tap link opens (`web/offers.rs`).
    pub link: LinkPage,
    pub items: Vec<Offering>,
}

fn on() -> bool {
    true
}

/// The words of the page the fallback link opens. Inline markup.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkPage {
    pub title: String,
    /// What pressing the button does.
    pub text: String,
    /// The button: it goes on to the network's sign-in.
    pub button: String,
    /// Once the account is linked.
    pub done: String,
    /// A link that expired, was replaced, or never was.
    pub gone: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offering {
    pub name: String,
    /// One line: what it is. May be empty.
    #[serde(default)]
    pub text: String,
    /// Where to learn more. May be empty.
    #[serde(default)]
    pub href: String,
}

impl Offers {
    /// Whether any of it shows: on, with an offering.
    pub fn shown(&self) -> bool {
        self.enabled && !self.items.is_empty()
    }

    pub(super) fn check(&self) -> Result<(), String> {
        let bad = |what: &str, why: &str| Err(format!("site.json offers.{what}: {why}"));
        if !self.shown() {
            return Ok(());
        }
        for (what, text) in [
            ("brand", &self.brand),
            ("rail", &self.rail),
            ("title", &self.title),
            ("question", &self.question),
            ("instructions", &self.instructions),
            ("link.title", &self.link.title),
            ("link.button", &self.link.button),
            ("link.done", &self.link.done),
            ("link.gone", &self.link.gone),
        ] {
            if text.trim().is_empty() {
                return bad(what, "say it in words");
            }
        }
        if self.items.len() > 32 {
            return bad("items", "at most 32");
        }
        for item in &self.items {
            if item.name.trim().is_empty() || item.name.len() > 80 {
                return bad("items", "each has a name, at most 80 characters");
            }
            if !item.href.is_empty() && !(item.href.starts_with("https://") && safe_href(&item.href)) {
                return bad(
                    &format!("items {}.href", item.name),
                    "an https: URL, or empty",
                );
            }
        }
        Ok(())
    }
}
