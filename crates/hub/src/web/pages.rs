//! The hub's few HTML pages: one stylesheet from the hub itself, every value escaped, and
//! script only where a page needs it (push on the approvals page, passkeys on sign-in),
//! always the hub's own file. A strict Content-Security-Policy backs that up. The name in
//! the bar and the titles, the mark, the colours and the fonts are the site's (`site.rs`),
//! set per request beside the bar (`web::bar`).

use std::sync::Arc;

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

/// The built-in site's stylesheet: the tokens, the pages' shell (`hub.css`) and the landing
/// page (`landing.css`). A hub serves its own site's (`Site::stylesheet`).
#[cfg(test)]
pub static CSS: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::site::builtin().stylesheet().to_string());

pub use crate::hub::Machine;
pub use crate::site::Front;
use crate::site::Site;

/// What the top bar of a page offers, set per request by `web::bar`.
#[derive(Clone)]
pub enum Bar {
    /// No web sign-in on this hub: the brand only.
    Plain,
    /// A live session: Approvals, Machines and a one-tap Sign out; on an account nobody
    /// secured yet, a line under it that says so and leads to securing it.
    SignedIn { unsecured: bool },
    /// No session: one Sign in that comes back to this page.
    SignedOut { return_to: String },
}

tokio::task_local! {
    pub static BAR: Bar;
    /// The site this request is drawn with: the hub's (`web::bar` sets it).
    pub static SITE: Arc<Site>;
}

/// The site the current request is drawn with; the built-in one outside a request.
pub fn site() -> Arc<Site> {
    SITE.try_with(Arc::clone)
        .unwrap_or_else(|_| crate::site::builtin())
}

fn bar_html(bar: &Bar, site: &Site) -> String {
    let links = match bar {
        Bar::Plain => String::new(),
        Bar::SignedIn { .. } => "<nav><a href=\"/approvals\">Approvals</a><a href=\"/account\">Machines</a>\
<form method=\"post\" action=\"/auth/logout\"><button type=\"submit\">Sign out</button></form></nav>"
            .to_string(),
        Bar::SignedOut { return_to } => format!(
            "<nav><a class=\"button primary\" href=\"/auth/login?return_to={}\">Sign in</a></nav>",
            escape(&super::encode(return_to))
        ),
    };
    let nudge = match bar {
        Bar::SignedIn { unsecured: true } => {
            "<p class=\"nudge\" role=\"status\">Your account is not \
secured yet: nothing can recover it if you lose this browser. <a href=\"/account#secure\">Secure it</a></p>"
        }
        _ => "",
    };
    // The site's mark, inline: the pages' CSP loads no images, and CSS colours it per theme.
    let mark = site
        .named(site.info.assets.logo.as_ref())
        .map(|logo| logo.text())
        .unwrap_or_default();
    format!(
        "<header class=\"bar\"><a class=\"brand\" href=\"/\">{mark}<span class=\"brand-name\">{}</span></a>{links}</header>{nudge}",
        escape(site.name())
    )
}

pub struct Viewer<'a> {
    pub subject: &'a str,
    pub name: Option<&'a str>,
    pub owner: bool,
    /// Signed in with the hub's own account (a passkey), not through Nomen.
    pub hub_account: bool,
    /// When an account nobody secured yet ends if unused: the page offers to secure it.
    pub unsecured_until: Option<u64>,
    /// The latest steps in the account's tenant, newest first.
    pub events: &'a [crate::accounts::tenants::Event],
    /// The other sign-ins the offer to secure the account lists, ready to link.
    pub ways: &'a str,
    /// The sign-ins linked to the account (Google, GitHub, email, an OpenID Connect provider…).
    pub linked: &'a [crate::accounts::identities::Linked],
    /// Its tenant's place on a hosted hub: waiting since when, or in since when.
    pub access: Option<&'a crate::accounts::access::Access>,
    /// The name of the plan its tenant is on, when a plan holds it.
    pub plan: Option<&'a str>,
    /// The hosted hub's operator: the account page leads to the operator page.
    pub operator: bool,
}

