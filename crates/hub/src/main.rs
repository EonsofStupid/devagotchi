//! devagotchi-hub: the bastion. Authenticates every caller through the identity
//! provider, routes typed operations to the broker that owns the target machine,
//! and audits each call on a hash chain. It never admits a job itself.

mod audit;
mod auth;
mod config;
mod hub;

use std::{
    env, ffi::OsString, net::SocketAddr, path::PathBuf, process::ExitCode, sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use devagotchi_contracts::{BridgeError, BridgeResult};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

use crate::{
    audit::Audit,
    auth::{AuthError, Verifier},
    config::HubConfig,
    hub::Hub,
};

const USAGE: &str = "Usage: devagotchi-hub --state-dir ABS_PATH --config ABS_JSON\n\nStarts the bastion: an authenticated Streamable HTTP MCP endpoint at /mcp that routes\noperation_start/get/read/cancel to enrolled brokers by name, with whoami, /healthz and\nRFC 9728 protected-resource metadata. Put it behind the edge for TLS.";

#[derive(Clone)]
struct AppState {
    verifier: Arc<Verifier>,
    hub: Hub,
}

fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> BridgeResult<Option<(PathBuf, PathBuf)>> {
    let args: Vec<OsString> = args.into_iter().collect();
    if args.len() == 1 && args[0] == "--help" {
        return Ok(None);
    }
    let (mut state, mut config) = (None, None);
    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        let value = iter
            .next()
            .ok_or_else(|| BridgeError::new("invalid_cli", "flag without a value"))?;
        match flag.to_str() {
            Some("--state-dir") if state.is_none() => state = Some(PathBuf::from(value)),
            Some("--config") if config.is_none() => config = Some(PathBuf::from(value)),
            _ => {
                return Err(BridgeError::new(
                    "invalid_cli",
                    "expected --state-dir ABS_PATH --config ABS_JSON",
                ));
            }
        }
    }
    match (state, config) {
        (Some(state), Some(config)) if state.is_absolute() && config.is_absolute() => {
            Ok(Some((state, config)))
        }
        _ => Err(BridgeError::new(
            "invalid_cli",
            "both --state-dir and --config are required and must be absolute",
        )),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let (state_dir, config_path) = match parse_args(env::args_os().skip(1)) {
        Ok(Some(paths)) => paths,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("devagotchi-hub: {error}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match run(state_dir, config_path).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("devagotchi-hub: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(state_dir: PathBuf, config_path: PathBuf) -> BridgeResult<()> {
    let text = std::fs::read_to_string(&config_path).map_err(|error| {
        BridgeError::new(
            "invalid_config",
            format!("{}: {error}", config_path.display()),
        )
    })?;
    let config = Arc::new(HubConfig::parse(&text)?);
    std::fs::create_dir_all(&state_dir)
        .map_err(|error| BridgeError::new("state_dir", error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| BridgeError::new("state_dir", error.to_string()))?;
    }
    let audit = Arc::new(Audit::open(&state_dir.join("audit.sqlite3"))?);
    let verifier = Arc::new(Verifier::from_config(&config.auth).await?);
    let listen: SocketAddr = config
        .listen
        .parse()
        .map_err(|_| BridgeError::new("invalid_config", "listen"))?;
    let state = AppState {
        verifier,
        hub: Hub {
            config: config.clone(),
            audit,
        },
    };

    let public_host = config
        .public_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
        .to_string();
    let mcp_config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_allowed_hosts([
            public_host.clone(),
            config.listen.clone(),
            "localhost".into(),
            "127.0.0.1".into(),
        ])
        .with_sse_keep_alive(Some(Duration::from_secs(15)));
    let mcp = {
        let hub = state.hub.clone();
        StreamableHttpService::new(
            move || Ok(hub.clone()),
            Arc::new(LocalSessionManager::default()),
            mcp_config,
        )
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .nest_service(
            "/mcp",
            Router::new()
                .fallback_service(mcp)
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    require_bearer,
                )),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| BridgeError::new("listen", format!("{listen}: {error}")))?;
    eprintln!(
        "devagotchi-hub ready: http://{listen}/mcp ({} brokers, auth {})",
        config.brokers.len(),
        match &config.auth {
            config::AuthConfig::Oidc { issuer, .. } => format!("oidc {issuer}"),
            config::AuthConfig::Dev { .. } => "dev tokens".to_string(),
        }
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|error| BridgeError::new("serve", error.to_string()))?;
    eprintln!("devagotchi-hub stopped");
    Ok(())
}

async fn require_bearer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let metadata = format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
        state.hub.config.public_url.trim_end_matches('/')
    );
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim)
        .map(str::to_string);
    let Some(token) = token else {
        return challenge(StatusCode::UNAUTHORIZED, &metadata, None);
    };
    match state.verifier.verify(&token).await {
        Ok(subject) => {
            request.extensions_mut().insert(subject);
            next.run(request).await
        }
        Err(AuthError::Invalid(_)) => {
            challenge(StatusCode::UNAUTHORIZED, &metadata, Some("invalid_token"))
        }
        Err(AuthError::Unavailable(why)) => {
            eprintln!("devagotchi-hub: issuer unavailable: {why}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "identity provider unavailable",
            )
                .into_response()
        }
    }
}

