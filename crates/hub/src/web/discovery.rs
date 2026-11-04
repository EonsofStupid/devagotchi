//! The routes search engines and AI agents read (`site/discovery.rs` makes their words):
//! `/llms.txt`, `/llms-full.txt`, `/robots.txt` and `/sitemap.xml`. No sign-in, nothing
//! counted as a visit, and every address in them is this hub's own `public_url`.

use axum::{
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};

use super::public::{kind, switch};
use crate::{AppState, site::discovery};

fn text(body: String, content_type: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    // A site read again changes them at once; caches check back.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// `/llms.txt`: what this hub is and the steps an AI follows to give its user a server.
pub async fn llms(State(state): State<AppState>) -> Response {
    let site = state.hub.config.site();
    text(
        discovery::llms_txt(&site, &state.hub.config.public_url, kind(&state)),
        "text/plain; charset=utf-8",
    )
}

/// `/llms-full.txt`: `/llms.txt` with every public page inlined.
pub async fn llms_full(State(state): State<AppState>) -> Response {
    let site = state.hub.config.site();
    text(
        discovery::llms_full_txt(
            &site,
            &state.hub.config.public_url,
            kind(&state),
            switch(&state),
            state.hub.config.analytics.retention_days,
        ),
        "text/plain; charset=utf-8",
    )
}

pub async fn robots(State(state): State<AppState>) -> Response {
    let site = state.hub.config.site();
    text(
        discovery::robots_txt(&site, &state.hub.config.public_url),
        "text/plain; charset=utf-8",
    )
}

/// `/sitemap.xml`: the public pages; `/` only where it is a landing page (a hub with no
/// web sign-in sends it to `/docs`).
pub async fn sitemap(State(state): State<AppState>) -> Response {
    let site = state.hub.config.site();
    let landing = state.passkeys.is_some() || state.web.is_some();
    text(
        discovery::sitemap_xml(&site, &state.hub.config.public_url, landing),
        "application/xml; charset=utf-8",
    )
}
