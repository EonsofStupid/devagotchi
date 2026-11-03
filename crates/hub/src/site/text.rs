//! The markup a line of the site's copy may carry, and the placeholders it may name.
//!
//! - `**strong**`, `*emphasis*`, `` `code` ``, `[text](href)`; `\` takes the next
//!   character as it is. Everything else is text: `<`, `>` and a bare `&` are escaped, and
//!   an entity (`&quot;`, `&#39;`) stays as written. No HTML passes through, so copy can
//!   never add a script, a style or another origin to a page.
//! - `{product}`, `{tagline}`, `{domain}`, `{operator}`, `{operator_contact}`,
//!   `{support_email}`: from `site.json`. `{link:<name>}`: a public link.
//!   `{label:<provider>}`: what a sign-in provider is called.
//! - `{term:<key>}` or `{term:<key>|shown}`: a glossary word with its tooltip.
//!   `{pop:<claim|tools|watch>|label}`: a button that opens that popup.
//! - Whatever the page adds where it draws the copy: `{mcp}` (this hub's MCP address),
//!   `{sign_in}`, `{sign_in_url}`, `{updated}`, `{link}`, `{hub}`, `{n}`.
//!
//! A placeholder the copy names but nothing fills is an error when the site is checked at
//! start (`Ctx::checking`); drawing a page never fails.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use super::Site;

/// The popups a `{pop:…}` may open.
pub const POPUPS: [&str; 3] = ["claim", "tools", "watch"];

/// What one page's copy is drawn with.
pub struct Ctx<'a> {
    pub site: &'a Site,
    vars: Vec<(&'static str, String)>,
    /// How many times each glossary word was marked, for unique tooltip ids.
    terms: RefCell<BTreeMap<String, u32>>,
    /// The popups something on the page opens.
    opened: RefCell<BTreeSet<String>>,
    /// Tooltips drawn as plain words (inside a popup that repeats a row).
    plain_terms: Cell<bool>,
    errors: RefCell<Vec<String>>,
    checking: bool,
}

/// What a placeholder stands for.
enum Filled {
    Text(String),
    Html(String),
}

impl<'a> Ctx<'a> {
    pub fn new(site: &'a Site) -> Self {
        Self {
            site,
            vars: Vec::new(),
            terms: RefCell::new(BTreeMap::new()),
            opened: RefCell::new(BTreeSet::new()),
            plain_terms: Cell::new(false),
            errors: RefCell::new(Vec::new()),
            checking: false,
        }
    }

    /// A context that records every problem instead of drawing around it.
    pub fn checking(site: &'a Site) -> Self {
        Self {
            checking: true,
            ..Self::new(site)
        }
    }

