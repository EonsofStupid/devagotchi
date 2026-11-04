//! `/go/<token>`: the page a workflow's link opens (increment 68, `crate::workflows`). The AI
//! hands its person this one link; the page shows each step, does what the step needs with a
//! click, and ticks it the moment the hub sees it done. Nobody signs in.
//!
//! - **The link is the credential**, as the line `machine_add` hands an AI in chat already
//!   is: random, kept only as its SHA-256, bound to one tenant and one workflow, and short-
//!   lived (`workflows.link_ttl_secs`, 30 minutes by default). Once the workflow is done or
//!   cancelled, or the link expired, it grants nothing more.
//! - **Opening it spends nothing.** A GET (a person, a chat app's preview, a mail scanner)
//!   shows the workflow and its steps, and never a code; nothing is written.
//! - **Get my command is a POST from this page.** It issues the step's single-use join code,
//!   as Add a machine does with its checks (early access, the plan's limits), and the page
//!   shows that line with a copy button, for the OS picked. The code comes back in this
//!   browser's own cookie (HttpOnly, for its 15 minutes), so the page shows the line again on
//!   a reload; it is never in a URL.
//! - **A phone turns on from the same link** (`POST /go/<token>/phone`), opened on the phone;
//!   the page draws the link as a QR code for its camera (inline SVG, `qrcode`).
//! - **An AI client connects with its own steps** (`ai_client`): the hub's MCP address or the
//!   command to paste, with Copy buttons. For an account nobody secured, one press joins the
//!   client's sign-in page in this browser to the account (`POST /go/<token>/client`), so its
//!   Connect lands there and not on a new account.
//! - **Keeping the account is offered after the first success, and optional.** Only to a
//!   browser that did a step here (its own random cookie, remembered by its SHA-256), once a
//!   step was seen done, while nobody has secured the account: Google or GitHub when the hub
//!   has them, a passkey otherwise (`POST /go/<token>/keep`).
//! - **The page follows the hub.** `/assets/workflow.js` asks `/go/<token>/status` every few
//!   seconds and moves the ticks, facts and states in place: it never reloads, so nobody
//!   loses the line they are copying. Without JavaScript, Check again does the same; never a
//!   `<meta refresh>`. It works at phone width.
//! - Forms post only from this hub's own page (`same_origin`). Every word is the site's
//!   (`content/workflows.json`, `content/messages.json` `workflow`).

use axum::{
    Form, Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::{
    approvals::same_origin,
    cookie_value,
    pages::{self, escape},
    routes::{see_other, unavailable},
};
use crate::{
    AppState,
    accounts::{
        sessions::SESSION_MAX_SECS,
        workflows::{WorkflowRow, browser_hash, new_browser_secret},
    },
    auth::Subject,
    devices::{CODE_TTL_SECS, now_secs},
    site::{
        Ctx, Site,
        workflows::{AI_CLIENT, MACHINE, PHONE, SECURE_ACCOUNT},
    },
    workflows::WorkflowStatus,
};

fn secure(state: &AppState) -> bool {
    state.hub.config.public_url.starts_with("https://")
}

/// The line this browser was handed: `<workflow id>.<step>.<code>`.
fn code_cookie(state: &AppState) -> &'static str {
    if secure(state) { "__Host-dg_wfc" } else { "dg_wfc" }
}

/// This browser's own secret: what marks it as one that did a step through a link.
fn browser_cookie(state: &AppState) -> &'static str {
    if secure(state) { "__Host-dg_wfb" } else { "dg_wfb" }
}

/// A `Set-Cookie` value as the hub's sign-in sets them; `max_age` 0 deletes it.
fn cookie(state: &AppState, name: &str, value: &str, max_age: u64) -> String {
    let flag = if secure(state) { "; Secure" } else { "" };
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{flag}")
}

fn link_path(token: &str) -> String {
    format!("/go/{token}")
}

/// This hub's MCP address, what an AI client is given.
fn mcp_url(state: &AppState) -> String {
    format!("{}/mcp", state.hub.config.public_url.trim_end_matches('/'))
}

/// The name an AI client adds this hub under: the product's, as a name a command line takes.
fn server_name(site: &Site) -> String {
    let name: String = site
        .name()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name = name.trim_matches('-').to_string();
    if name.is_empty() { "hub".into() } else { name }
}

fn ttl(state: &AppState) -> u64 {
    state
        .hub
        .workflows
        .as_ref()
        .map(|workflows| workflows.link_ttl())
        .unwrap_or(0)
}