fn challenge(status: StatusCode, metadata: &str, error: Option<&str>) -> Response {
    let value = match error {
        Some(error) => format!("{metadata}, error=\"{error}\""),
        None => metadata.to_string(),
    };
    let mut response = (status, "authentication required").into_response();
    if let Ok(header_value) = HeaderValue::from_str(&value) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, header_value);
    }
    response
}

async fn protected_resource(State(state): State<AppState>) -> Response {
    let Some(issuer) = state.verifier.issuer() else {
        return (StatusCode::NOT_FOUND, "no authorization server in dev mode").into_response();
    };
    let body = serde_json::json!({
        "resource": state.hub.config.public_url,
        "authorization_servers": [issuer],
        "bearer_methods_supported": ["header"],
        "resource_name": "Devagotchi hub",
    });
    axum::Json(body).into_response()
}

async fn healthz(State(state): State<AppState>) -> Response {
    let mut brokers = Vec::new();
    let mut all_up = true;
    for broker in &state.hub.config.brokers {
        let reachable = reachable(&broker.socket).await;
        all_up &= reachable;
        brokers.push(serde_json::json!({ "name": broker.name, "reachable": reachable }));
    }
    let audit = state.hub.audit.verify();
    let chain_ok = audit.as_ref().map(|status| status.ok).unwrap_or(false);
    let body = serde_json::json!({
        "ok": all_up && chain_ok,
        "brokers": brokers,
        "audit": audit.ok(),
    });
    let status = if all_up && chain_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, axum::Json(body)).into_response()
}

/// A broker is reachable when it answers a real request over its own IPC: the same
/// owner checks, framing and endpoint a routed call uses, on every platform.
async fn reachable(socket: &str) -> bool {
    let body = devagotchi_contracts::RequestBody::OperationList(devagotchi_contracts::ListInput {
        offset: 0,
        limit: 1,
    });
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            devagotchi_ipc::request(std::path::Path::new(socket), body)
        )
        .await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const STATE: &str = "/s";
    #[cfg(windows)]
    const STATE: &str = r"C:\s";
    #[cfg(unix)]
    const CONFIG: &str = "/c.json";
    #[cfg(windows)]
    const CONFIG: &str = r"C:\c.json";

    #[test]
    fn cli_requires_both_absolute_paths() {
        let ok =
            parse_args(["--state-dir", STATE, "--config", CONFIG].map(OsString::from)).unwrap();
        assert_eq!(ok, Some((PathBuf::from(STATE), PathBuf::from(CONFIG))));
        assert!(parse_args(["--config", CONFIG].map(OsString::from)).is_err());
        assert!(parse_args(["--state-dir", "s", "--config", CONFIG].map(OsString::from)).is_err());
        assert_eq!(parse_args(["--help"].map(OsString::from)).unwrap(), None);
    }
}
