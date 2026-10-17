//! A small native inspector. The independently running broker owns every job.

use std::{ffi::OsString, path::{Path, PathBuf}};
use desktop_bridge::contracts::{
    BridgeError, BridgeResult, GetInput, ListInput, OperationsPage, OperationView,
    OutputPage, ReadInput, RequestBody, ResponseBody,
};
use serde::Serialize;
use tauri::{State, WebviewUrl, WebviewWindowBuilder};

pub const USAGE: &str = "Usage:\n  devagotchi [--socket ABS_PATH]\n  devagotchi --help\n\nInspect an independently running local broker.\nDEVAGOTCHI_SOCKET supplies the socket path when --socket is omitted.\nWith neither set, the app opens its connection setup view.\nThe app does not start a broker or modify its grants.";

#[derive(Debug, Clone)]
pub struct Connection {
    socket: Option<PathBuf>,
    source: &'static str,
}

#[derive(Debug)]
pub enum Launch {
    Help,
    Open(Connection),
}

pub fn parse_launch(
    args: impl IntoIterator<Item = OsString>,
    environment_socket: Option<OsString>,
) -> BridgeResult<Launch> {
    let args: Vec<OsString> = args.into_iter().collect();
    let (socket, source) = match args.as_slice() {
        [flag] if flag == "--help" => return Ok(Launch::Help),
        [flag, path] if flag == "--socket" => (Some(PathBuf::from(path)), "command_line"),
        [] => match environment_socket {
            Some(path) => (Some(PathBuf::from(path)), "environment"),
            None => (None, "unconfigured"),
        },
        _ => return Err(BridgeError::new("invalid_cli", "expected --socket ABS_PATH, --help, or no arguments")),
    };
    if socket.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err(BridgeError::new("invalid_cli", "the broker socket path must be absolute"));
    }
    Ok(Launch::Open(Connection { socket, source }))
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionInfo {
    socket_path: Option<String>,
    source: &'static str,
    platform: &'static str,
    app_version: &'static str,
    transport: &'static str,
}

impl Connection {
    fn info(&self) -> ConnectionInfo {
        ConnectionInfo {
            socket_path: self.socket.as_ref().map(|path| path.to_string_lossy().into_owned()),
            source: self.source,
            platform: std::env::consts::OS,
            app_version: env!("CARGO_PKG_VERSION"),
            transport: "local_ipc",
        }
    }

    fn socket(&self) -> BridgeResult<&Path> {
        self.socket.as_deref().ok_or_else(|| BridgeError::new(
            "socket_unconfigured",
            "Set DEVAGOTCHI_SOCKET, or launch Devagotchi with --socket followed by an absolute broker socket path.",
        ))
    }

    async fn request(&self, body: RequestBody) -> BridgeResult<ResponseBody> {
        // No shell/plugin execution, automatic broker startup, implicit cancel,
        // retries of effects, or grant changes occur in the desktop host.
        desktop_bridge::ipc::request(self.socket()?, body).await
    }

    async fn list(&self, input: ListInput) -> BridgeResult<OperationsPage> {
        match self.request(RequestBody::OperationList(input)).await? {
            ResponseBody::Operations(value) => Ok(value),
            other => Err(unexpected("operations", &other)),
        }
    }

    async fn get(&self, input: GetInput) -> BridgeResult<OperationView> {
        match self.request(RequestBody::OperationGet(input)).await? {
            ResponseBody::Operation(value) => Ok(value),
            other => Err(unexpected("operation", &other)),
        }
    }

    async fn read(&self, input: ReadInput) -> BridgeResult<OutputPage> {
        match self.request(RequestBody::OperationRead(input)).await? {
            ResponseBody::Output(value) => Ok(value),
            other => Err(unexpected("output", &other)),
        }
    }

    async fn cancel(&self, input: GetInput) -> BridgeResult<OperationView> {
        match self.request(RequestBody::OperationCancel(input)).await? {
            ResponseBody::Operation(value) => Ok(value),
            other => Err(unexpected("operation", &other)),
        }
    }
}

fn unexpected(expected: &str, actual: &ResponseBody) -> BridgeError {
    let actual = match actual {
        ResponseBody::Start(_) => "start",
        ResponseBody::Operation(_) => "operation",
        ResponseBody::Output(_) => "output",
        ResponseBody::Operations(_) => "operations",
    };
    BridgeError::new("unexpected_broker_response", format!("Expected {expected}, received {actual}"))
}

