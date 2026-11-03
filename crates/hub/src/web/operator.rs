//! `/operator`: where a hosted hub's operator lets people in and sets their plans
//! (increment 64; what it changes is `accounts/access.rs`'s).
//!
//! - Served only to the hub's owner, signed in, on a hosted hub (`tenancy.mode multi`): the
//!   platform operator, whose `operators` sign-ins reach no other account. Anyone else, and
//!   any other hub, gets the same empty 404 an unknown path gets, so the page does not
//!   reveal itself.
//! - The waiting list: each tenant's joining, sign-ins, whether it is secured, the AI app
//!   that made it and its last activity; check any number and let them in at once, each on
//!   the plan picked on its row. The list of those let in: each one's plan (changed on its
//!   row), its use against the plan's limits, and when and by whom it was let in.
//! - A status line says what `early_access.mode` is now and how to change it: in the site,
//!   never here.
//! - Its analytics (`/operator/analytics`, `analytics.rs`) are a link away.
//! - Server-rendered forms, no script; posts are taken only from this hub's own pages
//!   (`same_origin`), like the approvals and machines pages. Every word is the site's
//!   (`content/messages.json` `operator`).

use std::collections::BTreeMap;

use axum::{
    Form,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::{
    approvals::same_origin,
    pages::{self, escape, utc},
    routes::see_other,
};
use crate::{
    AppState,
    accounts::{
        access::{self, ADMITTED, WAITING},
        operator::TenantView,
    },
    auth::Subject,
    site::{Cap, Ctx, Plan, SignUp, Site, model::LIMIT_KEYS},
};

/// The most tenants each list shows.
const MOST: u32 = 200;

/// The signed-in operator behind this request, if it is one.
pub(super) async fn operator(state: &AppState, headers: &HeaderMap) -> Option<Subject> {
    let hub = state.passkeys.as_ref()?;
    if !state.hub.config.multi_tenant() {
        return None;
    }
    let subject = hub.who(headers).await.ok().flatten()?;
    state.hub.config.is_owner(&subject.id).then_some(subject)
}

/// What anyone but the operator gets: an unknown path's answer.
pub(super) fn hidden() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// A post from another site: nothing changes.
fn not_from_here() -> Response {
    let site = pages::site();
    let copy = &site.messages.operator;
    pages::message(
        StatusCode::FORBIDDEN,
        &copy.title,
        &copy.not_from_here,
        Some(("/operator", &copy.title)),
    )
}

#[derive(Deserialize)]
pub struct Done {
    #[serde(default)]
    done: Option<String>,
    #[serde(default)]
    n: Option<u32>,
}

pub async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(done): Query<Done>,
) -> Response {
    let Some(_operator) = operator(&state, &headers).await else {
        return hidden();
    };
    let Some(hub) = &state.passkeys else {
        return hidden();
    };
    let accounts = hub.accounts();
    let (waiting, admitted) = match (
        accounts.tenant_views(WAITING, MOST).await,
        accounts.tenant_views(ADMITTED, MOST).await,
    ) {
        (Ok(waiting), Ok(admitted)) => (waiting, admitted),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let site = pages::site();
    let copy = &site.messages.operator;
    let status = match site.sign_up() {
        SignUp::Open => &copy.status_open,
        SignUp::EarlyAccess => &copy.status_early_access,
    };
    let done = match (done.done.as_deref(), done.n) {
        (Some("admitted"), n) => Some(
            Ctx::new(&site)
                .with("n", n.unwrap_or(0).to_string())
                .inline(&copy.done_admitted),
        ),
        (Some("plan"), _) => Some(Ctx::new(&site).inline(&copy.done_plan)),
        (Some("none"), _) => Some(Ctx::new(&site).inline(&copy.none_picked)),
        _ => None,
    }
    .map(|text| format!("<p class=\"note\" role=\"status\">{text}</p>"))
    .unwrap_or_default();
    let mut usage = Vec::with_capacity(admitted.len());
    for view in &admitted {
        usage.push(usage_of(&state, &site, view));
    }
    let body = format!(
        "<h1>{}</h1><p class=\"meta\" id=\"sign-up\">{} {}</p>\
<p class=\"meta\" id=\"analytics-link\">{}</p>{done}{}{}",
        escape(&copy.title),
        Ctx::new(&site).inline(status),
        Ctx::new(&site).inline(&copy.how_to_change),
        Ctx::new(&site).inline(&site.messages.analytics.operator_link),
        waiting_list(&site, &waiting),
        admitted_list(&site, &admitted, &usage),
    );
    pages::html(StatusCode::OK, &copy.title, &body)
}

/// A tenant's sign-ins: each provider as the site calls it, with its verified address.
fn sign_ins(site: &Site, view: &TenantView) -> String {
    if view.sign_ins.is_empty() {
        return escape(&site.messages.operator.no_sign_ins);
    }
    view.sign_ins
        .iter()
        .map(|linked| {
            let name = escape(site.label(&linked.provider, &linked.provider));
            match &linked.email {
                Some(email) => format!("{name} ({})", escape(email)),
                None => name,
            }
        })
        .collect::<Vec<_>>()
        .join("<br>")
}

/// The site's plans as options, `chosen` selected.
fn plan_options(site: &Site, chosen: Option<&str>) -> String {
    site.info
        .plans
        .iter()
        .map(|plan| {
            format!(
                "<option value=\"{}\"{}>{}</option>",
                escape(&plan.id),
                if chosen == Some(plan.id.as_str()) {
                    " selected"
                } else {
                    ""
                },
                escape(&Ctx::new(site).plain(&plan.name))
            )
        })
        .collect()
}

fn named(site: &Site, template: &str, tenant: &str) -> String {
    escape(&Ctx::new(site).with("tenant", tenant).plain(template))
}

fn waiting_list(site: &Site, waiting: &[TenantView]) -> String {
    let copy = &site.messages.operator;
    let columns = &copy.columns;
    let heading = format!(
        "<h2 id=\"waiting-title\">{} ({})</h2>",
        escape(&copy.waiting_title),
        waiting.len()
    );
    if waiting.is_empty() {
        return format!(
            "<section id=\"waiting\" aria-labelledby=\"waiting-title\">{heading}\
<p class=\"empty\">{}</p></section>",
            escape(&copy.waiting_empty)
        );
    }
    let default = site.default_plan().map(|plan| plan.id.as_str());
    let never = escape(&copy.never);
    let rows: String = waiting
        .iter()
        .map(|view| {
            let tenant = escape(&view.tenant);
            format!(
                "<tr><td><input type=\"checkbox\" name=\"tenant\" value=\"{tenant}\" aria-label=\"{}\"></td>\
<td><code>{tenant}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
<td><select name=\"plan.{tenant}\" aria-label=\"{}\">{}</select></td></tr>",
                named(site, &copy.pick, &view.tenant),
                view.access.waiting_since.map(utc).unwrap_or_else(|| never.clone()),
                sign_ins(site, view),
                escape(if view.secured { &copy.secured } else { &copy.not_secured }),
                view.made_by.as_deref().map(escape).unwrap_or_else(|| never.clone()),
                view.last_active.map(utc).unwrap_or_else(|| never.clone()),
                named(site, &copy.plan_for, &view.tenant),
                plan_options(site, default),
            )
        })
        .collect();
    format!(
        "<section id=\"waiting\" aria-labelledby=\"waiting-title\">{heading}\
<form method=\"post\" action=\"/operator/admit\"><div class=\"wide\"><table>\
<thead><tr><th scope=\"col\"><span class=\"visually-hidden\">{}</span></th><th scope=\"col\">{}</th>\
<th scope=\"col\">{}</th><th scope=\"col\">{}</th><th scope=\"col\">{}</th><th scope=\"col\">{}</th>\
<th scope=\"col\">{}</th><th scope=\"col\">{}</th></tr></thead><tbody>{rows}</tbody></table></div>\
<p class=\"actions\"><button class=\"approve\" type=\"submit\">{}</button></p></form></section>",
        escape(&copy.admit),
        escape(&columns.tenant),
        escape(&columns.joined),
        escape(&columns.sign_ins),
        escape(&columns.secured),
        escape(&columns.made_by),
        escape(&columns.active),
        escape(&columns.plan),
        escape(&copy.admit),
    )
}

/// A tenant's use of each limit against its plan, in the site's words.
fn usage_of(state: &AppState, site: &Site, view: &TenantView) -> String {
    let copy = &site.messages.operator;
    let plan = access::plan_for(site, &view.tenant, &view.access);
    let items: String = LIMIT_KEYS
        .iter()
        .map(|key| {
            let count = match *key {
                "machines" => view
                    .accounts
                    .iter()
                    .map(|account| state.hub.links.held(account).unwrap_or(0))
                    .sum(),
                "projects" => view
                    .accounts
                    .iter()
                    .map(|account| state.hub.boards.account_boards(account).len())
                    .sum(),
                _ => view.clients,
            };
            let text = match plan.as_ref().map(|plan| plan.cap(key)) {
                Some(Cap::AtMost(most)) => Ctx::new(site)
                    .with("count", count.to_string())
                    .with("limit", site.limit_words(key, most as usize))
                    .plain(&copy.usage_of),
                Some(Cap::Unlimited) => Ctx::new(site)
                    .with("count", site.limit_words(key, count))
                    .plain(&copy.usage_unlimited),
                Some(Cap::Unset) | None => site.limit_words(key, count),
            };
            format!("<li>{}</li>", escape(&text))
        })
        .collect();
    format!("<ul class=\"usage\">{items}</ul>")
}

fn admitted_list(site: &Site, admitted: &[TenantView], usage: &[String]) -> String {
    let copy = &site.messages.operator;
    let columns = &copy.columns;
    let heading = format!(
        "<h2 id=\"admitted-title\">{} ({})</h2>",
        escape(&copy.admitted_title),
        admitted.len()
    );
    if admitted.is_empty() {
        return format!(
            "<section id=\"admitted\" aria-labelledby=\"admitted-title\">{heading}\
<p class=\"empty\">{}</p></section>",
            escape(&copy.admitted_empty)
        );
    }
    let never = escape(&copy.never);
    let rows: String = admitted
        .iter()
        .zip(usage)
        .map(|(view, usage)| {
            let plan: Option<Plan> = access::plan_for(site, &view.tenant, &view.access);
            let tenant = escape(&view.tenant);
            let when = view
                .access
                .admitted_at
                .map(utc)
                .unwrap_or_else(|| never.clone());
            let by = view.access.admitted_by.clone().unwrap_or_default();
            format!(
                "<tr><td><code>{tenant}</code></td><td>{}</td><td>{}</td>\
<td><form class=\"row-form\" method=\"post\" action=\"/operator/plan\">\
<input type=\"hidden\" name=\"tenant\" value=\"{tenant}\">\
<select name=\"plan\" aria-label=\"{}\">{}</select> <button type=\"submit\">{}</button></form></td>\
<td>{usage}</td></tr>",
                sign_ins(site, view),
                escape(
                    &Ctx::new(site)
                        .with("when", when)
                        .with("by", by)
                        .plain(&copy.admitted_line)
                ),
                named(site, &copy.plan_for, &view.tenant),
                plan_options(site, plan.as_ref().map(|plan| plan.id.as_str())),
                escape(&copy.change_plan),
            )
        })
        .collect();
    format!(
        "<section id=\"admitted\" aria-labelledby=\"admitted-title\">{heading}<div class=\"wide\"><table>\
<thead><tr><th scope=\"col\">{}</th><th scope=\"col\">{}</th><th scope=\"col\">{}</th>\
<th scope=\"col\">{}</th><th scope=\"col\">{}</th></tr></thead><tbody>{rows}</tbody></table></div></section>",
        escape(&columns.tenant),
        escape(&columns.sign_ins),
        escape(&columns.admitted),
        escape(&columns.plan),
        escape(&columns.usage),
    )
}

/// `POST /operator/admit`: `tenant` once per checked row, `plan.<tenant>` per row. Each
/// checked tenant is let in on its row's plan.
pub async fn admit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(fields): Form<Vec<(String, String)>>,
) -> Response {
    let Some(operator) = operator(&state, &headers).await else {
        return hidden();
    };
    if !same_origin(&headers, &state.hub.config.public_url) {
        return not_from_here();
    }
    let Some(hub) = &state.passkeys else {
        return hidden();
    };
    let plans: BTreeMap<&str, &str> = fields
        .iter()
        .filter_map(|(key, value)| Some((key.strip_prefix("plan.")?, value.as_str())))
        .collect();
    let mut by_plan: BTreeMap<Option<&str>, Vec<String>> = BTreeMap::new();
    for (key, tenant) in &fields {
        if key == "tenant" {
            by_plan
                .entry(plans.get(tenant.as_str()).copied())
                .or_default()
                .push(tenant.clone());
        }
    }
    if by_plan.is_empty() {
        return see_other("/operator?done=none#waiting", &[]);
    }
    let mut let_in = 0;
    for (plan, tenants) in &by_plan {
        match hub.accounts().admit(tenants, *plan, &operator.id).await {
            Ok(admitted) => let_in += admitted.len(),
            Err(error) => return refused(&error),
        }
    }
    see_other(&format!("/operator?done=admitted&n={let_in}#waiting"), &[])
}

#[derive(Deserialize)]
pub struct PlanChange {
    tenant: String,
    plan: String,
}

/// `POST /operator/plan`: put one tenant on another plan.
pub async fn plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(change): Form<PlanChange>,
) -> Response {
    let Some(operator) = operator(&state, &headers).await else {
        return hidden();
    };
    if !same_origin(&headers, &state.hub.config.public_url) {
        return not_from_here();
    }
    let Some(hub) = &state.passkeys else {
        return hidden();
    };
    match hub
        .accounts()
        .set_plan(&change.tenant, &change.plan, &operator.id)
        .await
    {
        Ok(()) => see_other("/operator?done=plan#admitted", &[]),
        Err(error) => refused(&error),
    }
}

/// A step the store refused (a plan the site does not list, a tenant it does not have).
fn refused(error: &devagotchi_contracts::BridgeError) -> Response {
    let status = match error.code.as_str() {
        "unknown_plan" | "unknown_tenant" => StatusCode::BAD_REQUEST,
        _ => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let site = pages::site();
    pages::message(
        status,
        &site.messages.operator.title,
        &error.message,
        Some(("/operator", &site.messages.operator.title)),
    )
}
