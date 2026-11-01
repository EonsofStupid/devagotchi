//! The hub's own OAuth 2.1 authorization server for AI clients (32b): what Claude,
//! ChatGPT, Codex or any MCP client needs to connect to `<public_url>/mcp` with nothing
//! else deployed. The store side is `accounts/oauth.rs`.
//!
//! - `/.well-known/oauth-authorization-server` (RFC 8414) names the endpoints below; the
//!   protected-resource metadata (RFC 9728, `lib.rs`) names the hub as its server.
//! - `/oauth/register` (RFC 7591): a client registers its redirect URIs. Public clients
//!   only; PKCE S256 is required of every one.
//! - `/oauth/authorize`: a person signed in with their passkey sees which client asks and
//!   where it sends them, and approves or refuses. Not signed in: the sign-in page first,
//!   then back here. The approval is a POST from this page alone (`Origin` and
//!   `Sec-Fetch-Site` checked), so another site cannot approve for them.
//! - `/oauth/token`: a code (with its PKCE verifier) or a refresh token for tokens.
//! - `/oauth/revoke` (RFC 7009): ends a token's whole family.
//!
//! Tokens are for this hub's `/mcp` and nothing else (RFC 8707 `resource`); lifetimes are
//! the config's `auth.tokens`.

use axum::{
    Form, Json,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use super::{encode, pages, routes::see_other};
use crate::{
    AppState,
    accounts::oauth::{Client, Tokens, valid_redirect_uri},
};

/// The scopes a hub's own tokens carry: its MCP tools, and refresh tokens.
pub const SCOPES: [&str; 2] = ["mcp", "offline_access"];

/// Metadata and token endpoints answer browser-based clients too (an MCP inspector): none
/// of them reads a cookie, so any origin may call them.
pub fn allow_any_origin(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type, mcp-protocol-version"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
}

/// A CORS preflight for the endpoints above.
pub async fn preflight() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    allow_any_origin(&mut response);
    response
}

fn public_url(state: &AppState) -> String {
    state.hub.config.public_url.trim_end_matches('/').to_string()
}

/// The resources a token may be for: the hub, or its `/mcp` (what clients are given).
fn resource_for(state: &AppState, asked: Option<&str>) -> Option<String> {
    let base = public_url(state);
    let mcp = format!("{base}/mcp");
    match asked.map(|r| r.trim_end_matches('/')) {
        None | Some("") => Some(mcp),
        Some(r) if r == mcp || r == base => Some(r.to_string()),
        Some(_) => None,
    }
}

/// The scopes granted: the known ones asked for, `mcp` when none are.
fn scope_for(asked: Option<&str>) -> String {
    let mut granted: Vec<&str> = asked
        .unwrap_or("")
        .split_whitespace()
        .filter(|scope| SCOPES.contains(scope))
        .collect();
    if !granted.contains(&"mcp") {
        granted.insert(0, "mcp");
    }
    granted.dedup();
    granted.join(" ")
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    let mut response = (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    allow_any_origin(&mut response);
    response
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    allow_any_origin(&mut response);
    response
}

fn store_failed(error: &devagotchi_contracts::BridgeError) -> Response {
    eprintln!("devagotchi-hub: account store: {}", error.message);
    oauth_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "temporarily_unavailable",
        "the hub could not reach its store; try again",
    )
}

/// RFC 8414: where the hub's endpoints are and what it supports.
pub async fn metadata(State(state): State<AppState>) -> Response {
    if state.passkeys.is_none() {
        return oauth_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "this hub is not an authorization server",
        );
    }
    let base = public_url(&state);
    no_store(
        Json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/oauth/authorize"),
            "token_endpoint": format!("{base}/oauth/token"),
            "registration_endpoint": format!("{base}/oauth/register"),
            "revocation_endpoint": format!("{base}/oauth/revoke"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "revocation_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": SCOPES,
            "authorization_response_iss_parameter_supported": true,
        }))
        .into_response(),
    )
}

#[derive(Deserialize)]
pub struct Registration {
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
}