/// Where the account stands on a hosted hub: on the early-access list since when, or in
/// since when and on which plan (`content/messages.json` `access`).
fn access_note(viewer: &Viewer<'_>) -> String {
    let Some(access) = viewer.access else {
        return String::new();
    };
    let site = site();
    let copy = &site.messages.access;
    if access.waiting {
        let since = access.waiting_since.map(utc).unwrap_or_default();
        let text = crate::site::Ctx::new(&site)
            .with("since", since)
            .inline(&copy.account_waiting);
        return format!("<p class=\"note\" id=\"access\" role=\"status\">{text}</p>");
    }
    match (viewer.plan, access.admitted_at) {
        (Some(plan), Some(at)) => {
            let text = crate::site::Ctx::new(&site)
                .with("since", utc(at))
                .with("plan", plan)
                .inline(&copy.account_admitted);
            format!("<p class=\"note\" id=\"access\">{text}</p>")
        }
        _ => String::new(),
    }
}

pub fn html(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, Kind::Plain, "")
}

/// A public page (`/docs`, `/privacy`, …): a plain page with what search engines and link
/// previews read in its head (`site/discovery.rs`).
pub fn public(status: StatusCode, title: &str, body: &str, head: &str) -> Response {
    render(status, title, body, Kind::Plain, head)
}

/// A page that is also the installable app: the manifest, the icon and the hub's one
/// script (`/assets/push.js`) may load, from this origin only. It is drawn in the app's
/// frame (`app_page`) on its Approvals tab.
pub fn app(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, Kind::App { tab: Tab::Approvals, script: "/assets/push.js" }, "")
}

/// The phone app's tabs (`web/app/`), along the bottom of its frame.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Machines,
    Boards,
    Approvals,
}

/// A page of the phone app (`web/app/`): one fixed frame, the bar on top and the tabs at the
/// bottom, with the page scrolling inside it. Its one script is `/assets/app.js`.
pub fn app_page(status: StatusCode, title: &str, body: &str, tab: Tab) -> Response {
    render(status, title, body, Kind::App { tab, script: "/assets/app.js" }, "")
}

/// The frame's tab bar, the current tab marked; and, for `app.js` to show, the install
/// banner for iPhone or Android.
fn tabs(tab: Tab, script: &str, site: &Site) -> (String, String) {
    let words = &site.messages.app;
    let links: String = [
        (Tab::Home, "/app", &words.tab_home),
        (Tab::Machines, "/app/machines", &words.tab_machines),
        (Tab::Boards, "/app/boards", &words.tab_boards),
        (Tab::Approvals, "/approvals", &words.tab_approvals),
    ]
    .iter()
    .map(|(this, href, label)| {
        let current = if *this == tab { " aria-current=\"page\"" } else { "" };
        format!("<a href=\"{href}\"{current}>{}</a>", escape(label))
    })
    .collect();
    let banner = if script == "/assets/app.js" {
        format!(
            "<p class=\"install\" id=\"install\" hidden><span data-os=\"iphone\" hidden>{}</span>\
<span data-os=\"android\" hidden>{} <button id=\"install-go\" type=\"button\">{}</button></span></p>",
            escape(&words.install_iphone),
            escape(&words.install_android),
            escape(&words.install_button)
        )
    } else {
        String::new()
    };
    (format!("<nav class=\"tabs\">{links}</nav>"), banner)
}