/// What a link may still do: everything while open, only show that it is done for a while
/// once completed, nothing once cancelled or expired.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    Live,
    Finished,
}

fn standing(row: &WorkflowRow, ttl: u64, now: u64) -> Option<Standing> {
    match row.state.as_str() {
        "open" if now < row.expires_at => Some(Standing::Live),
        "completed" if now < row.expires_at.saturating_add(ttl) => Some(Standing::Finished),
        _ => None,
    }
}

/// The workflow a link names and its account, while the link shows anything; else the
/// answer for a link that does not work.
async fn found(state: &AppState, token: &str, now: u64) -> Result<(WorkflowRow, Subject, Standing), Response> {
    let Some(workflows) = &state.hub.workflows else {
        return Err(dead());
    };
    let row = match workflows.accounts().workflow_by_link(token).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(dead()),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return Err(unavailable());
        }
    };
    let Some(standing) = standing(&row, workflows.link_ttl(), now) else {
        return Err(dead());
    };
    match state.hub.workflow_subject(&row).await {
        Ok(Some(subject)) => Ok((row, subject, standing)),
        Ok(None) => Err(dead()),
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            Err(unavailable())
        }
    }
}

/// A link that does not work, for whatever reason: the same answer for all of them.
fn dead() -> Response {
    let site = pages::site();
    let copy = &site.messages.workflow;
    pages::workflow(
        StatusCode::NOT_FOUND,
        &copy.dead_title,
        &format!(
            "<h1>{}</h1><p>{}</p>",
            escape(&copy.dead_title),
            Ctx::new(&site).inline(&copy.dead_text)
        ),
        false,
        false,
    )
}

/// Whether this browser did a step of `row` through its link, within its lifetime again.
fn did_a_step(state: &AppState, headers: &HeaderMap, row: &WorkflowRow, now: u64) -> bool {
    cookie_value(headers, browser_cookie(state))
        .and_then(|secret| browser_hash(&secret))
        .is_some_and(|hash| row.progress.did_a_step(&hash, ttl(state), now))
}

/// Whether any step is done, as the hub sees it now: the first success.
fn succeeded(status: &WorkflowStatus) -> bool {
    status.steps.iter().any(|step| step.state == "done")
}

/// This browser's secret, the one it has or a new one, and the cookie that keeps it.
fn browser_secret(state: &AppState, headers: &HeaderMap) -> (String, String) {
    let secret = cookie_value(headers, browser_cookie(state))
        .filter(|secret| browser_hash(secret).is_some())
        .unwrap_or_else(|| new_browser_secret(crate::link::random::<32>()));
    let set = cookie(state, browser_cookie(state), &secret, ttl(state).saturating_mul(2));
    (secret, set)
}

/// `GET /go/<token>`: the workflow and its steps. Never a code but this browser's own.
pub async fn page(State(state): State<AppState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let now = now_secs();
    let (row, subject, standing) = match found(&state, &token, now).await {
        Ok(found) => found,
        Err(page) => return page,
    };
    let status = match state.hub.look_at_workflow(&subject, row.clone(), None, now).await {
        Ok(status) => status,
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return unavailable();
        }
    };
    let site = pages::site();
    let title = Ctx::new(&site).plain(&row.definition.title);
    let shown = cookie_value(&headers, code_cookie(&state));
    let view = View {
        state: &state,
        headers: &headers,
        site: &site,
        subject: &subject,
        row: &row,
        status: &status,
        path: link_path(&token),
        // Seeing it done just now completes it: it then only shows that it is.
        live: standing == Standing::Live && status.state == "open",
        now,
        session_account: match state.passkeys.as_ref() {
            Some(hub) => hub.who(&headers).await.ok().flatten().map(|subject| subject.id),
            None => None,
        },
    };
    let (body, passkey, push) = view.body(shown.as_deref());
    // A machine's workflow opened on a phone: the link is for a computer, so that comes
    // first, to copy or scan.
    let machine = row.definition.steps.iter().any(|step| step.kind == MACHINE);
    let phone = machine && headers
        .get(header::USER_AGENT)
        .and_then(|agent| agent.to_str().ok())
        .is_some_and(|agent| ["Mobi", "Android", "iPhone"].iter().any(|word| agent.contains(word)));
    let body = if phone { format!("{}{body}", view.on_a_phone()) } else { body };
    pages::workflow(StatusCode::OK, &title, &body, passkey, push)
}

