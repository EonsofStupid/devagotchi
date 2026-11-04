//! The public pages (`content/*.md`): a small, strict Markdown. Each page opens with its
//! front matter and then has blocks separated by blank lines:
//!
//! ```text
//! ---
//! title: Privacy policy          the page's <title>
//! updated: 1 November 2026       {updated} in messages.json's page_footer
//! description: What we keep      its <meta name="description"> (optional; else the
//!                                product's description)
//! ---
//! # Heading, ## Heading, ### Heading
//! A paragraph: lines run together, with the inline markup of text.rs.
//! - a list item (two spaces indent a continuation line)
//! 1. a numbered item
//! | Head | Head |                a table: a head row, a |---| row, then rows
//! ::: passkeys hosted            only on these hubs (passkeys, hosted, nomen, other)
//! ::: analytics                  only where the hub counts visits (or: no-analytics)
//! :::                            ends it (blocks nest: each must hold)
//! ```

use super::text::Ctx;

/// The kinds of hub a docs page tells apart (`::: …`): a hub with its own accounts, a
/// hosted one, one that signs in through Nomen, any other.
pub const DOC_FRONTS: [&str; 4] = ["passkeys", "hosted", "nomen", "other"];

/// Whether the hub counts visits and its accounts' milestones (`crate::analytics`): a
/// privacy page says what it counts only where it does.
pub const SWITCHES: [&str; 2] = ["analytics", "no-analytics"];

#[derive(Debug, Clone)]
pub struct Page {
    pub title: String,
    pub updated: String,
    /// What a search result says about the page; empty for the product's description.
    pub description: String,
    body: String,
}

impl Page {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.replace("\r\n", "\n");
        let Some(rest) = text.strip_prefix("---\n") else {
            return Err("opens with front matter: ---, title: …, updated: …, ---".into());
        };
        let Some((head, body)) = rest.split_once("\n---\n") else {
            return Err("front matter has no closing ---".into());
        };
        let (mut title, mut updated, mut description) = (None, None, None);
        for line in head.lines().filter(|l| !l.trim().is_empty()) {
            match line.split_once(':').map(|(k, v)| (k.trim(), v.trim())) {
                Some(("title", value)) if !value.is_empty() => title = Some(value.to_string()),
                Some(("updated", value)) => updated = Some(value.to_string()),
                Some(("description", value)) => description = Some(value.to_string()),
                _ => {
                    return Err(format!(
                        "front matter: {line:?} is not title:, updated: or description:"
                    ));
                }
            }
        }
        let mut depth = 0i32;
        for (n, line) in body.lines().enumerate() {
            if let Some(fronts) = line.strip_prefix(":::") {
                if fronts.trim().is_empty() {
                    depth -= 1;
                } else {
                    for front in fronts.split_whitespace() {
                        if !DOC_FRONTS.contains(&front) && !SWITCHES.contains(&front) {
                            return Err(format!(
                                "line {}: ::: {front} is not one of {}, {}",
                                n + 1,
                                DOC_FRONTS.join(", "),
                                SWITCHES.join(", ")
                            ));
                        }
                    }
                    depth += 1;
                }
                if depth < 0 {
                    return Err(format!("line {}: ::: closes nothing", n + 1));
                }
            }
        }
        if depth != 0 {
            return Err("a ::: block is not closed".into());
        }
        Ok(Self {
            title: title.ok_or("front matter needs a title:")?,
            updated: updated.unwrap_or_default(),
            description: description.unwrap_or_default(),
            body: body.to_string(),
        })
    }

    /// The lines of the page this hub shows (`render`'s choice of `::: …` blocks).
    fn shown<'a>(&'a self, shown_here: &[&str]) -> Vec<&'a str> {
        let mut lines = Vec::new();
        let mut shown = vec![true];
        for line in self.body.lines() {
            if let Some(fronts) = line.strip_prefix(":::") {
                if fronts.trim().is_empty() {
                    if shown.len() > 1 {
                        shown.pop();
                    }
                } else {
                    let here = fronts.split_whitespace().any(|f| shown_here.contains(&f));
                    shown.push(here && *shown.last().unwrap_or(&true));
                }
                continue;
            }
            if *shown.last().unwrap_or(&true) {
                lines.push(line);
            }
        }
        lines
    }

    /// The page as Markdown on a hub that is each of `shown_here`, its placeholders filled
    /// (`/llms.txt`, `/llms-full.txt`): the same blocks `render` shows, blank lines run
    /// together.
    pub fn markdown(&self, ctx: &Ctx, shown_here: &[&str]) -> String {
        let mut out = String::new();
        let mut blank = true;
        for line in self.shown(shown_here) {
            let line = line.trim_end();
            if line.trim().is_empty() {
                if !blank {
                    out.push('\n');
                }
                blank = true;
                continue;
            }
            out.push_str(&ctx.plain(line));
            out.push('\n');
            blank = false;
        }
        out.trim_end().to_string() + "\n"
    }

    /// The page's HTML on a hub that is each of `shown`: its kind (one of [`DOC_FRONTS`])
    /// and, where it matters, one of [`SWITCHES`]. A block shows when it names one of them.
    pub fn render(&self, ctx: &Ctx, shown_here: &[&str]) -> String {
        // Only what this hub shows.
        let lines = self.shown(shown_here);
        let mut out = String::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim_end();
            if line.trim().is_empty() {
                i += 1;
                continue;
            }
            if let Some((level, heading)) = heading(line) {
                out.push_str(&format!("<h{level}>{}</h{level}>", ctx.inline(heading)));
                i += 1;
            } else if line.starts_with("- ") {
                i = list(&lines, i, ctx, &mut out, false);
            } else if numbered(line).is_some() {
                i = list(&lines, i, ctx, &mut out, true);
            } else if line.starts_with('|') && lines.get(i + 1).is_some_and(|l| separator(l)) {
                i = table(&lines, i, ctx, &mut out);
            } else {
                let mut text = String::new();
                while i < lines.len() {
                    let line = lines[i].trim();
                    if line.is_empty() || heading(line).is_some() {
                        break;
                    }
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(line);
                    i += 1;
                }
                out.push_str(&format!("<p>{}</p>", ctx.inline(&text)));
            }
        }
        out
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=3).contains(&level) && line.as_bytes().get(level) == Some(&b' ') {
        Some((level, line[level + 1..].trim()))
    } else {
        None
    }
}

