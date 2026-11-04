//! `tokens.json`: the design tokens the stylesheet reads as custom properties, and the
//! fonts a site serves itself. The hub's stylesheet (`web/hub.css`, `web/landing.css`)
//! only ever uses `var(--…)`; the tokens set them, light and dark, so a site restyles every
//! page without touching a rule.
//!
//! ```jsonc
//! {
//!   "colors": { "bg": "#f3f5f1", "accent": "#2f6b4f", … },   // --bg, --accent, …
//!   "dark":   { "bg": "#0e1411", … },                          // under prefers-color-scheme: dark
//!   "fonts":  { "ui": "…", "display": "…", "mono": "…" },     // --font-ui, …
//!   "radii":  { "s": "6px", "m": "8px", "l": "12px", "xl": "20px", "pill": "999px" },
//!   "layout": { "bar-h": "60px", "gutter": "20px", "wide": "1120px", "narrow": "728px" },
//!   "theme_color": "#2f6b4f", "background_color": "#f7f6f2",  // the installed app
//!   "font_files": [ { "family": "Inter", "file": "inter.woff2", "weight": "100 900" } ]
//! }
//! ```
//!
//! A site's `tokens.json` gives only what it changes; a name the built-in tokens do not
//! have is a typo, and an error.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{Asset, SiteFile};

/// The hub's own rules, after the tokens.
const BASE: &str = concat!(
    include_str!("../web/hub.css"),
    include_str!("../web/landing.css")
);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub dark: BTreeMap<String, String>,
    pub fonts: BTreeMap<String, String>,
    pub radii: BTreeMap<String, String>,
    pub layout: BTreeMap<String, String>,
    pub theme_color: String,
    pub background_color: String,
    #[serde(default)]
    pub font_files: Vec<FontFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontFile {
    pub family: String,
    /// A `.woff2` or `.woff` in `assets/`.
    pub file: String,
    #[serde(default = "normal_weight")]
    pub weight: String,
    #[serde(default = "normal_style")]
    pub style: String,
}

fn normal_weight() -> String {
    "400".into()
}

fn normal_style() -> String {
    "normal".into()
}

pub struct Style {
    pub css: String,
    pub etag: String,
    pub fonts: bool,
    pub theme_color: String,
    pub background_color: String,
}

/// The token names the stylesheet reads, per group: anything else is a typo.
fn known() -> &'static BTreeMap<&'static str, Vec<String>> {
    static KNOWN: std::sync::OnceLock<BTreeMap<&'static str, Vec<String>>> =
        std::sync::OnceLock::new();
    KNOWN.get_or_init(|| {
        let builtin: Tokens = serde_json::from_str(include_str!("../../site/default/tokens.json"))
            .expect("the built-in tokens parse");
        let names = |map: &BTreeMap<String, String>| map.keys().cloned().collect::<Vec<_>>();
        BTreeMap::from([
            ("colors", names(&builtin.colors)),
            ("dark", names(&builtin.colors)),
            ("fonts", names(&builtin.fonts)),
            ("radii", names(&builtin.radii)),
            ("layout", names(&builtin.layout)),
        ])
    })
}

/// A value safe inside a declaration: no way out of it into another rule or a fetch.
fn safe_value(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 300
        && !value.to_ascii_lowercase().contains("url(")
        && !value.contains("/*")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " #%(),.-+\"'_/".contains(c))
}