/// RFC 7591: a public client registers its redirect URIs.
pub async fn register(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return oauth_error(StatusCode::NOT_FOUND, "not_found", "no authorization server");
    };
    if body.len() > 16 * 1024 {
        return oauth_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_client_metadata",
            "registration is at most 16 KiB",
        );
    }
    let Ok(asked) = serde_json::from_slice::<Registration>(&body) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "the body is a JSON client registration",
        );
    };
    if asked
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|method| method != "none")
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "this hub registers public clients: token_endpoint_auth_method none, with PKCE",
        );
    }
    match hub
        .accounts()
        .register_client(asked.client_name.as_deref().unwrap_or(""), &asked.redirect_uris)
        .await
    {
        Ok(client) => no_store(
            (
                StatusCode::CREATED,
                Json(json!({
                    "client_id": client.id,
                    "client_id_issued_at": crate::devices::now_secs(),
                    "client_name": client.name,
                    "redirect_uris": client.redirect_uris,
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                    "token_endpoint_auth_method": "none",
                })),
            )
                .into_response(),
        ),
        Err(error) if error.code.starts_with("invalid_") => {
            oauth_error(StatusCode::BAD_REQUEST, &error.code, &error.message)
        }
        Err(error) => store_failed(&error),
    }
}

/// An authorization request, as the client sent it.
#[derive(Deserialize, Clone)]
pub struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    scope: Option<String>,
    resource: Option<String>,
}

/// An authorization request that checks out: its client, and where errors may now go.
struct Checked {
    client: Client,
    redirect_uri: String,
    challenge: String,
    resource: String,
    scope: String,
    state: Option<String>,
}

/// A page about a request that cannot be sent back to its client: the client or its
/// redirect URI is unknown, so redirecting would hand a stranger the error (RFC 6749 §4.1.2.1).
fn refuse_page(why: &str) -> Response {
    pages::message(
        StatusCode::BAD_REQUEST,
        "This connection request is not valid",
        why,
        None,
    )
}

/// The client's redirect URI with the answer's parameters, and `iss` (RFC 9207).
fn back_to(redirect_uri: &str, params: &[(&str, &str)], issuer: &str) -> String {
    let mut out = redirect_uri.to_string();
    let mut sep = if out.contains('?') { '&' } else { '?' };
    for (key, value) in params.iter().chain([("iss", issuer)].iter()) {
        out.push(sep);
        out.push_str(key);
        out.push('=');
        out.push_str(&encode(value));
        sep = '&';
    }
    out
}

/// Check a request against its registered client. `Err` is the response to give: a page
/// when the client or redirect is unknown, else a redirect carrying the error.
async fn check(state: &AppState, query: &AuthorizeQuery) -> Result<Checked, Response> {
    let Some(hub) = state.passkeys.clone() else {
        return Err(super::routes::not_set_up());
    };
    let (Some(client_id), Some(redirect_uri)) = (&query.client_id, &query.redirect_uri) else {
        return Err(refuse_page("It names no client or no redirect address."));
    };
    let client = match hub.accounts().client(client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => {
            return Err(refuse_page(
                "The app asking is not registered with this hub. Add the hub to it again.",
            ));
        }
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return Err(super::routes::unavailable());
        }
    };
    if !client.redirect_uris.iter().any(|uri| uri == redirect_uri) || !valid_redirect_uri(redirect_uri) {
        return Err(refuse_page(
            "It sends you back to an address the app did not register.",
        ));
    }
    let issuer = public_url(state);
    let fail = |error: &str, description: &str| {
        let mut params = vec![("error", error), ("error_description", description)];
        if let Some(sent) = query.state.as_deref() {
            params.push(("state", sent));
        }
        see_other(&back_to(redirect_uri, &params, &issuer), &[])
    };
    if query.response_type.as_deref() != Some("code") {
        return Err(fail("unsupported_response_type", "response_type is code"));
    }
    let challenge = match (&query.code_challenge, query.code_challenge_method.as_deref()) {
        (Some(challenge), Some("S256")) if (43..=128).contains(&challenge.len()) => challenge.clone(),
        _ => return Err(fail("invalid_request", "PKCE with code_challenge_method S256 is required")),
    };
    let Some(resource) = resource_for(state, query.resource.as_deref()) else {
        return Err(fail("invalid_target", "this hub issues tokens for its own /mcp only"));
    };
    if query.state.as_deref().is_some_and(|s| s.len() > 1024) {
        return Err(fail("invalid_request", "state is at most 1024 characters"));
    }
    Ok(Checked {
        client,
        redirect_uri: redirect_uri.clone(),
        challenge,
        resource,
        scope: scope_for(query.scope.as_deref()),
        state: query.state.clone(),
    })
}