    pub fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.set(name, value);
        self
    }

    pub fn set(&mut self, name: &'static str, value: impl Into<String>) {
        let value = value.into();
        match self.vars.iter_mut().find(|(key, _)| *key == name) {
            Some(slot) => slot.1 = value,
            None => self.vars.push((name, value)),
        }
    }

    pub fn errors(&self) -> Vec<String> {
        self.errors.borrow().clone()
    }

    pub fn is_checking(&self) -> bool {
        self.checking
    }

    fn error(&self, message: String) {
        if self.checking {
            self.errors.borrow_mut().push(message);
        }
    }

    /// Record a problem found while drawing (a missing popup, say).
    pub fn problem(&self, message: impl Into<String>) {
        self.error(message.into());
    }

    /// Whether something on the page opens popup `id`.
    pub fn opens(&self, id: &str) -> bool {
        self.opened.borrow().contains(id)
    }

    pub fn open(&self, id: &str) {
        self.opened.borrow_mut().insert(id.to_string());
    }

    pub fn plain_terms(&self, on: bool) {
        self.plain_terms.set(on);
    }

    /// `text` as HTML.
    pub fn inline(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 16);
        self.render(text, &mut out);
        out
    }

    /// `text` with its placeholders filled and its markup dropped, unescaped: for an
    /// attribute (escape it), an email, a title or the MCP display.
    pub fn plain(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            match c {
                '\\' if rest.len() > 1 => {
                    let next = rest[1..].chars().next().unwrap_or('\\');
                    out.push(next);
                    rest = &rest[1 + next.len_utf8()..];
                }
                '{' => match rest.find('}') {
                    Some(end) => {
                        match self.fill(&rest[1..end]) {
                            Some(Filled::Text(value)) => out.push_str(&value),
                            Some(Filled::Html(_)) => {
                                // A tooltip or a button reads as its words.
                                let shown = rest[1..end]
                                    .split_once('|')
                                    .map(|(_, shown)| shown.to_string())
                                    .unwrap_or_else(|| self.term_word(&rest[1..end]));
                                out.push_str(&shown);
                            }
                            None => out.push_str(&rest[..=end]),
                        }
                        rest = &rest[end + 1..];
                    }
                    None => {
                        self.error(format!("{{ with no }} in {text:?}"));
                        out.push('{');
                        rest = &rest[1..];
                    }
                },
                _ => {
                    out.push(c);
                    rest = &rest[c.len_utf8()..];
                }
            }
        }
        out
    }

    fn term_word(&self, spec: &str) -> String {
        spec.strip_prefix("term:")
            .and_then(|key| self.site.landing.glossary.get(key))
            .map(|term| term.word.clone())
            .unwrap_or_default()
    }

    fn render(&self, text: &str, out: &mut String) {
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            let step = match c {
                '\\' if rest.len() > 1 => {
                    let next = rest[1..].chars().next().unwrap_or('\\');
                    push_text(out, &next.to_string());
                    1 + next.len_utf8()
                }
                '*' if rest.starts_with("**") => match rest[2..].find("**") {
                    Some(end) => {
                        out.push_str("<strong>");
                        self.render(&rest[2..2 + end], out);
                        out.push_str("</strong>");
                        end + 4
                    }
                    None => {
                        out.push_str("**");
                        2
                    }
                },
                '*' => match rest[1..].find('*') {
                    Some(end) if end > 0 => {
                        out.push_str("<em>");
                        self.render(&rest[1..1 + end], out);
                        out.push_str("</em>");
                        end + 2
                    }
                    _ => {
                        out.push('*');
                        1
                    }
                },
                '`' => match rest[1..].find('`') {
                    Some(end) => {
                        out.push_str("<code>");
                        let inner = self.plain(&rest[1..1 + end]);
                        push_text(out, &inner);
                        out.push_str("</code>");
                        end + 2
                    }
                    None => {
                        out.push('`');
                        1
                    }
                },
                '[' => match link_at(rest) {
                    Some((label, href, used)) => {
                        let href = self.plain(href);
                        if super::model::safe_href(&href) {
                            out.push_str("<a href=\"");
                            push_attr(out, &href);
                            out.push_str("\">");
                            self.render(label, out);
                            out.push_str("</a>");
                        } else {
                            self.error(format!(
                                "link {href:?} is not a path, #anchor, https: or mailto:"
                            ));
                            self.render(label, out);
                        }
                        used
                    }
                    None => {
                        out.push('[');
                        1
                    }
                },
                '{' => match rest.find('}') {
                    Some(end) => {
                        match self.fill(&rest[1..end]) {
                            Some(Filled::Text(value)) => push_text(out, &value),
                            Some(Filled::Html(html)) => out.push_str(&html),
                            None => push_text(out, &rest[..=end]),
                        }
                        end + 1
                    }
                    None => {
                        self.error(format!("{{ with no }} in {text:?}"));
                        out.push('{');
                        1
                    }
                },
                '&' => {
                    let len = entity_len(rest);
                    if len > 0 {
                        out.push_str(&rest[..len]);
                        len
                    } else {
                        out.push_str("&amp;");
                        1
                    }
                }
                '<' => {
                    out.push_str("&lt;");
                    1
                }
                '>' => {
                    out.push_str("&gt;");
                    1
                }
                other => {
                    out.push(other);
                    other.len_utf8()
                }
            };
            rest = &rest[step..];
        }
    }

    /// What `{spec}` stands for; `None` (and an error when checking) for what nothing fills.
    fn fill(&self, spec: &str) -> Option<Filled> {
        let site = self.site;
        let info = &site.info;
        let text = |value: &str| Some(Filled::Text(value.to_string()));
        if let Some((_, value)) = self.vars.iter().find(|(name, _)| *name == spec) {
            return text(value);
        }
        let filled = match spec.split_once(':') {
            None => match spec {
                "product" => text(&info.product.name),
                "tagline" => text(&info.product.tagline),
                "domain" => text(&info.product.domain),
                "operator" => text(&info.operator.name),
                "operator_contact" => text(&info.operator.contact),
                "support_email" => info.operator.support_email.as_deref().and_then(text),
                _ => None,
            },
            Some(("link", name)) => info.links.get(name).and_then(|url| text(url)),
            Some(("label", name)) => info.sign_in.labels.get(name).and_then(|label| text(label)),
            Some(("term", rest)) => {
                let (key, shown) = match rest.split_once('|') {
                    Some((key, shown)) => (key, Some(shown)),
                    None => (rest, None),
                };
                site.landing.glossary.get(key).map(|term| {
                    let shown = self.inline(shown.unwrap_or(&term.word));
                    if self.plain_terms.get() {
                        return Filled::Html(shown);
                    }
                    let n = {
                        let mut counts = self.terms.borrow_mut();
                        let n = counts.entry(key.to_string()).or_insert(0);
                        *n += 1;
                        *n
                    };
                    let id = format!("tip-{key}-{n}");
                    Filled::Html(format!(
                        "<span class=\"term\" tabindex=\"0\" aria-describedby=\"{id}\">{shown}\
<span class=\"tip\" role=\"tooltip\" id=\"{id}\">{}</span></span>",
                        self.inline(&term.definition)
                    ))
                })
            }
            Some(("pop", rest)) => {
                let (id, label) = rest.split_once('|').unwrap_or((rest, ""));
                if POPUPS.contains(&id) && !label.trim().is_empty() {
                    self.open(id);
                    Some(Filled::Html(format!(
                        "<button type=\"button\" class=\"pop\" popovertarget=\"m-{id}\">{}</button>",
                        self.inline(label)
                    )))
                } else {
                    None
                }
            }
            Some(_) => None,
        };
        if filled.is_none() {
            self.error(format!("nothing fills {{{spec}}}"));
        }
        filled
    }
}

/// `[label](href)` at the start of `text`: the label, the href and the bytes used.
fn link_at(text: &str) -> Option<(&str, &str, usize)> {
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = &text[close + 1..];
    if !after.starts_with('(') {
        return None;
    }
    // The href ends at the parenthesis that closes it: `(a(b))` is `a(b)`.
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    Some((&text[1..close], &after[1..end], close + 1 + end + 1))
}

/// The length of the entity reference at the start of `text`, or 0.
fn entity_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut i = 1;
    let numeric = bytes.get(1) == Some(&b'#');
    if numeric {
        i = 2;
        if matches!(bytes.get(2), Some(b'x' | b'X')) {
            i = 3;
        }
    }
    let start = i;
    while i < bytes.len() && i < 34 && bytes[i].is_ascii_alphanumeric() {
        i += 1;
    }
    if i > start && bytes.get(i) == Some(&b';') {
        i + 1
    } else {
        0
    }
}

/// Text inside an element: `<`, `>` and `&` escaped.
pub fn push_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// A value inside a double-quoted attribute.
pub fn push_attr(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

pub fn attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_attr(&mut out, text);
    out
}

pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_text(&mut out, text);
    out
}
