//! The hub's site: every word, name, link, colour and picture its pages, its sign-in email
//! and its MCP display show, from a site directory instead of the code. Design tokens and
//! content are kept apart from the code that lays them out, so one hub binary serves any
//! brand (`crates/hub/README.md`, "The site").
//!
//! ```text
//! site/
//!   site.json              product, operator, links, sign-in labels, landing order, MCP,
//!                          plans, services, early access, which assets are used
//!   tokens.json            colours, type, radii and sizes the stylesheet reads, and fonts
//!   content/landing.json   every landing section's copy
//!   content/messages.json  the sign-in email, the board's getting-started ticket, what
//!                          early access and plans say (waiting, a limit, "you're in")
//!   content/docs.md, privacy.md, terms.md, support.md   the public pages
//!   assets/                logo, icon, diagram, video, poster, fonts (served from 'self')
//! ```
//!
//! `site.dir` in the hub's config (or `DEVAGOTCHI_SITE_DIR`) names the directory. Only
//! `site.json` is required there: any other file left out is the built-in default's
//! (`crates/hub/site/default/`), and `tokens.json` and `messages.json` may give just the
//! values they change. The directory is read and checked once, when the hub starts; one
//! that does not check out stops the start with what is wrong and where. With no directory
//! the built-in default serves, compiled into the binary.

mod analytics_copy;
mod assets;
#[cfg(test)]
mod brand_tests;
mod markdown;
pub(crate) mod model;
mod plans;
mod style;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod text;
mod watch;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

pub use assets::Asset;
pub use markdown::Page;
pub use model::{
    Action, Band, Front, Item, Landing, Limit, Messages, Plan, SignUp, SiteFile, Step, Variants,
    safe_href,
};
pub use plans::Cap;
pub use text::Ctx;
pub use watch::{EVERY as WATCH_EVERY, SiteCell, watch};

/// The environment variable that names the site directory, over `site.dir` in the config.
pub const SITE_ENV: &str = "DEVAGOTCHI_SITE_DIR";

/// The pages a site has, by the path they are served at.
pub const PAGES: [&str; 4] = ["docs", "privacy", "terms", "support"];

/// The text files a site directory may hold, besides `assets/`.
const TEXT_FILES: [&str; 8] = [
    "site.json",
    "tokens.json",
    "content/landing.json",
    "content/messages.json",
    "content/docs.md",
    "content/privacy.md",
    "content/terms.md",
    "content/support.md",
];
const MAX_TEXT_BYTES: u64 = 1024 * 1024;

/// A loaded, checked site.
pub struct Site {
    /// Where it was read from; `None` for the built-in default.
    pub dir: Option<PathBuf>,
    pub info: SiteFile,
    pub landing: Landing,
    pub messages: Messages,
    pub pages: BTreeMap<String, Page>,
    pub assets: BTreeMap<String, Asset>,
    style: style::Style,
}

impl std::fmt::Debug for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.dir {
            Some(dir) => write!(f, "Site({})", dir.display()),
            None => write!(f, "Site(built in)"),
        }
    }
}

/// The site's files as read: text by path, assets by file name.
#[derive(Clone, Default)]
struct Files {
    text: BTreeMap<String, String>,
    assets: BTreeMap<String, axum::body::Bytes>,
}

macro_rules! builtin_text {
    ($($path:literal),* $(,)?) => {
        [$(($path, include_str!(concat!("../site/default/", $path)))),*]
    };
}
macro_rules! builtin_assets {
    ($($name:literal),* $(,)?) => {
        [$(($name, &include_bytes!(concat!("../site/default/assets/", $name))[..])),*]
    };
}

