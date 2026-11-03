//! Which requests are visits, and what of one is kept (`analytics.rs` says why each).

use std::sync::LazyLock;

use axum::http::{HeaderMap, Method, Uri, header};

/// The public pages: the only paths counted.
pub const PAGES: [&str; 5] = ["/", "/docs", "/privacy", "/terms", "/support"];
/// The device classes a visit is told apart by.
#[cfg_attr(not(test), allow(dead_code))]
pub const DEVICES: [&str; 3] = ["mobile", "tablet", "desktop"];
/// The query parameters kept, in this order; everything else in a query is dropped.
pub const UTM: [&str; 3] = ["utm_source", "utm_medium", "utm_campaign"];
/// The longest kept value of a `utm_*` parameter, in characters.
const UTM_MAX: usize = 100;

/// The User-Agent substrings of robots, lowercase (`bots.txt`, one per line).
static BOTS: LazyLock<Vec<String>> = LazyLock::new(|| {
    include_str!("bots.txt")
        .lines()
        .map(|line| line.trim().to_ascii_lowercase())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
});

/// One counted visit, as it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The UTC day, as Unix seconds at its midnight.
    pub day: i64,
    /// The UTC hour, 0 to 23.
    pub hour: i64,
    pub path: &'static str,
    /// `utm_source`, `utm_medium`, `utm_campaign`; empty when the link had none.
    pub utm: [String; 3],
    /// The referring site's host, or empty.
    pub referrer: String,
    /// One of [`DEVICES`].
    pub device: &'static str,
    /// The day's visitor id (`Analytics::visitor`).
    pub visitor: String,
}

/// Why a request is not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Not a `GET` of a public page.
    NotAPage,
    /// `Sec-GPC: 1` or `DNT: 1`.
    OptedOut,
    /// No User-Agent, or a robot's.
    Robot,
}

/// A request that is a visit, before its visitor is known: what is kept of it, and the
/// User-Agent its visitor id is made from (and nothing else).
#[derive(Debug)]
pub struct Seen<'a> {
    pub path: &'static str,
    pub utm: [String; 3],
    pub referrer: String,
    pub device: &'static str,
    pub agent: &'a str,
}

/// Whether this request is a visit to count, on a hub whose own host is `own_host`.
pub fn seen<'a>(
    method: &Method,
    uri: &Uri,
    headers: &'a HeaderMap,
    own_host: &str,
) -> Result<Seen<'a>, Skip> {
    let path = PAGES
        .iter()
        .find(|page| **page == uri.path())
        .ok_or(Skip::NotAPage)?;
    if method != Method::GET {
        return Err(Skip::NotAPage);
    }
    let says_one = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.trim() == "1")
    };
    if says_one("sec-gpc") || says_one("dnt") {
        return Err(Skip::OptedOut);
    }
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .ok_or(Skip::Robot)?;
    if robot(agent) {
        return Err(Skip::Robot);
    }
    let referrer = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(host_of)
        .filter(|host| host != own_host)
        .unwrap_or_default();
    Ok(Seen {
        path,
        utm: utm(uri.query().unwrap_or("")),
        referrer,
        device: device(agent),
        agent,
    })
}

/// Whether a User-Agent is a robot's: it names one of `bots.txt`'s.
pub fn robot(agent: &str) -> bool {
    let agent = agent.to_ascii_lowercase();
    BOTS.iter().any(|bot| agent.contains(bot.as_str()))
}

/// Phone, tablet or computer, from a User-Agent.
pub fn device(agent: &str) -> &'static str {
    let agent = agent.to_ascii_lowercase();
    let has = |word: &str| agent.contains(word);
    if has("ipad")
        || has("tablet")
        || has("kindle")
        || has("silk/")
        || has("playbook")
        || (has("android") && !has("mobile"))
    {
        "tablet"
    } else if has("mobi")
        || has("iphone")
        || has("ipod")
        || has("android")
        || has("windows phone")
        || has("blackberry")
        || has("opera mini")
    {
        "mobile"
    } else {
        "desktop"
    }
}

/// The `utm_*` values of a query, decoded, without control characters or `|` (the daily
/// totals join the three with it), at most [`UTM_MAX`] characters each; the rest of the
/// query is dropped.
pub fn utm(query: &str) -> [String; 3] {
    let mut out: [String; 3] = Default::default();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if let Some(slot) = UTM.iter().position(|name| *name == key) {
            let value = value.replace('+', " ");
            let decoded = percent_encoding::percent_decode_str(&value).decode_utf8_lossy();
            out[slot] = decoded
                .chars()
                .filter(|c| !c.is_control() && *c != '|')
                .collect::<String>()
                .trim()
                .chars()
                .take(UTM_MAX)
                .collect();
        }
    }
    out
}

/// The host of an `http(s)://` URL, lowercase, without its port or user: a name of
/// `[a-z0-9.-]` (or an IPv6 address in brackets), at most 253 characters.
pub fn host_of(url: &str) -> Option<String> {
    let url = url.trim();
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(v6) = authority.strip_prefix('[') {
        format!("[{}]", v6.split(']').next().unwrap_or(""))
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let fine = !host.is_empty()
        && host.len() <= 253
        && host.bytes().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'.' | b'-' | b'[' | b']' | b':')
        });
    fine.then_some(host)
}