/// "Step n of total": the current step, or the last once all are done.
pub(super) fn progress_text(site: &Site, status: &WorkflowStatus) -> String {
    let total = status.steps.len();
    let n = status
        .steps
        .iter()
        .position(|step| step.state == "current")
        .map(|at| at + 1)
        .unwrap_or(total);
    Ctx::new(site)
        .with("n", n.to_string())
        .with("total", total.to_string())
        .plain(&site.messages.workflow.step_of)
}

fn state_label<'a>(site: &'a Site, state: &str, optional: bool) -> &'a str {
    let copy = &site.messages.workflow;
    match state {
        "done" => &copy.state_done,
        "current" => &copy.state_current,
        "unavailable" => &copy.state_unavailable,
        _ if optional => &copy.state_optional,
        _ => &copy.state_pending,
    }
}

/// One look at a workflow's page.
struct View<'a> {
    state: &'a AppState,
    headers: &'a HeaderMap,
    site: &'a Site,
    subject: &'a Subject,
    row: &'a WorkflowRow,
    status: &'a WorkflowStatus,
    path: String,
    /// The link still does things (open, not expired); else it only shows it is done.
    live: bool,
    now: u64,
    /// The account this browser's session is of, if it has one.
    session_account: Option<String>,
}

impl View<'_> {
    /// The steps, each with what it needs while the link is live, and keeping the account
    /// where this browser may. Returns the body and whether the passkey and phone scripts
    /// run here.
    fn body(&self, shown: Option<&str>) -> (String, bool, bool) {
        let (site, copy, ctx) = (self.site, &self.site.messages.workflow, Ctx::new(self.site));
        let (mut passkey, mut push) = (false, false);
        let keep = self.keep_section(&mut passkey);
        let has_keep_step = self.row.definition.steps.iter().any(|step| step.kind == SECURE_ACCOUNT);
        let mut items = String::new();
        for (index, (step, seen)) in self.row.definition.steps.iter().zip(&self.status.steps).enumerate() {
            let mut body = format!("<p>{}</p>", self.words().inline(&step.text));
            if seen.state != "done" && seen.why.is_none() {
                match step.kind.as_str() {
                    MACHINE if self.live => body.push_str(&self.machine_step(index, seen, shown)),
                    PHONE if self.live => {
                        if let Some(key) = self.state.hub.push.as_ref().map(|p| p.public_key.clone()) {
                            push = true;
                            body.push_str(&format!(
                                "<p><button class=\"button primary\" id=\"notify\" type=\"button\" data-key=\"{}\" \
data-subscribe=\"{}/phone\" data-auto=\"off\">{}</button></p>\
<p class=\"meta\" id=\"notify-status\" role=\"status\"></p><p class=\"meta\">{}</p>{}",
                                escape(&key),
                                escape(&self.path),
                                escape(&ctx.plain(&copy.notify)),
                                ctx.inline(&copy.phone_hint),
                                self.qr(),
                            ));
                        }
                    }
                    AI_CLIENT if self.live => body.push_str(&self.client_step(index, step.client.as_deref())),
                    SECURE_ACCOUNT => body.push_str(&keep),
                    _ => {}
                }
            }
            let why = seen
                .why
                .as_deref()
                .map(|why| format!("<p class=\"why\">{}</p>", escape(why)))
                .unwrap_or_default();
            items.push_str(&format!(
                "<li class=\"step\" id=\"step-{n}\" data-kind=\"{kind}\" data-state=\"{state}\"{optional}>\
<h2><span class=\"step-mark\" aria-hidden=\"true\"></span><span class=\"step-title\">{title}</span>\
<span class=\"step-state\">{label}</span></h2>\
<p class=\"fact\" role=\"status\">{fact}</p>{why}<div class=\"step-body\">{body}</div></li>",
                n = index + 1,
                kind = escape(&step.kind),
                state = escape(&seen.state),
                optional = if seen.optional { " data-optional=\"true\"" } else { "" },
                label = escape(&ctx.plain(state_label(site, &seen.state, seen.optional))),
                title = escape(&seen.title),
                fact = escape(seen.fact.as_deref().unwrap_or_default()),
            ));
        }
        let finished = format!(
            "<p class=\"note finished\" id=\"finished\" role=\"status\"{}>{}</p>",
            if self.status.state == "completed" { "" } else { " hidden" },
            ctx.inline(&copy.finished)
        );
        let access = self
            .subject
            .access
            .as_ref()
            .filter(|access| access.waiting)
            .map(|access| {
                format!(
                    "<p class=\"note\" id=\"access\" role=\"status\">{}</p>",
                    Ctx::new(site)
                        .with("since", access.waiting_since.map(pages::utc).unwrap_or_default())
                        .inline(&site.messages.access.account_waiting)
                )
            })
            .unwrap_or_default();
        let after = if has_keep_step { String::new() } else { keep };
        let check = if self.live {
            format!(
                "<form method=\"get\" action=\"{}\" class=\"check\" id=\"check\"><button type=\"submit\">{}</button></form>",
                escape(&self.path),
                escape(&ctx.plain(&copy.check_again))
            )
        } else {
            String::new()
        };
        let body = format!(
            "<h1>{title}</h1><p class=\"meta\">{intro}</p>{access}\
<p class=\"meta\" id=\"progress\" aria-live=\"polite\">{progress}</p>{finished}\
<ol class=\"steps\" id=\"steps\" data-status=\"{path}/status\" data-live=\"{live}\" data-done=\"{done}\" \
data-current=\"{current}\" data-pending=\"{pending}\" data-unavailable=\"{unavailable}\" \
data-optional=\"{optional}\" data-copy=\"{copy_label}\" data-copied=\"{copied}\">{items}</ol>{after}{check}",
            title = escape(&self.status.title),
            intro = ctx.inline(&self.row.definition.intro),
            progress = escape(&progress_text(site, self.status)),
            path = escape(&self.path),
            live = self.live,
            done = escape(&ctx.plain(&copy.state_done)),
            current = escape(&ctx.plain(&copy.state_current)),
            pending = escape(&ctx.plain(&copy.state_pending)),
            unavailable = escape(&ctx.plain(&copy.state_unavailable)),
            optional = escape(&ctx.plain(&copy.state_optional)),
            copy_label = escape(&ctx.plain(&copy.copy)),
            copied = escape(&ctx.plain(&copy.copied)),
        );
        (body, passkey, push)
    }

    /// Keeping the account: for a browser that did a step here while nobody has secured
    /// the account, shown once a step is done (the page's script shows it the moment one
    /// is). Google or GitHub when the hub has them, a passkey otherwise.
    fn keep_section(&self, passkey: &mut bool) -> String {
        let (copy, ctx) = (&self.site.messages.workflow, Ctx::new(self.site));
        let Some(hub) = self.state.passkeys.as_ref() else {
            return String::new();
        };
        if self.subject.provisional_until.is_none()
            || !did_a_step(self.state, self.headers, self.row, self.now)
        {
            return String::new();
        }
        let action = format!("{}/keep", escape(&self.path));
        let names = hub.provider_names();
        let ways = if names.is_empty() {
            *passkey = true;
            format!(
                "<p><button class=\"button primary\" id=\"passkey-add\" type=\"button\" data-prepare=\"{action}\" \
data-return-to=\"{}\" disabled>{}</button></p><p class=\"meta\" id=\"passkey-status\" role=\"status\"></p>",
                escape(&self.path),
                escape(&ctx.plain(&copy.make_passkey)),
            )
        } else {
            names
                .iter()
                .map(|(name, label)| {
                    format!(
                        "<form method=\"post\" action=\"{action}\" class=\"inline keep-with\">\
<input type=\"hidden\" name=\"how\" value=\"{}\"><button class=\"button\" type=\"submit\">{}</button></form>",
                        escape(name),
                        escape(label)
                    )
                })
                .collect()
        };
        format!(
            "<section class=\"keep\" id=\"keep\" aria-labelledby=\"keep-title\"{}><h2 id=\"keep-title\">{}</h2>\
<p>{}</p>{ways}</section>",
            if succeeded(self.status) { "" } else { " hidden" },
            escape(&ctx.plain(&copy.keep_title)),
            ctx.inline(&copy.keep_text),
        )
    }

    /// Copy that may name this hub's MCP address (`{mcp}`) and the name an AI client adds it
    /// under (`{server}`).
    fn words(&self) -> Ctx<'_> {
        Ctx::new(self.site).with("mcp", mcp_url(self.state)).with("server", server_name(self.site))
    }

    /// First on the page on a phone: open it on the computer, with the link to copy and its
    /// QR code to scan.
    fn on_a_phone(&self) -> String {
        let copy = &self.site.messages.workflow;
        let ctx = Ctx::new(self.site);
        let link = format!("{}{}", self.state.hub.config.public_url.trim_end_matches('/'), self.path);
        format!(
            "<section class=\"note\" id=\"open-on-computer\"><p><strong>{}</strong></p>\
<div class=\"command\"><pre><code id=\"page-link\">{}</code></pre>\
<button type=\"button\" class=\"copy\" data-copy=\"page-link\" hidden>{}</button></div>{}</section>",
            escape(&ctx.plain(&copy.open_on_computer)),
            escape(&link),
            escape(&ctx.plain(&copy.copy)),
            self.qr(),
        )
    }

    /// The link as a QR code, for a phone's camera: drawn here as inline SVG, nothing fetched.
    fn qr(&self) -> String {
        let link = format!("{}{}", self.state.hub.config.public_url.trim_end_matches('/'), self.path);
        let Ok(code) = qrcode::QrCode::new(link.as_bytes()) else {
            return String::new();
        };
        let svg = code
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(176, 176)
            .dark_color(qrcode::render::svg::Color("#000000"))
            .light_color(qrcode::render::svg::Color("#ffffff"))
            .quiet_zone(true)
            .build();
        // The SVG alone, without its XML declaration, labelled for a screen reader.
        let svg = svg.split_once("<svg").map(|(_, rest)| rest).unwrap_or_default();
        let label = escape(&Ctx::new(self.site).plain(&self.site.messages.workflow.qr));
        format!("<figure class=\"qr\" id=\"qr\"><svg role=\"img\" aria-label=\"{label}\"{svg}</figure>")
    }

    /// An `ai_client` step that is not done: the client's own steps, the address or command to
    /// paste with Copy buttons, and, for an account nobody secured, the button that joins
    /// the client's sign-in page in this browser to this account.
    fn client_step(&self, index: usize, client: Option<&str>) -> String {
        let (copy, ctx) = (&self.site.messages.workflow, self.words());
        let Some(words) = client.and_then(|client| copy.clients.get(client)) else {
            return String::new();
        };
        let name = ctx.plain(&words.name);
        let named = || self.words().with("client", name.clone());
        let mut out = format!("<p>{}</p>", ctx.inline(&words.how));
        for (at, line) in words.boxes.iter().enumerate() {
            out.push_str(&format!(
                "<p class=\"meta\">{}</p><div class=\"command\"><pre><code id=\"client-{n}-{at}\">{}</code></pre>\
<button type=\"button\" class=\"copy\" data-copy=\"client-{n}-{at}\" hidden>{}</button></div>",
                escape(&ctx.plain(&line.label)),
                escape(&ctx.plain(&line.text)),
                escape(&ctx.plain(&copy.copy)),
                n = index + 1,
            ));
        }
        if self.subject.provisional_until.is_none() {
            out.push_str(&format!("<p class=\"meta\">{}</p>", named().inline(&copy.client_secured)));
        } else if self.session_account.as_deref() == Some(self.subject.id.as_str()) {
            out.push_str(&format!(
                "<p class=\"note\" id=\"client-ready-{}\">{}</p>",
                index + 1,
                named().inline(&copy.client_ready)
            ));
        } else {
            out.push_str(&format!(
                "<p class=\"meta\">{}</p><form method=\"post\" action=\"{}/client\" class=\"inline\">\
<input type=\"hidden\" name=\"step\" value=\"{index}\"><button class=\"button primary\" type=\"submit\" id=\"client-here-{}\">{}</button></form>",
                named().inline(&copy.client_browser),
                escape(&self.path),
                index + 1,
                escape(&named().plain(&copy.client_here)),
            ));
        }
        out
    }

    /// A machine step that is not done: the line, while this browser holds a live code for
    /// the step's machine; else the form that gets it (the OS to pick, unless the step
    /// names one, and the name).
    fn machine_step(&self, index: usize, seen: &crate::workflows::StepStatus, shown: Option<&str>) -> String {
        let (copy, ctx) = (&self.site.messages.workflow, Ctx::new(self.site));
        let progress = self.row.progress.step(index);
        let stored = progress.and_then(|p| p.machine.as_deref());
        let live = shown.and_then(|value| {
            let mut parts = value.splitn(3, '.');
            let (id, at, code) = (parts.next()?, parts.next()?, parts.next()?);
            (id == self.row.id && at == index.to_string() && crate::join::valid_code(code)).then_some(code)
        });
        if let (Some(code), Some(stored)) = (live, stored)
            && matches!(self.state.hub.links.live_code(code), Ok(Some(device)) if device == stored)
        {
            let os = progress.and_then(|p| p.os.as_deref()).unwrap_or("linux");
            let line = crate::hub::join_line(&self.state.hub.config.public_url, os, code);
            return format!(
                "<p class=\"meta\">{}</p><div class=\"command\"><pre><code id=\"command-{n}\">{}</code></pre>\
<button type=\"button\" class=\"copy\" data-copy=\"command-{n}\" hidden>{}</button></div>\
<p class=\"meta\">{}</p>{}",
                copy.os_hint.get(os).map(|hint| ctx.inline(hint)).unwrap_or_default(),
                escape(&line),
                escape(&ctx.plain(&copy.copy)),
                ctx.inline(&copy.command_note),
                self.command_form(index, seen, true),
                n = index + 1,
            );
        }
        // A machine that joined and is not online yet needs no new line.
        if matches!(
            self.state.hub.machine_seen(&self.subject.id, progress),
            crate::workflows::MachineSeen::Offline
        ) {
            return String::new();
        }
        self.command_form(index, seen, false)
    }

    fn command_form(&self, index: usize, seen: &crate::workflows::StepStatus, again: bool) -> String {
        let (copy, ctx) = (&self.site.messages.workflow, Ctx::new(self.site));
        let fixed = self.row.definition.steps[index].os.clone();
        let picked = seen.os.clone();
        let choice = match &fixed {
            Some(os) => format!("<input type=\"hidden\" name=\"os\" value=\"{}\">", escape(os)),
            None => {
                let offered = self.state.hub.machine_oses();
                let first = picked
                    .as_deref()
                    .filter(|os| offered.contains(os))
                    .or(offered.first().copied());
                let options: String = offered
                    .iter()
                    .map(|os| {
                        format!(
                            "<label class=\"choice\"><input type=\"radio\" name=\"os\" value=\"{os}\"{}> {}</label>",
                            if Some(*os) == first { " checked" } else { "" },
                            escape(&ctx.plain(copy.os.get(*os).map(String::as_str).unwrap_or(os)))
                        )
                    })
                    .collect();
                format!(
                    "<fieldset class=\"os\"><legend>{}</legend>{options}<p class=\"meta mac\">{}</p></fieldset>",
                    escape(&ctx.plain(&copy.os_label)),
                    ctx.inline(&copy.mac)
                )
            }
        };
        let os = fixed.or(picked).unwrap_or_else(|| "linux".into());
        let name = self.state.hub.machine_name_for(self.subject, self.row, index, &os);
        format!(
            "<form method=\"post\" action=\"{path}/command\" class=\"get-command\">\
<input type=\"hidden\" name=\"step\" value=\"{index}\">{choice}\
<label class=\"name\">{}<input type=\"text\" name=\"name\" value=\"{}\" required maxlength=\"63\" \
pattern=\"[a-z0-9][a-z0-9\\-]{{0,62}}\" autocapitalize=\"none\" autocomplete=\"off\" spellcheck=\"false\"></label>\
<p class=\"actions\"><button class=\"{}\" type=\"submit\" id=\"get-command-{}\">{}</button></p></form>",
            escape(&ctx.plain(&copy.name_label)),
            escape(&name),
            if again { "button" } else { "button primary" },
            index + 1,
            escape(&ctx.plain(&copy.get_command)),
            path = escape(&self.path),
        )
    }
}

