//! Counting visits to the public pages as the hub serves them (`crate::analytics`): no
//! script, no cookie, no beacon. Each request the web router answers passes here; a `GET`
//! of a public page answered `200` is counted unless `visit::seen` or the bar
//! (`OperatorVisit`, the signed-in operator) says not to. The count is queued, never
//! waited on.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};

use crate::{
    AppState,
    analytics::{OperatorVisit, View, day_of, visit},
    devices::now_secs,
};

pub async fn count(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(analytics) = state.hub.analytics.clone() else {
        return next.run(request).await;
    };
    let view = if request.extensions().get::<OperatorVisit>().is_some() {
        None
    } else {
        visit::seen(
            request.method(),
            request.uri(),
            request.headers(),
            analytics.own_host(),
        )
        .ok()
        .map(|seen| {
            let now = now_secs();
            let day = day_of(now);
            let address =
                super::hosted::client_address(&state, request.extensions(), request.headers());
            View {
                day,
                hour: (crate::store::secs(now) - day) / 3600,
                path: seen.path,
                utm: seen.utm,
                referrer: seen.referrer,
                device: seen.device,
                visitor: analytics.visitor(day, address, seen.agent),
            }
        })
    };
    let response = next.run(request).await;
    if let Some(view) = view
        && response.status() == StatusCode::OK
    {
        analytics.page_view(view);
    }
    response
}