/// The text after `12. `, if the line is a numbered item.
fn numbered(line: &str) -> Option<&str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && line[digits..].starts_with(". ")).then(|| &line[digits + 2..])
}

fn list(lines: &[&str], mut i: usize, ctx: &Ctx, out: &mut String, ordered: bool) -> usize {
    let mut items: Vec<String> = Vec::new();
    while i < lines.len() {
        let line = lines[i].trim_end();
        let item = if ordered {
            numbered(line)
        } else {
            line.strip_prefix("- ")
        };
        match item {
            Some(text) => items.push(text.trim().to_string()),
            None if line.starts_with("  ") && !line.trim().is_empty() && !items.is_empty() => {
                let last = items.last_mut().expect("an item");
                last.push(' ');
                last.push_str(line.trim());
            }
            None => break,
        }
        i += 1;
    }
    let tag = if ordered { "ol" } else { "ul" };
    out.push_str(&format!("<{tag}>"));
    for item in items {
        out.push_str(&format!("<li>{}</li>", ctx.inline(&item)));
    }
    out.push_str(&format!("</{tag}>"));
    i
}

fn separator(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('|')
        && line.contains('-')
        && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn cells(line: &str) -> Vec<&str> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    line.split('|').map(str::trim).collect()
}

fn table(lines: &[&str], mut i: usize, ctx: &Ctx, out: &mut String) -> usize {
    out.push_str("<table><thead><tr>");
    for cell in cells(lines[i]) {
        out.push_str(&format!("<th>{}</th>", ctx.inline(cell)));
    }
    out.push_str("</tr></thead><tbody>");
    i += 2;
    while i < lines.len() && lines[i].trim_start().starts_with('|') {
        out.push_str("<tr>");
        for cell in cells(lines[i]) {
            out.push_str(&format!("<td>{}</td>", ctx.inline(cell)));
        }
        out.push_str("</tr>");
        i += 1;
    }
    out.push_str("</tbody></table>");
    i
}
