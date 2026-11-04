//! Into an account once a sign-in proved it (`providers::finish`, `sign_in.rs`): the
//! browser's session goes on if it is already that account's, else a new session opens,
//! and the browser goes back where it was going, with any other cookies the sign-in sets.

use std::sync::Arc;

use axum::response::Response;

use super::{HubSignIn, routes::see_other};
use crate::{accounts::Account, auth::Subject};

pub(super) async fn open_and_return(
    hub: &Arc<HubSignIn>,
    current: Option<&Subject>,
    account: &Account,
    return_to: &str,
    extra_cookies: &[String],
) -> Response {
    if account.owner {
        hub.learn_owner_of(&account.id);
    }
    // Still the same account: the session goes on.
    if current.is_some_and(|c| c.id == account.id) {
        return see_other(return_to, extra_cookies);
    }
    match hub.accounts().open_session(&account.id, None).await {
        Ok(token) => {
            let mut cookies = vec![hub.cookie(
                hub.session_cookie(),
                &token,
                crate::accounts::sessions::SESSION_MAX_SECS,
            )];
            cookies.extend_from_slice(extra_cookies);
            see_other(return_to, &cookies)
        }
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            super::routes::unavailable()
        }
    }
}

/// A page whose form signs in and returns to `return_to`: when that is an AI app's authorize
/// request, the browser follows the answer on to the app's redirect address, which the
/// page's `form-action` must name too (a browser holds a form's redirects to it).
pub(super) fn form_may_return(response: &mut Response, return_to: &str) {
    let Some(query) = return_to.strip_prefix("/oauth/authorize?") else {
        return;
    };
    let redirect = query.split('&').find_map(|pair| pair.strip_prefix("redirect_uri="));
    let decoded = redirect.and_then(|value| percent_encoding::percent_decode_str(value).decode_utf8().ok());
    let Some(origin) = decoded.and_then(|uri| super::passkeys::origin_of(&uri)) else {
        return;
    };
    let headers = response.headers_mut();
    let policy = headers.get(axum::http::header::CONTENT_SECURITY_POLICY).and_then(|v| v.to_str().ok());
    let Some(policy) = policy.map(|p| p.replace("form-action 'self'", &format!("form-action 'self' {origin}"))) else {
        return;
    };
    if let Ok(value) = axum::http::HeaderValue::from_str(&policy) {
        headers.insert(axum::http::header::CONTENT_SECURITY_POLICY, value);
    }
}
