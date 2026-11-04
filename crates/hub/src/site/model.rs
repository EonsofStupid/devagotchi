//! What the site's JSON files hold, and the checks a value must pass before the hub
//! starts. Every struct refuses fields it does not know, so a misspelt key is an error at
//! start, never a silent default.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// What `/` shows someone not signed in, and which copy a variant picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Front {
    /// A hub with its own accounts whose owner cannot sign in yet: claim it.
    Unclaimed,
    /// A hub with its own accounts: sign in with a passkey.
    Passkeys,
    /// A hub that signs people in through Nomen (`auth.mode nomen` with `web`).
    Nomen,
    /// A hosted hub (`tenancy.mode multi`): accounts are made on first connect.
    Hosted,
}

impl Front {
    pub fn key(self) -> &'static str {
        match self {
            Front::Unclaimed => "unclaimed",
            Front::Passkeys => "passkeys",
            Front::Nomen => "nomen",
            Front::Hosted => "hosted",
        }
    }
}

/// The keys a variant may have: one per front, `other` (a hub with no browser sign-in, on
/// the docs page) and `default`.
const VARIANT_KEYS: [&str; 6] = [
    "default",
    "unclaimed",
    "passkeys",
    "nomen",
    "hosted",
    "other",
];

/// Copy that may differ by front: one value, or `{ "default": …, "hosted": …, … }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Variants<T> {
    One(T),
    By(BTreeMap<String, T>),
}

impl<T: Default> Default for Variants<T> {
    fn default() -> Self {
        Variants::One(T::default())
    }
}

impl<T> Variants<T> {
    /// The value for `front`: its own, else `default`.
    pub fn pick(&self, front: &str) -> Option<&T> {
        match self {
            Variants::One(value) => Some(value),
            Variants::By(map) => map.get(front).or_else(|| map.get("default")),
        }
    }