/// The request again, as this hub's own path: where sign-in returns to.
fn this_request(query: &AuthorizeQuery) -> String {
    let mut path = String::from("/oauth/authorize");
    let mut sep = '?';
    for (key, value) in [
        ("response_type", &query.response_type),
        ("client_id", &query.client_id),
        ("redirect_uri", &query.redirect_uri),
        ("code_challenge", &query.code_challenge),
        ("code_challenge_method", &query.code_challenge_method),
        ("state", &query.state),
        ("scope", &query.scope),
        ("resource", &query.resource),
    ] {
        if let Some(value) = value {
            path.push(sep);
            path.push_str(key);
            path.push('=');
            path.push_str(&encode(value));
            sep = '&';
        }
    }
    path
}

/// The approval page: which app, for which account, sending you back where.
pub async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    let checked = match check(&state, &query).await {
        Ok(checked) => checked,
        Err(response) => return response,
    };
    let hub = state.passkeys.clone().expect("checked");
    let person = match hub.who(&headers).await {
        Ok(Some(person)) => person,
        Ok(None) => {
            let back = this_request(&query);
            return see_other(&format!("/auth/login?return_to={}", encode(&back)), &[]);
        }
        Err(()) => return super::routes::unavailable(),
    };
    let Some(origin) = super::passkeys::origin_of(&checked.redirect_uri) else {
        return refuse_page("Its redirect address has no origin.");
    };
    let hidden = |name: &str, value: &str| {
        format!(
            "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
            pages::escape(value)
        )
    };
    let body = format!(
        "<h1>Connect {app}?</h1>\
<p><strong>{app}</strong> asks to use this hub as <strong>{who}</strong>: every tool on the machines you reach, \
until you disconnect it or it goes unused for {days} days.</p>\
<p class=\"meta\">It sends you back to {origin}.</p>\
<form method=\"post\" action=\"/oauth/authorize\">{fields}\
<p><button class=\"button primary\" name=\"decision\" value=\"approve\">Connect</button> \
<button class=\"button\" name=\"decision\" value=\"deny\">Cancel</button></p></form>",
        app = pages::escape(&checked.client.name),
        who = pages::escape(person.name.as_deref().unwrap_or(&person.id)),
        days = state.hub.config.token_config().refresh_secs.div_ceil(86_400),
        origin = pages::escape(&origin),
        fields = [
            hidden("client_id", &checked.client.id),
            hidden("redirect_uri", &checked.redirect_uri),
            hidden("code_challenge", &checked.challenge),
            hidden("resource", &checked.resource),
            hidden("scope", &checked.scope),
            hidden("state", checked.state.as_deref().unwrap_or("")),
        ]
        .concat(),
    );
    let mut response = pages::html(StatusCode::OK, "Connect an app", &body);
    // The approval redirects to the app: the form may lead there and nowhere else.
    if let Ok(csp) = HeaderValue::from_str(&format!(
        "default-src 'none'; style-src 'self'; form-action 'self' {origin}; frame-ancestors 'none'; base-uri 'none'"
    )) {
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    response
}

#[derive(Deserialize)]
pub struct Decision {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    resource: String,
    scope: String,
    #[serde(default)]
    state: String,
    decision: String,
}

