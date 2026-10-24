//! The hub's few HTML pages: no script, one stylesheet from the hub itself, every value
//! escaped. A strict Content-Security-Policy backs that up.

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

pub const CSS: &str = include_str!("hub.css");

pub use crate::hub::Machine;

pub struct Viewer<'a> {
    pub subject: &'a str,
    pub name: Option<&'a str>,
    pub owner: bool,
}

pub fn html(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, false)
}

/// A page that is also the installable app: the manifest, the icon and the hub's one
/// script (`/assets/push.js`) may load, from this origin only.
pub fn app(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, true)
}

fn render(status: StatusCode, title: &str, body: &str, app: bool) -> Response {
    let head = if app {
        "<link rel=\"manifest\" href=\"/manifest.webmanifest\"><link rel=\"icon\" href=\"/assets/icon.svg\">\
<meta name=\"theme-color\" content=\"#2f6b4f\"><script src=\"/assets/push.js\" defer></script>"
    } else {
        ""
    };
    let page = format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{} · Devagotchi</title><link rel=\"stylesheet\" href=\"/assets/hub.css\">{head}</head>\
<body><main><p class=\"brand\">Devagotchi</p>{body}</main></body></html>\n",
        escape(title)
    );
    let mut response = (status, page).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(if app {
            "default-src 'none'; style-src 'self'; script-src 'self'; connect-src 'self'; worker-src 'self'; \
manifest-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        } else {
            "default-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        }),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// A short page that says what happened and what to do next.
pub fn message(
    status: StatusCode,
    title: &str,
    text: &str,
    action: Option<(&str, &str)>,
) -> Response {
    let action = action
        .map(|(href, label)| {
            format!(
                "<p><a class=\"button\" href=\"{}\">{}</a></p>",
                escape(href),
                escape(label)
            )
        })
        .unwrap_or_default();
    html(
        status,
        title,
        &format!("<h1>{}</h1><p>{}</p>{action}", escape(title), escape(text)),
    )
}

pub fn account(viewer: &Viewer<'_>, machines: &[Machine]) -> Response {
    let shown = viewer.name.unwrap_or(viewer.subject);
    let owner = if viewer.owner {
        "<p class=\"note\">You own this hub: you reach every machine on it.</p>"
    } else {
        ""
    };
    let rows: String = machines
        .iter()
        .map(|m| {
            format!(
                "<tr><td>{}</td><td>{}</td><td><span class=\"status {}\">{}</span></td><td>{}</td></tr>",
                escape(&m.name),
                if m.kind == "local" { "On the hub's host" } else { "Linked machine" },
                m.status,
                status_label(m.status),
                m.enrolled_at.map(utc).unwrap_or_else(|| "—".into()),
            )
        })
        .collect();
    let machines = if machines.is_empty() {
        "<p class=\"empty\">No machines yet. A machine shows up here once it is enrolled and you are granted it.</p>".to_string()
    } else {
        format!(
            "<table><thead><tr><th>Machine</th><th>Where</th><th>Status</th><th>Enrolled</th></tr></thead><tbody>{rows}</tbody></table>"
        )
    };
    let body = format!(
        "<h1>Signed in as {}</h1>\
<p class=\"meta\">Shippin account ID <code>{}</code></p>{owner}\
<h2>Your machines</h2>{machines}\
<form method=\"post\" action=\"/auth/logout\"><button type=\"submit\">Sign out</button></form>",
        escape(shown),
        escape(viewer.subject),
    );
    html(StatusCode::OK, "Your account", &body)
}

fn status_label(status: &str) -> &'static str {
    match status {
        "online" => "Online",
        "offline" => "Offline",
        "revoked" => "Revoked",
        _ => "Not enrolled",
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Unix seconds as `YYYY-MM-DD HH:MM UTC` (civil-from-days, H. Hinnant).
pub fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}