fn builtin_files() -> Files {
    let text = builtin_text!(
        "site.json",
        "tokens.json",
        "content/landing.json",
        "content/messages.json",
        "content/docs.md",
        "content/privacy.md",
        "content/terms.md",
        "content/support.md",
    );
    let assets = builtin_assets!(
        "logo.svg",
        "icon.svg",
        "pet.svg",
        "diagram.svg",
        "turnkey.mp4",
        "turnkey-poster.png",
    );
    Files {
        text: text
            .into_iter()
            .map(|(path, body)| (path.to_string(), body.to_string()))
            .collect(),
        assets: assets
            .into_iter()
            .map(|(name, bytes)| (name.to_string(), axum::body::Bytes::from_static(bytes)))
            .collect(),
    }
}

static BUILTIN: LazyLock<Arc<Site>> = LazyLock::new(|| {
    Arc::new(Site::from_files(builtin_files(), None).expect("the built-in site checks out"))
});

/// The built-in default site, compiled into the binary.
pub fn builtin() -> Arc<Site> {
    BUILTIN.clone()
}

impl Site {
    /// Read and check the site directory at `dir` (absolute). The error says which file
    /// and what in it is wrong.
    pub fn load(dir: &Path) -> Result<Self, String> {
        if !dir.is_absolute() {
            return Err(format!("{} is not an absolute path", dir.display()));
        }
        let meta = std::fs::metadata(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        if !meta.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        if !dir.join("site.json").is_file() {
            return Err(format!(
                "{} has no site.json (the one file a site directory must have)",
                dir.display()
            ));
        }
        let mut files = builtin_files();
        for path in TEXT_FILES {
            let file = dir.join(path);
            if !file.exists() {
                continue;
            }
            let text = read_text(&file)?;
            let merged = match path {
                // Tokens and messages may give only what they change.
                "tokens.json" | "content/messages.json" => {
                    merge_json(&files.text[path], &text).map_err(|why| format!("{path}: {why}"))?
                }
                _ => text,
            };
            files.text.insert(path.to_string(), merged);
        }
        let content = dir.join("content");
        if content.is_dir() {
            for entry in read_dir(&content)? {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with('.')
                    && !TEXT_FILES.contains(&format!("content/{name}").as_str())
                {
                    return Err(format!(
                        "content/{name} is not a file a site reads (landing.json, messages.json, docs.md, privacy.md, terms.md, support.md)"
                    ));
                }
            }
        }
        let assets = dir.join("assets");
        if assets.is_dir() {
            let mut total = 0u64;
            for entry in read_dir(&assets)? {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let path = entry.path();
                let meta =
                    std::fs::metadata(&path).map_err(|error| format!("assets/{name}: {error}"))?;
                if !meta.is_file() {
                    return Err(format!(
                        "assets/{name}: assets is one flat directory of files"
                    ));
                }
                assets::check_name(&name).map_err(|why| format!("assets/{name}: {why}"))?;
                if meta.len() > assets::MAX_ASSET_BYTES {
                    return Err(format!(
                        "assets/{name} is {} bytes; an asset is at most {}",
                        meta.len(),
                        assets::MAX_ASSET_BYTES
                    ));
                }
                total += meta.len();
                if total > assets::MAX_TOTAL_BYTES {
                    return Err(format!(
                        "assets/ holds more than {} bytes",
                        assets::MAX_TOTAL_BYTES
                    ));
                }
                let bytes =
                    std::fs::read(&path).map_err(|error| format!("assets/{name}: {error}"))?;
                files.assets.insert(name, bytes.into());
            }
        }
        Self::from_files(files, Some(dir.to_path_buf()))
    }

    fn from_files(files: Files, dir: Option<PathBuf>) -> Result<Self, String> {
        let text = |path: &str| files.text.get(path).map(String::as_str).unwrap_or_default();
        let info: SiteFile = parse_json("site.json", text("site.json"))?;
        let landing: Landing = parse_json("content/landing.json", text("content/landing.json"))?;
        let messages: Messages =
            parse_json("content/messages.json", text("content/messages.json"))?;
        let tokens: style::Tokens = parse_json("tokens.json", text("tokens.json"))?;
        let mut pages = BTreeMap::new();
        for name in PAGES {
            let path = format!("content/{name}.md");
            let page = Page::parse(text(&path)).map_err(|why| format!("{path}: {why}"))?;
            pages.insert(name.to_string(), page);
        }
        let mut loaded = BTreeMap::new();
        for (name, bytes) in files.assets {
            let asset = Asset::new(&name, bytes).map_err(|why| format!("assets/{name}: {why}"))?;
            loaded.insert(name, asset);
        }
        let style = style::Style::build(&tokens, &info, &loaded)?;
        // Only what the site uses is served: a file left in assets/ (or a built-in one this
        // site does not name) stays off the web.
        let names = &info.assets;
        let used: Vec<&String> = [
            &names.logo,
            &names.icon,
            &names.mascot,
            &names.diagram,
            &names.video,
            &names.poster,
        ]
        .into_iter()
        .flatten()
        .chain(tokens.font_files.iter().map(|font| &font.file))
        .collect();
        loaded.retain(|name, _| used.contains(&name));
        let site = Self {
            dir,
            info,
            landing,
            messages,
            pages,
            assets: loaded,
            style,
        };
        site.check()?;
        Ok(site)
    }

    /// Everything a request could render, rendered once now: a placeholder, a link or a
    /// section that does not check out stops the start, not a page later.
    fn check(&self) -> Result<(), String> {
        self.info.check()?;
        self.landing.check()?;
        self.check_assets()?;
        self.info.landing.check(&self.landing)?;
        let mut errors = Vec::new();
        for front in [
            Front::Unclaimed,
            Front::Passkeys,
            Front::Nomen,
            Front::Hosted,
        ] {
            let ctx = Ctx::checking(self);
            crate::web::landing_for(self, &ctx, front, "https://hub.example.org");
            errors.extend(
                ctx.errors()
                    .into_iter()
                    .map(|e| format!("content/landing.json ({}): {e}", front.key())),
            );
        }
        for (name, page) in &self.pages {
            for front in markdown::DOC_FRONTS {
                for switch in markdown::SWITCHES {
                    let ctx = Ctx::checking(self)
                        .with("mcp", "https://hub.example.org/mcp")
                        .with("updated", page.updated.clone())
                        .with("retention_days", "90");
                    page.render(&ctx, &[front, switch]);
                    errors.extend(
                        ctx.errors()
                            .into_iter()
                            .map(|e| format!("content/{name}.md: {e}")),
                    );
                }
            }
        }
        // Each message with what fills it where it is sent.
        for (what, template, var, value, inline) in [
            (
                "email.subject",
                &self.messages.email.subject,
                "link",
                "https://hub.example.org/auth/email#x",
                false,
            ),
            (
                "email.body",
                &self.messages.email.body,
                "link",
                "https://hub.example.org/auth/email#x",
                false,
            ),
            (
                "welcome.title",
                &self.messages.welcome.title,
                "hub",
                "https://hub.example.org",
                false,
            ),
            (
                "welcome.body",
                &self.messages.welcome.body,
                "hub",
                "https://hub.example.org",
                false,
            ),
            (
                "page_footer",
                &self.messages.page_footer,
                "updated",
                "1 January 2026",
                true,
            ),
        ] {
            let ctx = Ctx::checking(self).with(var, value);
            if inline {
                ctx.inline(template);
            } else {
                ctx.plain(template);
            }
            errors.extend(
                ctx.errors()
                    .into_iter()
                    .map(|e| format!("content/messages.json {what}: {e}")),
            );
        }
        for (what, template, vars, inline) in self.message_checks() {
            let mut ctx = Ctx::checking(self);
            for (var, value) in vars {
                ctx.set(var, value);
            }
            if inline {
                ctx.inline(template);
            } else {
                ctx.plain(template);
            }
            errors.extend(
                ctx.errors()
                    .into_iter()
                    .map(|e| format!("content/messages.json {what}: {e}")),
            );
        }
        let ctx = Ctx::checking(self);
        ctx.plain(&self.info.mcp.instructions);
        errors.extend(
            ctx.errors()
                .into_iter()
                .map(|e| format!("site.json mcp.instructions: {e}")),
        );
        errors.dedup();
        match errors.first() {
            None => Ok(()),
            Some(_) if errors.len() == 1 => Err(errors.remove(0)),
            Some(_) => Err(format!("{} (and {} more)", errors[0], errors.len() - 1)),
        }
    }

    fn check_assets(&self) -> Result<(), String> {
        let names = &self.info.assets;
        for (what, name, kinds, inline) in [
            ("logo", &names.logo, &["svg"][..], true),
            ("mascot", &names.mascot, &["svg"][..], true),
            ("diagram", &names.diagram, &["svg"][..], true),
            ("icon", &names.icon, &["svg", "png"][..], false),
            ("video", &names.video, &["mp4", "webm"][..], false),
            (
                "poster",
                &names.poster,
                &["png", "jpg", "jpeg", "webp"][..],
                false,
            ),
        ] {
            let Some(name) = name else { continue };
            let asset = self
                .assets
                .get(name)
                .ok_or_else(|| format!("site.json assets.{what}: there is no assets/{name}"))?;
            if !kinds.contains(&asset.extension()) {
                return Err(format!(
                    "site.json assets.{what}: assets/{name} must be {}",
                    kinds.join(" or ")
                ));
            }
            if inline {
                assets::check_inline_svg(&asset.text())
                    .map_err(|why| format!("assets/{name} ({what}, drawn inline): {why}"))?;
            }
        }
        if names.poster.is_some() && names.video.is_none() {
            return Err("site.json assets.poster is for the video; name assets.video too".into());
        }
        Ok(())
    }

    /// The product's name, as every page and message says it.
    pub fn name(&self) -> &str {
        &self.info.product.name
    }

    /// What a sign-in provider is called on this site's pages: its `sign_in.labels` entry,
    /// else `fallback` (the provider's own label from the hub's config).
    pub fn label<'a>(&'a self, provider: &str, fallback: &'a str) -> &'a str {
        self.info
            .sign_in
            .labels
            .get(provider)
            .map(String::as_str)
            .unwrap_or(fallback)
    }

    /// Where a provider's button goes among the others: its place in `sign_in.order`;
    /// providers it leaves out keep the config's order, after the ones it names.
    pub fn sign_in_rank(&self, provider: &str) -> usize {
        self.info
            .sign_in
            .order
            .iter()
            .position(|name| name == provider)
            .unwrap_or(usize::MAX)
    }

    /// `/assets/<name>?v=<hash>` for one of the site's assets.
    pub fn asset_url(&self, name: &str) -> Option<String> {
        self.assets.get(name).map(Asset::url)
    }

    /// The asset `site.json` names for `what`, if it names one.
    pub fn named(&self, what: Option<&String>) -> Option<&Asset> {
        what.and_then(|name| self.assets.get(name))
    }

    /// The whole stylesheet: the tokens, the fonts, the hub's own rules and the landing
    /// page's rail for this site's sections.
    pub fn stylesheet(&self) -> &str {
        &self.style.css
    }

    /// The stylesheet's strong ETag: it is revalidated, so a site read again restyles
    /// every page at once.
    pub fn stylesheet_etag(&self) -> &str {
        &self.style.etag
    }

    /// Whether the pages load fonts (their CSP then allows `font-src 'self'`).
    pub fn has_fonts(&self) -> bool {
        self.style.fonts
    }

    /// The colour a phone paints its bar in for the installed app.
    pub fn theme_color(&self) -> &str {
        &self.style.theme_color
    }

    /// The installed app's manifest.
    pub fn manifest(&self) -> String {
        let icons: Vec<serde_json::Value> = self
            .named(self.info.assets.icon.as_ref())
            .map(|icon| {
                serde_json::json!({
                    "src": icon.url(),
                    "sizes": if icon.extension() == "svg" { "any" } else { "512x512" },
                    "type": icon.mime,
                    "purpose": "any",
                })
            })
            .into_iter()
            .collect();
        let manifest = serde_json::json!({
            "name": self.info.product.name,
            "short_name": self.info.product.short_name.as_deref().unwrap_or(&self.info.product.name),
            "description": self.info.product.description,
            "start_url": "/approvals",
            "scope": "/",
            "display": "standalone",
            "background_color": self.style.background_color,
            "theme_color": self.style.theme_color,
            "icons": icons,
        });
        serde_json::to_string_pretty(&manifest).unwrap_or_default()
    }

    /// The service worker with this site's name and icon in its notifications.
    pub fn service_worker(&self) -> String {
        let icon = self
            .named(self.info.assets.icon.as_ref())
            .map(Asset::url)
            .unwrap_or_default();
        include_str!("web/assets/sw.js")
            .replace(
                "@TITLE@",
                &serde_json::to_string(&self.info.product.name).unwrap_or_default(),
            )
            .replace("@ICON@", &serde_json::to_string(&icon).unwrap_or_default())
    }

    /// The MCP server's name and instructions as AI clients see them.
    pub fn mcp_info(&self) -> rmcp::model::Implementation {
        let mcp = &self.info.mcp;
        let mut info =
            rmcp::model::Implementation::new(mcp.name.clone(), env!("CARGO_PKG_VERSION"));
        if let Some(title) = &mcp.title {
            info = info.with_title(Ctx::new(self).plain(title));
        }
        info
    }

    pub fn mcp_instructions(&self) -> String {
        Ctx::new(self).plain(&self.info.mcp.instructions)
    }

    /// The sign-in email: subject and body, with the link in it.
    pub fn sign_in_email(&self, link: &str) -> (String, String) {
        let ctx = Ctx::new(self).with("link", link);
        (
            ctx.plain(&self.messages.email.subject),
            ctx.plain(&self.messages.email.body),
        )
    }

    /// The getting-started ticket a new board shows: title and steps, with this hub's
    /// address in them.
    pub fn welcome(&self, public_url: &str) -> (String, String) {
        let ctx = Ctx::new(self).with("hub", public_url.trim_end_matches('/'));
        (
            ctx.plain(&self.messages.welcome.title),
            ctx.plain(&self.messages.welcome.body),
        )
    }

    /// The hub `devagotchi login` signs in at when none is named: the site's
    /// `links.hosted_hub`.
    pub fn hosted_hub(&self) -> Option<&str> {
        self.info.links.get("hosted_hub").map(String::as_str)
    }
}

fn read_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>, String> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("{}: {error}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn read_text(file: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(file).map_err(|error| format!("{}: {error}", file.display()))?;
    if meta.len() > MAX_TEXT_BYTES {
        return Err(format!(
            "{} is {} bytes; a text file of a site is at most {MAX_TEXT_BYTES}",
            file.display(),
            meta.len()
        ));
    }
    std::fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))
}

fn parse_json<T: serde::de::DeserializeOwned>(path: &str, text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|error| format!("{path}: {error}"))
}

/// `over` on top of `base`: objects merge key by key, anything else replaces.
fn merge_json(base: &str, over: &str) -> Result<String, String> {
    fn merge(base: &mut serde_json::Value, over: serde_json::Value) {
        match (base, over) {
            (serde_json::Value::Object(base), serde_json::Value::Object(over)) => {
                for (key, value) in over {
                    match base.get_mut(&key) {
                        Some(slot) => merge(slot, value),
                        None => {
                            base.insert(key, value);
                        }
                    }
                }
            }
            (slot, value) => *slot = value,
        }
    }
    let mut base: serde_json::Value = serde_json::from_str(base).map_err(|e| e.to_string())?;
    let over: serde_json::Value = serde_json::from_str(over).map_err(|e| e.to_string())?;
    if !over.is_object() {
        return Err("must be a JSON object".into());
    }
    merge(&mut base, over);
    Ok(base.to_string())
}