/// The person's answer, from the approval page only.
pub async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<Decision>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if !hub.from_own_page(&headers) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not from this hub's page",
            "Approve a connection on this hub's own page.",
            None,
        );
    }
    let query = AuthorizeQuery {
        response_type: Some("code".into()),
        client_id: Some(form.client_id),
        redirect_uri: Some(form.redirect_uri),
        code_challenge: Some(form.code_challenge),
        code_challenge_method: Some("S256".into()),
        state: (!form.state.is_empty()).then_some(form.state),
        scope: Some(form.scope),
        resource: Some(form.resource),
    };
    let checked = match check(&state, &query).await {
        Ok(checked) => checked,
        Err(response) => return response,
    };
    let person = match hub.who(&headers).await {
        Ok(Some(person)) => person,
        Ok(None) => {
            let back = this_request(&query);
            return see_other(&format!("/auth/login?return_to={}", encode(&back)), &[]);
        }
        Err(()) => return super::routes::unavailable(),
    };
    let issuer = public_url(&state);
    let mut params: Vec<(&str, &str)> = Vec::new();
    let code;
    if form.decision == "approve" {
        code = match hub
            .accounts()
            .issue_code(
                &checked.client.id,
                &person.id,
                &checked.redirect_uri,
                &checked.challenge,
                &checked.resource,
                &checked.scope,
            )
            .await
        {
            Ok(code) => code,
            Err(error) => {
                eprintln!("devagotchi-hub: account store: {}", error.message);
                return super::routes::unavailable();
            }
        };
        params.push(("code", &code));
    } else {
        params.push(("error", "access_denied"));
    }
    if let Some(sent) = checked.state.as_deref() {
        params.push(("state", sent));
    }
    see_other(&back_to(&checked.redirect_uri, &params, &issuer), &[])
}

#[derive(Deserialize)]
pub struct TokenRequest {
    grant_type: String,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

fn tokens_json(tokens: Tokens) -> Response {
    no_store(
        Json(json!({
            "access_token": tokens.access,
            "token_type": "Bearer",
            "expires_in": tokens.expires_in,
            "refresh_token": tokens.refresh,
            "scope": tokens.scope,
        }))
        .into_response(),
    )
}

/// A code or a refresh token for tokens (RFC 6749 §4.1.3, §6).
pub async fn token(State(state): State<AppState>, Form(asked): Form<TokenRequest>) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return oauth_error(StatusCode::NOT_FOUND, "not_found", "no authorization server");
    };
    let Some(client) = asked.client_id.as_deref() else {
        return oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "client_id is required");
    };
    let resource = asked.resource.as_deref();
    if resource.is_some() && resource_for(&state, resource).is_none() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "this hub issues tokens for its own /mcp only",
        );
    }
    let resource = resource.map(|r| r.trim_end_matches('/'));
    let done = match asked.grant_type.as_str() {
        "authorization_code" => {
            let (Some(code), Some(redirect_uri), Some(verifier)) =
                (&asked.code, &asked.redirect_uri, &asked.code_verifier)
            else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "code, redirect_uri and code_verifier are required",
                );
            };
            hub.accounts()
                .exchange_code(code, client, redirect_uri, verifier, resource)
                .await
        }
        "refresh_token" => {
            let Some(refresh) = &asked.refresh_token else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "refresh_token is required",
                );
            };
            hub.accounts().refresh(refresh, client, resource).await
        }
        _ => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "authorization_code or refresh_token",
            );
        }
    };
    match done {
        Ok(tokens) => tokens_json(tokens),
        Err(error) if error.code == "invalid_grant" || error.code == "account_disabled" => {
            oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", &error.message)
        }
        Err(error) => store_failed(&error),
    }
}

#[derive(Deserialize)]
pub struct Revocation {
    token: String,
}

/// RFC 7009: end a token's family. Unknown tokens answer the same.
pub async fn revoke(State(state): State<AppState>, Form(asked): Form<Revocation>) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return oauth_error(StatusCode::NOT_FOUND, "not_found", "no authorization server");
    };
    match hub.accounts().revoke_token(&asked.token).await {
        Ok(()) => no_store(StatusCode::OK.into_response()),
        Err(error) => store_failed(&error),
    }
}
