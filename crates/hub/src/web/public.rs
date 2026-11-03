//! The hub's public pages: what the site's product is and how to connect an AI (`/docs`),
//! the privacy policy, the terms and where to get help. They are what the AI directories
//! link to (Claude's Connectors Directory, OpenAI's plugin directory), so they describe this
//! hub as it runs, never planned behavior. Their words are the site's (`content/*.md`,
//! `site/markdown.rs`); no sign-in, no script.

use axum::{extract::State, http::StatusCode, response::Response};

use super::pages;
use crate::{AppState, site::Ctx};

/// The page `name` of the site, drawn for this hub: its own MCP address, and the copy for
/// the way it signs people in.
fn page(state: &AppState, name: &str) -> Response {
    let site = state.hub.config.site();
    let Some(page) = site.pages.get(name) else {
        return pages::message(
            StatusCode::NOT_FOUND,
            "Not found",
            "There is no such page.",
            None,
        );
    };
    let ctx = Ctx::new(&site)
        .with(
            "mcp",
            format!("{}/mcp", state.hub.config.public_url.trim_end_matches('/')),
        )
        .with("updated", page.updated.clone());
    let body = page.render(&ctx, kind(state));
    pages::html(
        StatusCode::OK,
        &ctx.plain(&page.title),
        &format!(
            "{body}<p class=\"meta\">{}</p>",
            ctx.inline(&site.messages.page_footer)
        ),
    )
}

/// Which of a page's `::: …` blocks this hub shows.
fn kind(state: &AppState) -> &'static str {
    if state.passkeys.is_some() {
        if state.hub.config.multi_tenant() {
            "hosted"
        } else {
            "passkeys"
        }
    } else if matches!(
        state.hub.config.auth,
        crate::config::AuthConfig::Nomen { .. }
    ) {
        "nomen"
    } else {
        "other"
    }
}

/// `/docs`: what the product is and how to connect an AI to this hub, at this hub's own
/// address and with the sign-in it runs.
pub async fn docs(State(state): State<AppState>) -> Response {
    page(&state, "docs")
}

pub async fn privacy(State(state): State<AppState>) -> Response {
    page(&state, "privacy")
}

pub async fn terms(State(state): State<AppState>) -> Response {
    page(&state, "terms")
}

pub async fn support(State(state): State<AppState>) -> Response {
    page(&state, "support")
}

/// The site's public page `name` as one hub of `kind` would draw it, for tests.
#[cfg(test)]
pub fn render(site: &crate::site::Site, name: &str, kind: &str, public_url: &str) -> String {
    let page = &site.pages[name];
    let ctx = Ctx::new(site)
        .with("mcp", format!("{}/mcp", public_url.trim_end_matches('/')))
        .with("updated", page.updated.clone());
    format!(
        "{}<p class=\"meta\">{}</p>",
        page.render(&ctx, kind),
        ctx.inline(&site.messages.page_footer)
    )
}