    fn check(&self, what: &str) -> Result<(), String> {
        if let Variants::By(map) = self {
            for key in map.keys() {
                if !VARIANT_KEYS.contains(&key.as_str()) {
                    return Err(format!(
                        "{what}: {key:?} is not one of {}",
                        VARIANT_KEYS.join(", ")
                    ));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- site.json

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteFile {
    pub product: Product,
    pub operator: Operator,
    /// Public links by name: `{link:<name>}` in the copy. `hosted_hub` is the hub
    /// `devagotchi login` signs in at by default.
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    #[serde(default)]
    pub sign_in: SignIn,
    pub landing: Layout,
    pub mcp: Mcp,
    /// The plans the landing page's Plans section shows.
    #[serde(default)]
    pub plans: Vec<Plan>,
    /// The services the landing page's Services section shows.
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub early_access: EarlyAccess,
    #[serde(default)]
    pub assets: AssetNames,
    /// What the landing page's `SoftwareApplication` JSON-LD says beyond the product's name
    /// and description (`discovery.rs`). Left out, it says only those.
    #[serde(default)]
    pub software: Software,
    /// The network this site belongs to and the free offerings an account links to when its
    /// person says yes in the chat (`offers.rs`, `src/offers.rs`). Left out: none.
    #[serde(default)]
    pub offers: Option<super::offers::Offers>,
}

/// The facts a search engine reads about the software, as schema.org words them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Software {
    /// schema.org `applicationCategory`: `DeveloperApplication`, `BusinessApplication`…
    #[serde(default)]
    pub category: Option<String>,
    /// schema.org `operatingSystem`: the systems it runs on, as a list.
    #[serde(default)]
    pub operating_systems: Vec<String>,
    /// What it costs: a schema.org `Offer`, only when the price is one number.
    #[serde(default)]
    pub offer: Option<Offer>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    /// A number as text: `0` for free.
    pub price: String,
    /// ISO 4217: `USD`.
    pub currency: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Product {
    /// The product's name: the bar, every page's title, the email, the passkey prompt.
    pub name: String,
    /// The installed app's name under its icon; `name` when absent.
    #[serde(default)]
    pub short_name: Option<String>,
    /// A line that says what it is: the landing page's title.
    pub tagline: String,
    /// One sentence for the installed app's description.
    #[serde(default)]
    pub description: String,
    /// The marketing domain, as `{domain}` in the copy: a host name. The public pages'
    /// canonical addresses and the sitemap are `https://<domain>/…` when it is set
    /// (`discovery::site_base`).
    #[serde(default)]
    pub domain: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operator {
    /// Who runs the service, as the privacy policy and terms name them.
    pub name: String,
    /// Where to reach them: a URL (https, or mailto:).
    #[serde(default)]
    pub contact: String,
    #[serde(default)]
    pub support_email: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignIn {
    /// The order sign-in buttons appear in, by provider name (`google`, `github`, an
    /// OpenID Connect provider's name); the ones left out follow, in the config's order.
    #[serde(default)]
    pub order: Vec<String>,
    /// What a provider is called on the pages, by its name (`nomen` for Nomen sign-in):
    /// `{label:<name>}` in the copy, and its button's label.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// The landing page's sections after the hero, in order, each on or off.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub sections: Vec<SectionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionEntry {
    /// A built-in section (`how`, `compare`, `features`, `network`, `plans`, `services`,
    /// `connect`, `self-host`, `next`, `faq`, `get-started`) or one of
    /// `content/landing.json`'s `custom` sections. Also its anchor on the page. `network`
    /// shows only while `offers` does.
    pub id: String,
    #[serde(default = "yes")]
    pub on: bool,
}

fn yes() -> bool {
    true
}

/// The built-in sections, by id.
pub const SECTIONS: [&str; 11] = [
    "how",
    "compare",
    "features",
    "network",
    "plans",
    "services",
    "connect",
    "self-host",
    "next",
    "faq",
    "get-started",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mcp {
    /// The MCP server's name (`serverInfo.name`): `[a-z0-9][a-z0-9-]*`.
    pub name: String,
    /// Its display title (`serverInfo.title`), when the site gives one.
    #[serde(default)]
    pub title: Option<String>,
    /// What every AI client is told about the hub when it connects.
    pub instructions: String,
}

/// A plan: what the Plans section shows, and what a hosted hub holds each tenant on that
/// plan to (`accounts/access.rs`). `limits` names only what the hub counts and enforces
/// ([`LIMIT_KEYS`]); a limit a plan leaves out is the hub config's (`account_boards.
/// free_limit`, the machine quota, `tenancy.unsecured`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub id: String,
    pub name: String,
    /// The price as it reads: "Free", "$12 a month". Text, never computed.
    pub price: String,
    /// The plan a new tenant starts on; without one marked, the first plan.
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub limits: BTreeMap<String, Limit>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub cta: Option<Action>,
}

/// The limits a plan may set: each one the hub counts per tenant and refuses past
/// (`access.rs`). `projects`: boards a tenant binds (`board_bind`); `machines`: machines it
/// adds (`machine_add`, Add a machine), enrolled or with a live code; `clients`: AI apps
/// and terminals connected at once (each live refresh-token family).
pub const LIMIT_KEYS: [&str; 3] = ["projects", "machines", "clients"];

/// Limit names an earlier site could word in `content/landing.json` but no plan may set
/// any more, because the hub does not count them: their words are accepted, never drawn.
const RETIRED_LIMIT_WORDS: [&str; 1] = ["idle_days"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Limit {
    Count(u32),
    Word(Unlimited),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unlimited {
    Unlimited,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    pub text: String,
    #[serde(default)]
    pub href: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Whether anyone may sign up now, or people join an early-access list first.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EarlyAccess {
    pub mode: SignUp,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub cta: Option<Action>,
}

impl Default for EarlyAccess {
    fn default() -> Self {
        Self {
            mode: SignUp::Open,
            note: String::new(),
            cta: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignUp {
    Open,
    EarlyAccess,
}

/// Which of `assets/` the pages use. Each is optional: no video, no Watch it set up.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetNames {
    /// The bar's mark, drawn inline (SVG).
    #[serde(default)]
    pub logo: Option<String>,
    /// The app's icon and favicon (SVG or PNG).
    #[serde(default)]
    pub icon: Option<String>,
    /// The picture beside the landing page's closing call, drawn inline (SVG).
    #[serde(default)]
    pub mascot: Option<String>,
    /// How it fits together, drawn inline (SVG) in the hero and How it works.
    #[serde(default)]
    pub diagram: Option<String>,
    /// The setup video (MP4 or WebM) and its poster.
    #[serde(default)]
    pub video: Option<String>,
    #[serde(default)]
    pub poster: Option<String>,
}

impl SiteFile {
    pub fn check(&self) -> Result<(), String> {
        let bad = |what: &str, why: &str| Err(format!("site.json {what}: {why}"));
        let name = self.product.name.trim();
        if name.is_empty() || name.len() > 64 {
            return bad("product.name", "1 to 64 characters");
        }
        if self.product.tagline.trim().is_empty() {
            return bad("product.tagline", "say what it is in a line");
        }
        // Where the public pages live (`discovery::site_base`): a host name, no scheme or path.
        let domain = self.product.domain.trim();
        if !domain.is_empty()
            && (domain.len() > 253
                || domain.starts_with(['.', '-'])
                || !domain
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
        {
            return bad("product.domain", "a host name, like example.org, with no https:// or path");
        }
        if self.operator.name.trim().is_empty() {
            return bad("operator.name", "who runs the service");
        }
        if !self.operator.contact.is_empty() && !safe_href(&self.operator.contact) {
            return bad("operator.contact", "an https: or mailto: URL");
        }
        if let Some(email) = &self.operator.support_email
            && (!email.contains('@') || email.contains(char::is_whitespace))
        {
            return bad("operator.support_email", "an email address");
        }
        for (key, url) in &self.links {
            if !valid_key(key) {
                return bad(&format!("links.{key}"), "names are [a-z0-9_-]");
            }
            if !(url.starts_with("https://") || url.starts_with("mailto:") || loopback(url)) {
                return bad(&format!("links.{key}"), "an https: or mailto: URL");
            }
        }
        for (key, label) in &self.sign_in.labels {
            if !valid_key(key) || label.trim().is_empty() {
                return bad(
                    &format!("sign_in.labels.{key}"),
                    "a provider's name and its label",
                );
            }
        }
        let mcp = &self.mcp.name;
        if mcp.is_empty()
            || mcp.len() > 64
            || !mcp.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            || !mcp
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return bad("mcp.name", "[a-z0-9][a-z0-9-]*, at most 64");
        }
        if self.mcp.instructions.trim().is_empty() || self.mcp.instructions.len() > 16 * 1024 {
            return bad("mcp.instructions", "1 byte to 16 KiB");
        }
        let mut ids = BTreeSet::new();
        if self.plans.iter().filter(|plan| plan.default).count() > 1 {
            return bad("plans", "at most one plan is marked \"default\": true");
        }
        for plan in &self.plans {
            if !valid_key(&plan.id) || !ids.insert(plan.id.as_str()) {
                return bad(&format!("plans {}", plan.id), "ids are unique, [a-z0-9_-]");
            }
            if plan.name.trim().is_empty() || plan.price.trim().is_empty() {
                return bad(&format!("plans {}", plan.id), "a name and a price");
            }
            for key in plan.limits.keys() {
                if !LIMIT_KEYS.contains(&key.as_str()) {
                    return bad(
                        &format!("plans {}.limits.{key}", plan.id),
                        &format!(
                            "the hub enforces only {}; a plan names no limit it does not count",
                            LIMIT_KEYS.join(", ")
                        ),
                    );
                }
            }
            if let Some(cta) = &plan.cta {
                cta.check(&format!("site.json plans {}.cta", plan.id))?;
            }
        }
        for service in &self.services {
            if service.name.trim().is_empty() {
                return bad("services", "each has a name");
            }
            if let Some(href) = &service.href
                && !safe_href(href)
            {
                return bad(
                    &format!("services {}", service.name),
                    "href is a path, #anchor, https: or mailto:",
                );
            }
        }
        if let Some(cta) = &self.early_access.cta {
            cta.check("site.json early_access.cta")?;
        }
        if let Some(offers) = &self.offers {
            offers.check()?;
        }
        if let Some(offer) = &self.software.offer {
            if offer.price.parse::<f64>().map_or(true, |p| !p.is_finite() || p < 0.0) {
                return bad("software.offer.price", "a number as text, \"0\" for free");
            }
            if offer.currency.len() != 3 || !offer.currency.bytes().all(|b| b.is_ascii_uppercase()) {
                return bad("software.offer.currency", "an ISO 4217 code such as USD");
            }
        }
        Ok(())
    }
}

impl SiteFile {
    /// The site's offers, while they show (`offers.rs`): on, with an offering.
    pub fn offers(&self) -> Option<&super::offers::Offers> {
        self.offers.as_ref().filter(|offers| offers.shown())
    }

    /// The landing page's sections that show, in order: those that are on, and `network`
    /// only while the offers show.
    pub fn sections(&self) -> impl Iterator<Item = &str> {
        let offers = self.offers().is_some();
        self.landing
            .on()
            .filter(move |id| *id != "network" || offers)
    }
}

impl Layout {
    pub fn check(&self, landing: &Landing) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for entry in &self.sections {
            if !seen.insert(entry.id.as_str()) {
                return Err(format!(
                    "site.json landing.sections: {} is listed twice",
                    entry.id
                ));
            }
            if !SECTIONS.contains(&entry.id.as_str()) && !landing.custom.contains_key(&entry.id) {
                return Err(format!(
                    "site.json landing.sections: {} is neither a built-in section ({}) nor in content/landing.json custom",
                    entry.id,
                    SECTIONS.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// The sections that are on, in order.
    pub fn on(&self) -> impl Iterator<Item = &str> {
        self.sections.iter().filter(|s| s.on).map(|s| s.id.as_str())
    }
}

// ---------------------------------------------------------------- content/landing.json

/// Every landing section's copy. Text fields take the inline markup of `text.rs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Landing {
    /// The page's `<title>` by front.
    pub title: Variants<String>,
    /// The rail's name for screen readers.
    pub rail_label: String,
    /// The Sign in button's label by front (`{sign_in}` in the copy).
    pub sign_in: Variants<String>,
    pub hero: Hero,
    /// What a new hub shows under the hero: how to claim it.
    pub claim: StepsBand,
    pub how: StepsBand,
    /// Another product beside this one, row by row: a site may add one; the built-in
    /// site has none.
    #[serde(default)]
    pub compare: Compare,
    pub features: Features,
    pub plans: PlansCopy,
    pub services: Band,
    pub connect: Connect,
    pub self_host: SelfHost,
    pub next: Band,
    pub faq: Faq,
    pub closing: Closing,
    pub footer: Footer,
    pub popups: Popups,
    /// The words a newcomer may not know, by key: `{term:<key>}` shows the word with its
    /// definition as a tooltip.
    pub glossary: BTreeMap<String, Term>,
    /// Sections of the site's own, by id, placed by `site.json` like the built-in ones.
    #[serde(default)]
    pub custom: BTreeMap<String, Band>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hero {
    pub rail: String,
    pub title: String,
    /// A line above the title on a new hub ("Waiting for its owner").
    #[serde(default)]
    pub status: Variants<String>,
    pub lead: Variants<String>,
    pub actions: Variants<Vec<Action>>,
    #[serde(default)]
    pub note: Variants<String>,
    #[serde(default)]
    pub video_label: String,
    #[serde(default)]
    pub video_caption: String,
}

/// A link that looks like a button.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub label: String,
    pub href: String,
    #[serde(default)]
    pub primary: bool,
}

impl Action {
    fn check(&self, what: &str) -> Result<(), String> {
        if self.label.trim().is_empty() {
            return Err(format!("{what}: a label"));
        }
        // A placeholder (`{sign_in_url}`, `{link:…}`) is checked where it is drawn.
        if !self.href.starts_with('{') && !safe_href(&self.href) {
            return Err(format!(
                "{what}: href {:?} is a path, #anchor, https: or mailto:",
                self.href
            ));
        }
        Ok(())
    }
}

/// A band of numbered steps (How it works, and claiming a new hub).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepsBand {
    #[serde(default)]
    pub rail: String,
    pub title: String,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub meta: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub title: String,
    pub text: Variants<String>,
    /// Lines shown as a terminal under the text; `# …` is a comment.
    #[serde(default)]
    pub terminal: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compare {
    pub rail: String,
    pub title: String,
    pub intro: String,
    /// The first column's heading, read by screen readers only.
    pub what: String,
    /// The other product's column and this one's.
    pub them: String,
    pub us: String,
    /// Read by screen readers after a row's citation: "the evidence for".
    pub evidence_for: String,
    pub rows: Vec<CompareRow>,
    /// Ends every evidence popup's source line.
    pub source_note: String,
    /// Under the table.
    pub footnote: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareRow {
    pub what: String,
    pub them: String,
    pub cite: String,
    pub us: String,
    /// What the cited source shows, a paragraph each.
    pub evidence: Vec<String>,
    pub source: Action,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Features {
    pub rail: String,
    pub title: String,
    pub pillars: Vec<Pillar>,
    #[serde(default)]
    pub phone: Option<Phone>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pillar {
    pub title: String,
    pub items: Vec<Item>,
}

/// A term and what it means: a `<dt>` and its `<dd>`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub term: String,
    pub text: Variants<String>,
}

/// The approval drawn on a phone beside the features.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phone {
    pub asker: String,
    pub title: String,
    pub what: String,
    pub approve: String,
    pub deny: String,
    pub caption: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlansCopy {
    pub rail: String,
    pub title: String,
    #[serde(default)]
    pub intro: String,
    /// How each limit reads: `{ "one": "{n} project", "other": "{n} projects",
    /// "unlimited": "Unlimited projects" }`.
    pub limits: BTreeMap<String, LimitWords>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitWords {
    pub one: String,
    pub other: String,
    pub unlimited: String,
}

/// A plain band: a title, an intro, and a list. The Services section, the Where it's
/// going section and a site's own sections.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Band {
    #[serde(default)]
    pub rail: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub intro: String,
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connect {
    pub rail: String,
    pub title: String,
    pub intro: Variants<String>,
    pub clients: Variants<Vec<Item>>,
    pub meta: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelfHost {
    pub rail: String,
    pub title: String,
    pub intro: String,
    pub terminal: Vec<String>,
    pub facts: Vec<String>,
    #[serde(default)]
    pub hosted: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Faq {
    pub rail: String,
    pub title: String,
    pub questions: Vec<Question>,
    #[serde(default)]
    pub meta: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub q: String,
    pub a: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Closing {
    pub rail: String,
    pub title: String,
    pub lead: Variants<String>,
    pub actions: Variants<Vec<Action>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Footer {
    pub label: String,
    pub links: Vec<Action>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Popups {
    pub claim: ClaimPopup,
    pub tools: ToolsPopup,
    pub watch: WatchPopup,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimPopup {
    pub title: String,
    pub steps: Vec<Item>,
    #[serde(default)]
    pub paragraphs: Vec<String>,
    #[serde(default)]
    pub source: String,
}

/// The tools popup's words; the tools themselves are the hub's (`web/landing/popups.rs`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsPopup {
    pub title: String,
    pub intro: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchPopup {
    pub title: String,
    /// The button that opens it.
    pub button: String,
    pub caption: String,
    #[serde(default)]
    pub steps: Vec<Item>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Term {
    /// What `{term:<key>}` shows when it names no text of its own.
    pub word: String,
    pub definition: String,
}

impl Landing {
    pub fn check(&self) -> Result<(), String> {
        let what = |field: &str| format!("content/landing.json {field}");
        self.title.check(&what("title"))?;
        self.sign_in.check(&what("sign_in"))?;
        self.hero.status.check(&what("hero.status"))?;
        self.hero.lead.check(&what("hero.lead"))?;
        self.hero.note.check(&what("hero.note"))?;
        self.hero.actions.check(&what("hero.actions"))?;
        for (band, name) in [(&self.claim, "claim"), (&self.how, "how")] {
            for step in &band.steps {
                step.text.check(&what(&format!("{name}.steps")))?;
            }
        }
        for pillar in &self.features.pillars {
            for item in &pillar.items {
                item.text.check(&what("features.pillars"))?;
            }
        }
        self.connect.intro.check(&what("connect.intro"))?;
        self.connect.clients.check(&what("connect.clients"))?;
        self.closing.lead.check(&what("closing.lead"))?;
        self.closing.actions.check(&what("closing.actions"))?;
        for (key, words) in &self.plans.limits {
            if RETIRED_LIMIT_WORDS.contains(&key.as_str()) {
                continue;
            }
            if !LIMIT_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "{}: a limit is one of {}",
                    what(&format!("plans.limits.{key}")),
                    LIMIT_KEYS.join(", ")
                ));
            }
            for text in [&words.one, &words.other] {
                if !text.contains("{n}") {
                    return Err(format!(
                        "{}: say {{n}} where the number goes",
                        what(&format!("plans.limits.{key}"))
                    ));
                }
            }
        }
        for key in self.glossary.keys() {
            if !valid_key(key) {
                return Err(what(&format!("glossary.{key}: keys are [a-z0-9_-]")));
            }
        }
        for key in self.custom.keys() {
            // Also an id and a CSS name on the page: it starts with a letter.
            if !valid_key(key)
                || !key.starts_with(|c: char| c.is_ascii_lowercase())
                || SECTIONS.contains(&key.as_str())
                || key == "top"
            {
                return Err(what(&format!(
                    "custom.{key}: an id of its own, a letter then [a-z0-9_-], not a built-in section's"
                )));
            }
        }
        if self.compare.rows.len() > 64 {
            return Err(what("compare.rows: at most 64"));
        }
        for row in &self.compare.rows {
            row.source.check(&what("compare.rows source"))?;
        }
        for link in &self.footer.links {
            link.check(&what("footer.links"))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- content/messages.json

/// Text the hub sends or writes outside its pages. Plain text with placeholders.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Messages {
    pub email: Email,
    pub welcome: Welcome,
    /// Under every public page (inline markup; `{updated}` is the page's date).
    pub page_footer: String,
    /// Early access and plans on a hosted hub (`accounts/access.rs`).
    pub access: AccessCopy,
    /// The operator page (`web/operator.rs`).
    pub operator: OperatorCopy,
    /// The operator's analytics page (`web/analytics.rs`).
    pub analytics: AnalyticsCopy,
    /// The workflow page (`web/workflow.rs`) and what a workflow's steps stand at.
    pub workflow: super::workflows::WorkflowCopy,
    /// The sign-in page of a hosted hub (`web/sign_in.rs`). Plain text.
    #[serde(default)]
    pub sign_in: SignInMessages,
}

/// The sign-in page's words (`web/sign_in.rs`): Google, or an email and a password.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SignInMessages {
    pub title: String,
    pub or: String,
    pub tab_in: String,
    pub tab_up: String,
    pub email: String,
    pub password: String,
    pub submit_in: String,
    pub submit_up: String,
    pub forgot: String,
    pub wrong: String,
    pub taken: String,
    pub short: String,
    pub slow_down: String,
    pub reset_sent: String,
    pub reset_unavailable: String,
    pub set_password: String,
    /// The page when a Google sign-in's address already has an account: `{email}`.
    pub link_title: String,
    pub link: String,
    pub keep_separate: String,
}

impl Default for SignInMessages {
    fn default() -> Self {
        let s = |text: &str| text.to_string();
        Self {
            title: s("Sign in"),
            or: s("or"),
            tab_in: s("Sign in"),
            tab_up: s("Sign up"),
            email: s("Email"),
            password: s("Password"),
            submit_in: s("Sign in"),
            submit_up: s("Create account"),
            forgot: s("Forgot password?"),
            wrong: s("Email or password is wrong."),
            taken: s("That email already has an account. Sign in instead."),
            short: s("Use at least 8 characters."),
            slow_down: s("Too many tries. Wait a few minutes."),
            reset_sent: s("Check your email for a sign-in link."),
            reset_unavailable: s("Password reset isn't available yet \u{2014} sign in with Google."),
            set_password: s("Set a password"),
            link_title: s("{email} already has an account. Link Google to it?"),
            link: s("Link"),
            keep_separate: s("Keep separate"),
        }
    }
}

/// The operator page's words. Inline markup where it says so; `{tenant}` is a tenant's id.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorCopy {
    /// The page's title and heading.
    pub title: String,
    /// On the operator's account page, the way to it (inline markup).
    pub account_link: String,
    /// What sign-up is now, by `early_access.mode` (inline markup).
    pub status_open: String,
    pub status_early_access: String,
    /// How to change it: `site.json`, then the site push (inline markup).
    pub how_to_change: String,
    /// What a done step says (inline markup): `{n}`, how many were let in.
    pub done_admitted: String,
    pub done_plan: String,
    /// Let in pressed with nobody checked.
    pub none_picked: String,
    /// A post from another site, refused.
    pub not_from_here: String,
    pub waiting_title: String,
    pub waiting_empty: String,
    pub admitted_title: String,
    pub admitted_empty: String,
    pub columns: OperatorColumns,
    pub secured: String,
    pub not_secured: String,
    pub no_sign_ins: String,
    /// Where there is nothing to show (no activity, no AI app).
    pub never: String,
    /// The accessible names of a row's checkbox and plan picker: `{tenant}`.
    pub pick: String,
    pub plan_for: String,
    /// The buttons.
    pub admit: String,
    pub change_plan: String,
    /// When and by whom a tenant was let in: `{when}`, `{by}`.
    pub admitted_line: String,
    /// A limit's use: `{count}` and `{limit}` (the plan's, worded), or with no limit.
    pub usage_of: String,
    pub usage_unlimited: String,
}

/// The operator's analytics page (`web/analytics.rs`). Inline markup where it says so.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsCopy {
    /// The page's title and heading.
    pub title: String,
    /// On the operator page, the way to it (inline markup).
    pub operator_link: String,
    /// Under the heading: what is counted and how (inline markup).
    pub intro: String,
    /// A hub that counts nothing (inline markup).
    pub off: String,
    /// The range links' label, and each one: `{n}` days.
    pub range: String,
    pub range_days: String,
    /// The CSV link.
    pub export: String,
    /// How long raw visits are kept: `{days}` (inline markup).
    pub kept: String,
    /// Records dropped since the hub started: `{n}` (inline markup).
    pub dropped: String,
    pub traffic_title: String,
    /// The chart: its title, its summary (`{views}`, `{visitors}`, `{n}` days) and each
    /// day's tooltip (`{day}`, `{views}`, `{visitors}`).
    pub chart_title: String,
    pub chart_summary: String,
    pub chart_day: String,
    /// What opens the chart's numbers as a table.
    pub table_summary: String,
    pub pages_title: String,
    pub referrers_title: String,
    pub campaigns_title: String,
    pub devices_title: String,
    /// An empty list.
    pub none: String,
    pub funnel_title: String,
    /// Under the funnel's heading: `{n}` days (inline markup).
    pub funnel_intro: String,
    pub accounts_title: String,
    pub clients_title: String,
    pub sign_ins_title: String,
    /// Tenants waiting now: `{n}` (inline markup).
    pub waiting_now: String,
    /// Tenants with an operation lately: `{week}`, `{month}` (inline markup).
    pub active: String,
    /// The sign-in a passkey secured an account with (providers are named by the site).
    pub passkey: String,
    pub columns: AnalyticsColumns,
    pub steps: AnalyticsSteps,
    pub clients: AnalyticsClients,
    pub devices: AnalyticsDevices,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsColumns {
    pub day: String,
    pub views: String,
    pub visitors: String,
    pub page: String,
    pub referrer: String,
    pub source: String,
    pub medium: String,
    pub campaign: String,
    pub device: String,
    pub share: String,
    pub step: String,
    pub count: String,
    pub conversion: String,
    pub client: String,
    pub sign_in: String,
    pub accounts: String,
}

/// The funnel's steps.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsSteps {
    pub visitors: String,
    pub made: String,
    pub secured: String,
    pub admitted: String,
    pub machine: String,
    pub operation: String,
}

/// The AI client families an account is made by (`analytics::funnel::FAMILIES`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsClients {
    pub claude: String,
    pub chatgpt: String,
    pub grok: String,
    pub cursor: String,
    pub other: String,
}

/// The device classes (`analytics::visit::DEVICES`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsDevices {
    pub mobile: String,
    pub tablet: String,
    pub desktop: String,
}

/// The operator page's column headings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorColumns {
    pub tenant: String,
    pub joined: String,
    pub sign_ins: String,
    pub secured: String,
    pub made_by: String,
    pub active: String,
    pub plan: String,
    pub admitted: String,
    pub usage: String,
}

/// What a hosted hub says about early access and plans. `{since}` is a date, `{account}`
/// the person's account page.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessCopy {
    /// What every tool call but `whoami` answers while the tenant waits to be let in:
    /// `{since}`, `{account}`.
    pub waiting: String,
    /// The heading over that on a page (adding a machine, deciding an approval).
    pub waiting_title: String,
    /// The account page while it waits (inline markup): `{since}`.
    pub account_waiting: String,
    /// The account page once it is in (inline markup): `{since}`, `{plan}` (its name).
    pub account_admitted: String,
    /// A plan's limit reached: `{plan}`, `{limit}` and `{count}` (worded as the Plans
    /// section words them), `{plans}` (where the plans are: `links.plans`, else this hub's
    /// `/#plans`).
    pub limit: String,
    /// The heading over that on a page (an AI app past the plan's `clients`).
    pub limit_title: String,
    /// The email that says they are in: `{account}`.
    pub admitted_email: Email,
}

/// The sign-in email: `{link}` is the one-time link.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Email {
    pub subject: String,
    pub body: String,
}

/// The getting-started ticket on a new board: `{hub}` is this hub's address.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    pub title: String,
    pub body: String,
}

// ---------------------------------------------------------------- shared checks

pub(super) fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn loopback(url: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|prefix| {
            url.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
        })
}

/// Where a link on a page may go: a path on this hub, an anchor, https, or mail.
pub fn safe_href(href: &str) -> bool {
    let ok_chars = !href
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '<' || c == '>');
    ok_chars
        && ((href.starts_with('/') && !href.starts_with("//"))
            || href.starts_with('#')
            || href.starts_with("https://")
            || href.starts_with("mailto:")
            || loopback(href))
}