fn json(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /go/<token>/status`: what the page's script moves. The same view as the page: the
/// steps' states and facts, and whether this browser may keep the account now.
pub async fn status(State(state): State<AppState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let now = now_secs();
    let (row, subject, _) = match found(&state, &token, now).await {
        Ok(found) => found,
        Err(_) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "state": "gone" })),
    };
    match state.hub.look_at_workflow(&subject, row.clone(), None, now).await {
        Ok(status) => {
            let site = pages::site();
            let keep = subject.provisional_until.is_some()
                && succeeded(&status)
                && did_a_step(&state, &headers, &row, now);
            json(
                StatusCode::OK,
                serde_json::json!({
                    "state": status.state,
                    "progress": progress_text(&site, &status),
                    "keep": keep,
                    "steps": status.steps.iter().map(|step| serde_json::json!({
                        "state": step.state,
                        "fact": step.fact,
                    })).collect::<Vec<_>>(),
                }),
            )
        }
        Err(_) => json(StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "state": "unavailable" })),
    }
}

#[derive(Deserialize)]
pub struct CommandForm {
    step: usize,
    #[serde(default)]
    os: Option<String>,
    name: String,
}

/// A refusal on the workflow's page: what happened, plainly, and the way back to it.
fn refusal(site: &Site, status: StatusCode, title: &str, text: &str, path: &str) -> Response {
    pages::message(
        status,
        title,
        text,
        Some((path, &Ctx::new(site).plain(&site.messages.workflow.check_again))),
    )
}

/// A post from another site: refused, nothing changed.
fn cross_site(token: &str) -> Response {
    let site = pages::site();
    let copy = &site.messages.workflow;
    refusal(
        &site,
        StatusCode::FORBIDDEN,
        &copy.dead_title,
        &Ctx::new(&site).plain(&copy.refused),
        &link_path(token),
    )
}

/// `POST /go/<token>/command`: Get my command. Issues the step's join code for the
/// workflow's account (as Add a machine does, with its checks), notes it on the workflow,
/// and hands the code back in this browser's own cookie. The link is all it takes.
pub async fn command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(form): Form<CommandForm>,
) -> Response {
    let now = now_secs();
    if !same_origin(&headers, &state.hub.config.public_url) {
        return cross_site(&token);
    }
    let (row, subject) = match found(&state, &token, now).await {
        Ok((row, subject, Standing::Live)) => (row, subject),
        Ok(_) => return dead(),
        Err(page) => return page,
    };
    let site = pages::site();
    let path = link_path(&token);
    let title = Ctx::new(&site).plain(&row.definition.title);
    let Some(step) = row.definition.steps.get(form.step).filter(|step| step.kind == MACHINE) else {
        return see_other(&path, &[]);
    };
    let os = match (&step.os, form.os.as_deref()) {
        (Some(os), _) => os.clone(),
        (None, Some(os)) if state.hub.machine_oses().contains(&os) => os.to_string(),
        _ => return see_other(&path, &[]),
    };
    if let Some(why) = state.hub.machine_unavailable(&site, Some(&os)) {
        return refusal(&site, StatusCode::NOT_FOUND, &title, &why, &path);
    }
    let name = form.name.trim().to_ascii_lowercase();
    let (stored, code) = match state.hub.add_own_machine(&subject, &name) {
        Ok(issued) => issued,
        Err(error) => {
            let status = match error.code.as_str() {
                "device_store" => StatusCode::SERVICE_UNAVAILABLE,
                "early_access" | "plan_limit" | "machine_limit" => StatusCode::FORBIDDEN,
                _ => StatusCode::CONFLICT,
            };
            return refusal(&site, status, &title, &error.message, &path);
        }
    };
    let Some(workflows) = &state.hub.workflows else {
        return dead();
    };
    let (secret, keep_browser) = browser_secret(&state, &headers);
    let browser = browser_hash(&secret).unwrap_or_default();
    if let Err(error) = workflows
        .accounts()
        .note_machine(&row.tenant, &row.id, form.step, &os, &stored, &name, &browser, now)
        .await
    {
        eprintln!("devagotchi-hub: account store: {}", error.message);
        return unavailable();
    }
    workflows.changed();
    see_other(
        &format!("{path}#step-{}", form.step + 1),
        &[
            cookie(
                &state,
                code_cookie(&state),
                &format!("{}.{}.{code}", row.id, form.step),
                CODE_TTL_SECS,
            ),
            keep_browser,
        ],
    )
}

/// `POST /go/<token>/phone`: the phone's subscription (`push.js`), kept for the workflow's
/// account. The link is all it takes; this browser is remembered as one that did a step.
pub async fn phone(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(sub): Json<super::notify::Subscription>,
) -> Response {
    let now = now_secs();
    if !same_origin(&headers, &state.hub.config.public_url) {
        return super::notify::plain(StatusCode::FORBIDDEN, "not_same_origin");
    }
    let (row, subject) = match found(&state, &token, now).await {
        Ok((row, subject, Standing::Live)) => (row, subject),
        Ok(_) | Err(_) => return super::notify::plain(StatusCode::NOT_FOUND, "link_gone"),
    };
    if !row.definition.steps.iter().any(|step| step.kind == PHONE) {
        return super::notify::plain(StatusCode::NOT_FOUND, "no_phone_step");
    }
    if let Err(response) = super::notify::save(&state, &subject.id, &sub) {
        return response;
    }
    let (secret, keep_browser) = browser_secret(&state, &headers);
    if let Some(workflows) = &state.hub.workflows
        && let Err(error) = workflows
            .accounts()
            .note_browser(&row.tenant, &row.id, &browser_hash(&secret).unwrap_or_default(), now)
            .await
    {
        eprintln!("devagotchi-hub: account store: {}", error.message);
        return unavailable();
    }
    let mut response = (StatusCode::NO_CONTENT, ()).into_response();
    if let Ok(value) = HeaderValue::from_str(&keep_browser) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

#[derive(Deserialize)]
pub struct ClientForm {
    step: usize,
}

/// `POST /go/<token>/client`: before an AI client opens its sign-in page in this browser,
/// for an account nobody secured, this browser joins the account (a session of it), so the
/// client's Connect lands on this account and not a new one. The link is all it takes,
/// while it is live; a secured account signs in on the client's page instead.
pub async fn client(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(form): Form<ClientForm>,
) -> Response {
    let now = now_secs();
    if !same_origin(&headers, &state.hub.config.public_url) {
        return cross_site(&token);
    }
    let (row, subject) = match found(&state, &token, now).await {
        Ok((row, subject, Standing::Live)) => (row, subject),
        Ok(_) => return dead(),
        Err(page) => return page,
    };
    let path = link_path(&token);
    let Some(step) = row.definition.steps.get(form.step).filter(|step| step.kind == AI_CLIENT) else {
        return see_other(&path, &[]);
    };
    let (Some(workflows), Some(hub)) = (&state.hub.workflows, &state.passkeys) else {
        return dead();
    };
    if subject.provisional_until.is_none() {
        return see_other(&format!("{path}#step-{}", form.step + 1), &[]);
    }
    let (secret, keep_browser) = browser_secret(&state, &headers);
    let browser = browser_hash(&secret).unwrap_or_default();
    let client = step.client.clone().unwrap_or_default();
    match workflows
        .accounts()
        .client_session(&row.tenant, &row.id, &browser, &client, crate::link::random::<32>(), now)
        .await
    {
        Ok(session) => see_other(
            &format!("{path}#step-{}", form.step + 1),
            &[hub.cookie(hub.session_cookie(), &session, SESSION_MAX_SECS), keep_browser],
        ),
        Err(error) => match error.code.as_str() {
            "secured" | "expired" | "unknown_workflow" | "account_disabled" => see_other(&path, &[]),
            _ => {
                eprintln!("devagotchi-hub: account store: {}", error.message);
                unavailable()
            }
        },
    }
}

#[derive(Deserialize)]
pub struct KeepForm {
    /// `passkey`, or the name of a sign-in provider this hub has.
    how: String,
}

/// `POST /go/<token>/keep`: the browser that did a step keeps the account, once a step was
/// done and while nobody has secured it. A session of the account is opened in this browser
/// and it goes on to add a sign-in: the provider's (back here after), or, for a passkey, the
/// page's script makes one at once (`passkey.js`, which asked with `how=passkey`).
pub async fn keep(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Form(form): Form<KeepForm>,
) -> Response {
    let now = now_secs();
    if !same_origin(&headers, &state.hub.config.public_url) {
        return cross_site(&token);
    }
    let path = link_path(&token);
    let (row, _subject, _) = match found(&state, &token, now).await {
        Ok(found) => found,
        Err(page) => return page,
    };
    let (Some(workflows), Some(hub)) = (&state.hub.workflows, &state.passkeys) else {
        return dead();
    };
    let names = hub.provider_names();
    let provider = names.iter().find(|(name, _)| *name == form.how).map(|(name, _)| name.clone());
    if provider.is_none() && (form.how != "passkey" || !names.is_empty()) {
        return see_other(&path, &[]);
    }
    let Some(browser) = cookie_value(&headers, browser_cookie(&state)).and_then(|secret| browser_hash(&secret)) else {
        return see_other(&path, &[]);
    };
    let session = match workflows
        .accounts()
        .keep_session(&row.tenant, &row.id, &browser, workflows.link_ttl(), crate::link::random::<32>(), now)
        .await
    {
        Ok(session) => session,
        Err(error) => {
            return match error.code.as_str() {
                "not_this_browser" | "too_soon" | "secured" | "unknown_workflow" | "account_disabled" => {
                    see_other(&path, &[])
                }
                _ => {
                    eprintln!("devagotchi-hub: account store: {}", error.message);
                    unavailable()
                }
            };
        }
    };
    let signed_in = hub.cookie(hub.session_cookie(), &session, SESSION_MAX_SECS);
    match provider {
        Some(name) => see_other(
            &format!("/auth/provider/{name}?return_to={}&link=1", super::encode(&path)),
            &[signed_in],
        ),
        None => {
            let mut response = (StatusCode::NO_CONTENT, ()).into_response();
            let headers = response.headers_mut();
            if let Ok(value) = HeaderValue::from_str(&signed_in) {
                headers.append(header::SET_COOKIE, value);
            }
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
    }
}

/// The page's script: follows the steps, copies the line.
pub async fn script() -> Response {
    let mut response = include_str!("assets/workflow.js").into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}
