//! The hub's few HTML pages: one stylesheet from the hub itself, every value escaped, and
//! script only where a page needs it (push on the approvals page, passkeys on sign-in),
//! always the hub's own file. A strict Content-Security-Policy backs that up.

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

pub const CSS: &str = include_str!("hub.css");

pub use crate::hub::Machine;

/// What the top bar of a page offers, set per request by `web::bar`.
#[derive(Clone)]
pub enum Bar {
    /// No web sign-in on this hub: the brand only.
    Plain,
    /// A live session: Approvals, Machines and a one-tap Sign out.
    SignedIn,
    /// No session: one Sign in that comes back to this page.
    SignedOut { return_to: String },
}

tokio::task_local! {
    pub static BAR: Bar;
}

fn bar_html(bar: &Bar) -> String {
    let links = match bar {
        Bar::Plain => String::new(),
        Bar::SignedIn => "<nav><a href=\"/approvals\">Approvals</a><a href=\"/account\">Machines</a>\
<form method=\"post\" action=\"/auth/logout\"><button type=\"submit\">Sign out</button></form></nav>"
            .to_string(),
        Bar::SignedOut { return_to } => format!(
            "<nav><a class=\"button primary\" href=\"/auth/login?return_to={}\">Sign in</a></nav>",
            escape(&super::encode(return_to))
        ),
    };
    format!(
        "<header class=\"bar\"><a class=\"brand\" href=\"/\">{MARK}Devagotchi</a>{links}</header>"
    )
}

/// The app icon's pet, inline: the pages' CSP loads no images, and CSS colors it per theme.
const MARK: &str = "<svg class=\"mark\" viewBox=\"0 0 32 32\" aria-hidden=\"true\" focusable=\"false\">\
<rect class=\"mark-bg\" width=\"32\" height=\"32\" rx=\"8\"/><ellipse class=\"mark-egg\" cx=\"16\" cy=\"17.3\" rx=\"9.4\" ry=\"10.6\"/>\
<circle class=\"mark-face\" cx=\"12.9\" cy=\"15.6\" r=\"1.4\"/><circle class=\"mark-face\" cx=\"19.1\" cy=\"15.6\" r=\"1.4\"/>\
<path class=\"mark-smile\" d=\"M13.5 20.1q2.5 1.9 5 0\"/></svg>";

pub struct Viewer<'a> {
    pub subject: &'a str,
    pub name: Option<&'a str>,
    pub owner: bool,
    /// Signed in with the hub's own account (a passkey), not through Shippin.
    pub hub_account: bool,
}

pub fn html(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, Kind::Plain)
}

/// A page that is also the installable app: the manifest, the icon and the hub's one
/// script (`/assets/push.js`) may load, from this origin only.
pub fn app(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, Kind::App)
}

/// A page that runs one of the hub's own scripts (`/assets/passkey.js`), loaded from this
/// origin and talking only to it.
pub fn scripted(status: StatusCode, title: &str, body: &str, script: &'static str) -> Response {
    render(status, title, body, Kind::Script(script))
}

#[derive(Clone, Copy)]
enum Kind {
    Plain,
    /// The landing page: a plain page (no script) laid out wide (`body.landing`).
    Landing,
    App,
    Script(&'static str),
}

fn render(status: StatusCode, title: &str, body: &str, kind: Kind) -> Response {
    let head = match kind {
        Kind::Plain | Kind::Landing => String::new(),
        Kind::App => "<link rel=\"manifest\" href=\"/manifest.webmanifest\"><link rel=\"icon\" href=\"/assets/icon.svg\">\
<meta name=\"theme-color\" content=\"#2f6b4f\"><script src=\"/assets/push.js\" defer></script>"
            .to_string(),
        Kind::Script(script) => format!("<script src=\"{script}\" defer></script>"),
    };
    let page = format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{} · Devagotchi</title><link rel=\"stylesheet\" href=\"/assets/hub.css\">{head}</head>\
<body{class}>{bar}<main>{body}</main></body></html>\n",
        escape(title),
        class = match kind {
            Kind::Landing => " class=\"landing\"",
            _ => "",
        },
        bar = BAR
            .try_with(bar_html)
            .unwrap_or_else(|_| bar_html(&Bar::Plain)),
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
        HeaderValue::from_static(match kind {
            Kind::App => {
                "default-src 'none'; style-src 'self'; script-src 'self'; connect-src 'self'; worker-src 'self'; \
manifest-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
            }
            Kind::Script(_) => {
                "default-src 'none'; style-src 'self'; script-src 'self'; connect-src 'self'; \
form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
            }
            Kind::Plain | Kind::Landing => {
                "default-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
            }
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

/// What `/` shows someone not signed in.
pub enum Front {
    /// A hub with its own accounts whose owner cannot sign in yet: claim it.
    Unclaimed,
    /// A hub with its own accounts: sign in with a passkey.
    Passkeys,
    /// A hub that signs people in with Shippin (Nomen).
    Shippin,
}

/// `/` for someone not signed in: the product page (`landing.rs`). A hub nobody owns offers
/// only the claim. The MCP address is this hub's own `public_url`, never a hosted one.
pub fn home(front: Front, public_url: &str) -> Response {
    let (title, body) = super::landing::page(&front, public_url);
    render(StatusCode::OK, title, &body, Kind::Landing)
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
            let revoke = if m.owned && m.status != "revoked" {
                format!(
                    "<form method=\"post\" action=\"/machines/revoke\"><input type=\"hidden\" name=\"name\" value=\"{}\">\
<button class=\"deny\" type=\"submit\">Revoke</button></form>",
                    escape(&m.name)
                )
            } else {
                String::new()
            };
            format!(
                "<tr id=\"{n}\"><td>{n}</td><td>{}</td><td><span class=\"status {}\">{}</span></td><td>{}</td><td>{revoke}</td></tr>",
                if m.kind == "local" {
                    "On the hub's host"
                } else if m.owned {
                    "Added by you"
                } else {
                    "Linked machine"
                },
                m.status,
                status_label(m.status),
                m.enrolled_at.map(utc).unwrap_or_else(|| "—".into()),
                n = escape(&m.name),
            )
        })
        .collect();
    let machines = if machines.is_empty() {
        "<p class=\"empty\">No machines yet. Add one below; a machine the hub operator links for you shows up here too.</p>".to_string()
    } else {
        format!(
            "<table><thead><tr><th>Machine</th><th>Where</th><th>Status</th><th>Enrolled</th><th></th></tr></thead><tbody>{rows}</tbody></table>"
        )
    };
    let add = "<h2>Add a machine</h2>\
<form method=\"post\" action=\"/machines/add\"><label>Name it (lowercase letters, digits, dashes)\
<input type=\"text\" name=\"name\" required maxlength=\"63\" pattern=\"[a-z0-9][a-z0-9\\-]{0,62}\" \
autocapitalize=\"none\" autocomplete=\"off\" spellcheck=\"false\" placeholder=\"platinum\"></label>\
<p class=\"actions\"><button class=\"approve\" type=\"submit\">Get a one-time code</button></p></form>";
    let body = format!(
        "<h1>Signed in as {}</h1>\
<p class=\"meta\">{kind} ID <code>{}</code></p>{owner}\
<h2>Your machines</h2>{machines}{add}\
<form method=\"post\" action=\"/auth/logout\"><button type=\"submit\">Sign out</button></form>",
        escape(shown),
        escape(viewer.subject),
        kind = if viewer.hub_account {
            "Account"
        } else {
            "Shippin account"
        },
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