/// A workflow's page (`workflow.rs`): its own script, which follows the steps, and the
/// passkey script while the account can be secured there, and the phone's (with the app's
/// manifest) while a phone can be turned on there. All from this origin.
pub fn workflow(
    status: StatusCode,
    title: &str,
    body: &str,
    passkey: bool,
    push: bool,
) -> Response {
    let mut response = render(status, title, body, Kind::Workflow { passkey, push }, "");
    // Its forms post here: the browser sends a POST's true Origin only under `same-origin`.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

/// A page that runs one of the hub's own scripts (`/assets/passkey.js`), loaded from this
/// origin and talking only to it.
pub fn scripted(status: StatusCode, title: &str, body: &str, script: &'static str) -> Response {
    render(status, title, body, Kind::Script(script), "")
}

#[derive(Clone, Copy)]
enum Kind {
    Plain,
    /// The landing page: a plain page (no script) laid out wide as full-viewport pages
    /// under the fixed bar (`body.landing`, `landing.css`), with the hub's own setup video.
    Landing,
    /// A page in the phone app's frame (`app_page`), on `tab`, with its one script.
    App { tab: Tab, script: &'static str },
    Script(&'static str),
    Workflow {
        passkey: bool,
        push: bool,
    },
}

/// A page: `meta` is what search engines and link previews read in its head, on the
/// public pages only.
fn render(status: StatusCode, title: &str, body: &str, kind: Kind, meta: &str) -> Response {
    let site = site();
    let head = match kind {
        Kind::Plain | Kind::Landing => String::new(),
        Kind::App { script, .. } => {
            let icon = site
                .named(site.info.assets.icon.as_ref())
                .map(|icon| format!("<link rel=\"icon\" href=\"{}\">", escape(&icon.url())))
                .unwrap_or_default();
            format!(
                "<link rel=\"manifest\" href=\"/manifest.webmanifest\">{icon}\
<meta name=\"theme-color\" content=\"{}\"><script src=\"{script}\" defer></script>",
                escape(site.theme_color())
            )
        }
        Kind::Script(script) => format!("<script src=\"{script}\" defer></script>"),
        Kind::Workflow { passkey, push } => {
            let mut head = String::from("<script src=\"/assets/workflow.js\" defer></script>");
            if passkey {
                head.push_str("<script src=\"/assets/passkey.js\" defer></script>");
            }
            if push {
                head.push_str(&format!(
                    "<link rel=\"manifest\" href=\"/manifest.webmanifest\">\
<meta name=\"theme-color\" content=\"{}\"><script src=\"/assets/push.js\" defer></script>",
                    escape(site.theme_color())
                ));
            }
            head
        }
    };
    let (tabs, banner) = match kind {
        Kind::App { tab, script } => tabs(tab, script, &site),
        _ => (String::new(), String::new()),
    };
    let page = format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{} · {}</title>{meta}<link rel=\"stylesheet\" href=\"/assets/hub.css\">{head}</head>\
<body{class}>{bar}<main>{banner}{body}</main>{tabs}</body></html>\n",
        escape(title),
        escape(site.name()),
        class = match kind {
            Kind::Landing => " class=\"landing\"",
            Kind::App { .. } => " class=\"app\"",
            _ => "",
        },
        bar = BAR
            .try_with(|bar| bar_html(bar, &site))
            .unwrap_or_else(|_| bar_html(&Bar::Plain, &site)),
    );
    let mut response = (status, page).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let policy = match kind {
        Kind::App { .. } | Kind::Workflow { .. } => {
            "default-src 'none'; style-src 'self'; script-src 'self'; connect-src 'self'; worker-src 'self'; \
manifest-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        }
        Kind::Script(_) => {
            "default-src 'none'; style-src 'self'; script-src 'self'; connect-src 'self'; \
form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        }
        Kind::Plain => {
            "default-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        }
        // The setup video and its poster, from this hub only.
        Kind::Landing => {
            "default-src 'none'; style-src 'self'; img-src 'self'; media-src 'self'; form-action 'self'; \
frame-ancestors 'none'; base-uri 'none'"
        }
    };
    // A site that serves its own fonts: from this hub only.
    let policy = if site.has_fonts() {
        format!("{policy}; font-src 'self'")
    } else {
        policy.to_string()
    };
    if let Ok(value) = HeaderValue::from_str(&policy) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
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

/// `/` for someone not signed in: the product page (`landing.rs`). A hub nobody owns offers
/// only the claim. The MCP address is this hub's own `public_url`, never a hosted one.
pub fn home(front: Front, public_url: &str) -> Response {
    let (title, body) = super::landing::page(&front, public_url);
    let site = site();
    let meta = crate::site::discovery::head(
        &site,
        public_url,
        &crate::site::discovery::Meta {
            path: "/",
            title: &title,
            description: "",
            // A hub nobody owns yet is not one to find.
            noindex: front == Front::Unclaimed,
            json_ld: crate::site::discovery::landing_json_ld(&site, front, public_url),
        },
    );
    render(StatusCode::OK, &title, &body, Kind::Landing, &meta)
}

pub fn account(viewer: &Viewer<'_>, machines: &[Machine]) -> Response {
    let shown = viewer.name.unwrap_or(viewer.subject);
    let owner = if viewer.owner {
        "<p class=\"note\">You own this hub: you reach every machine on it.</p>"
    } else {
        ""
    };
    let operator = if viewer.operator {
        let site = site();
        format!(
            "<p class=\"note\" id=\"operator\">{}</p>",
            crate::site::Ctx::new(&site).inline(&site.messages.operator.account_link)
        )
    } else {
        String::new()
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
    let secure = viewer
        .unsecured_until
        .map(|until| super::hosted::secure_offer(until, true, viewer.ways))
        .unwrap_or_default();
    // A secured hub account: the sign-ins that reach it, and the others it may add.
    let sign_ins = if viewer.hub_account && viewer.unsecured_until.is_none() {
        let rows: String = viewer
            .linked
            .iter()
            .map(|linked| {
                format!(
                    "<li>{}{}</li>",
                    escape(&linked.provider),
                    linked
                        .email
                        .as_deref()
                        .map(|email| format!(" ({})", escape(email)))
                        .unwrap_or_default()
                )
            })
            .collect();
        let rows = if rows.is_empty() {
            "<li>Passkeys only</li>".to_string()
        } else {
            rows
        };
        format!(
            "<h2>How you sign in</h2><ul class=\"linked\">{rows}</ul>{}",
            viewer.ways
        )
    } else {
        String::new()
    };
    let history = if viewer.events.is_empty() {
        String::new()
    } else {
        let rows: String = viewer
            .events
            .iter()
            .map(|event| {
                format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                    utc(event.at),
                    escape(&event.event),
                    escape(&event.detail)
                )
            })
            .collect();
        format!(
            "<h2>Recent activity in your space</h2><table class=\"activity\"><thead><tr><th>When</th>\
<th>What</th><th></th></tr></thead><tbody>{rows}</tbody></table>"
        )
    };
    let body = format!(
        "<h1>Signed in as {}</h1>\
<p class=\"meta\">{kind} ID <code>{}</code></p>{owner}{operator}{access}{secure}\
<h2>Your machines</h2>{machines}{add}{sign_ins}{history}\
<form method=\"post\" action=\"/auth/logout\"><button type=\"submit\">Sign out</button></form>",
        escape(shown),
        escape(viewer.subject),
        access = access_note(viewer),
        kind = if viewer.hub_account {
            "Account".to_string()
        } else {
            format!("{} account", escape(site().label("nomen", "Nomen")))
        },
    );
    // The offer to secure the account makes its passkey on this page: the hub's one
    // passkey script runs here then, and only then.
    if viewer.unsecured_until.is_some() {
        let mut response = scripted(StatusCode::OK, "Your account", &body, "/assets/passkey.js");
        // Its email form posts here: the browser sends that POST's true Origin only under
        // `same-origin`.
        response.headers_mut().insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        );
        response
    } else {
        html(StatusCode::OK, "Your account", &body)
    }
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
