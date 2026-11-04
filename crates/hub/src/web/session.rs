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