#[tauri::command]
fn connection_info(state: State<'_, Connection>) -> ConnectionInfo {
    state.info()
}

#[tauri::command]
async fn list_operations(state: State<'_, Connection>, input: ListInput) -> BridgeResult<OperationsPage> {
    state.list(input).await
}

#[tauri::command]
async fn get_operation(state: State<'_, Connection>, input: GetInput) -> BridgeResult<OperationView> {
    state.get(input).await
}

#[tauri::command]
async fn read_output(state: State<'_, Connection>, input: ReadInput) -> BridgeResult<OutputPage> {
    state.read(input).await
}

#[tauri::command]
async fn cancel_operation(state: State<'_, Connection>, input: GetInput) -> BridgeResult<OperationView> {
    state.cancel(input).await
}

fn local_navigation(scheme: &str, host: Option<&str>, port: Option<u16>, development: bool) -> bool {
    (scheme == "tauri" && host == Some("localhost") && port.is_none())
        || (scheme == "http" && host == Some("tauri.localhost") && port == Some(80))
        || (development && scheme == "http" && host == Some("127.0.0.1") && port == Some(1420))
}

pub fn run(connection: Connection) -> tauri::Result<()> {
    tauri::Builder::default()
        .manage(connection)
        .invoke_handler(tauri::generate_handler![
            connection_info, list_operations, get_operation, read_output, cancel_operation,
        ])
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Devagotchi")
                .inner_size(1380.0, 900.0)
                .min_inner_size(960.0, 640.0)
                .center()
                .theme(Some(tauri::Theme::Dark))
                .on_navigation(|url| local_navigation(
                    url.scheme(), url.host_str(), url.port_or_known_default(), cfg!(debug_assertions),
                ))
                .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_configuration_is_explicit() {
        assert!(matches!(parse_launch([OsString::from("--help")], None).unwrap(), Launch::Help));
        let Launch::Open(empty) = parse_launch([], None).unwrap() else { panic!("expected connection") };
        assert_eq!(empty.info().source, "unconfigured");
        assert_eq!(empty.socket().unwrap_err().code, "socket_unconfigured");
        assert!(parse_launch(["--socket".into(), "relative.sock".into()], None).is_err());
        assert!(parse_launch(["--start-broker".into()], None).is_err());
    }

    #[test]
    fn navigation_cannot_leave_the_app() {
        assert!(local_navigation("tauri", Some("localhost"), None, false));
        assert!(local_navigation("http", Some("tauri.localhost"), Some(80), false));
        assert!(local_navigation("http", Some("127.0.0.1"), Some(1420), true));
        assert!(!local_navigation("http", Some("127.0.0.1"), Some(1420), false));
        assert!(!local_navigation("https", Some("devagotchi.net"), Some(443), true));
        assert!(!local_navigation("file", None, None, true));
        assert!(!local_navigation("tauri", Some("elsewhere"), None, false));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_request_preserves_paging_and_rejects_wrong_response_kind() {
        use std::os::unix::fs::PermissionsExt;
        use desktop_bridge::{contracts::{WireRequest, WireResponse, MAX_FRAME_BYTES}, ipc};
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = directory.path().join("test.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let connection = Connection { socket: Some(socket), source: "command_line" };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let raw = ipc::read_frame(&mut stream, MAX_FRAME_BYTES).await.unwrap();
            let request: WireRequest = serde_json::from_slice(&raw).unwrap();
            match request.request {
                RequestBody::OperationList(input) => {
                    assert_eq!(input.offset, 64);
                    assert_eq!(input.limit, 16);
                }
                other => panic!("unexpected request: {other:?}"),
            }
            let result = WireResponse::Ok(ResponseBody::Operations(OperationsPage {
                operations: vec![], next_offset: Some(80),
            }));
            ipc::write_frame(&mut stream, &serde_json::to_vec(&result).unwrap(), ipc::MAX_RESPONSE_BYTES).await.unwrap();
        });
        let output = connection.list(ListInput { offset: 64, limit: 16 }).await.unwrap();
        assert_eq!(output.next_offset, Some(80));
        server.await.unwrap();
        let mismatch = unexpected("output", &ResponseBody::Operations(output));
        assert_eq!(mismatch.code, "unexpected_broker_response");
    }
}