impl Style {
    pub fn build(
        tokens: &Tokens,
        site: &SiteFile,
        assets: &BTreeMap<String, Asset>,
    ) -> Result<Self, String> {
        let known = known();
        for (group, map) in [
            ("colors", &tokens.colors),
            ("dark", &tokens.dark),
            ("fonts", &tokens.fonts),
            ("radii", &tokens.radii),
            ("layout", &tokens.layout),
        ] {
            for (name, value) in map {
                if !known[group].contains(name) {
                    return Err(format!(
                        "tokens.json {group}.{name}: the stylesheet reads no such token ({})",
                        known[group].join(", ")
                    ));
                }
                if !safe_value(value) {
                    return Err(format!(
                        "tokens.json {group}.{name}: {value:?} is not a plain CSS value"
                    ));
                }
            }
        }
        for (what, value) in [
            ("theme_color", &tokens.theme_color),
            ("background_color", &tokens.background_color),
        ] {
            if !safe_value(value) {
                return Err(format!(
                    "tokens.json {what}: {value:?} is not a plain CSS value"
                ));
            }
        }
        let mut css = String::from(":root {\n");
        let line = |css: &mut String, entries: Vec<(String, &String)>| {
            let joined: Vec<String> = entries
                .into_iter()
                .map(|(name, value)| format!("--{name}: {value};"))
                .collect();
            css.push_str("  ");
            css.push_str(&joined.join(" "));
            css.push('\n');
        };
        line(
            &mut css,
            tokens.colors.iter().map(|(k, v)| (k.clone(), v)).collect(),
        );
        line(
            &mut css,
            tokens
                .fonts
                .iter()
                .map(|(k, v)| (format!("font-{k}"), v))
                .collect(),
        );
        line(
            &mut css,
            tokens
                .radii
                .iter()
                .map(|(k, v)| (format!("radius-{k}"), v))
                .collect(),
        );
        line(
            &mut css,
            tokens.layout.iter().map(|(k, v)| (k.clone(), v)).collect(),
        );
        css.push_str("  color-scheme: light dark;\n}\n");
        if !tokens.dark.is_empty() {
            css.push_str("@media (prefers-color-scheme: dark) {\n  :root {\n  ");
            line(
                &mut css,
                tokens.dark.iter().map(|(k, v)| (k.clone(), v)).collect(),
            );
            css.push_str("  }\n}\n");
        }
        for font in &tokens.font_files {
            let asset = assets.get(&font.file).ok_or_else(|| {
                format!("tokens.json font_files: there is no assets/{}", font.file)
            })?;
            if !matches!(asset.extension(), "woff2" | "woff") {
                return Err(format!(
                    "tokens.json font_files: assets/{} is not a .woff2 or .woff",
                    font.file
                ));
            }
            for (what, value) in [
                ("family", &font.family),
                ("weight", &font.weight),
                ("style", &font.style),
            ] {
                if !safe_value(value) || value.contains(['"', '\'']) {
                    return Err(format!(
                        "tokens.json font_files {}: {what} {value:?} is not a plain value",
                        font.file
                    ));
                }
            }
            css.push_str(&format!(
                "@font-face {{ font-family: \"{}\"; src: url({}) format(\"{}\"); font-weight: {}; font-style: {}; font-display: swap; }}\n",
                font.family,
                asset.url(),
                asset.extension(),
                font.weight,
                font.style,
            ));
        }
        css.push_str(BASE);
        css.push_str(&rail(site));
        use sha2::Digest;
        let etag = format!(
            "\"{}\"",
            &hex::encode(sha2::Sha256::digest(css.as_bytes()))[..32]
        );
        Ok(Self {
            css,
            etag,
            fonts: !tokens.font_files.is_empty(),
            theme_color: tokens.theme_color.clone(),
            background_color: tokens.background_color.clone(),
        })
    }
}

/// The landing page's rail lights the page in view: one view timeline per section this
/// site shows, named after its id.
fn rail(site: &SiteFile) -> String {
    let ids: Vec<&str> = std::iter::once("top").chain(site.sections()).collect();
    let scope: Vec<String> = ids.iter().map(|id| format!("--p-{id}")).collect();
    let mut css = format!(
        "@supports (timeline-scope: --a) and (animation-timeline: view()) {{\n  .landing main {{ timeline-scope: {}; }}\n",
        scope.join(", ")
    );
    for id in &ids {
        css.push_str(&format!(
            "  #{id} {{ view-timeline: --p-{id} block, --page block; }}\n"
        ));
    }
    css.push_str(
        "  .rail a { animation: rail-on linear; animation-range: cover 50vh cover calc(100% - 50vh); }\n",
    );
    for id in &ids {
        css.push_str(&format!(
            "  .rail a[href=\"#{id}\"] {{ animation-timeline: --p-{id}; }}\n"
        ));
    }
    css.push_str("}\n");
    css
}
