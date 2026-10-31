//! The broker runs independently from the MCP client process. This library is its
//! whole behaviour; `devagotchi start` and `devagotchi broker run` both end in [`serve`].

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

pub mod background;

pub const USAGE: &str = "Usage: devagotchi broker run --state-dir ABS_PATH --config ABS_JSON\n\nStarts a private per-user local broker. The state parent directory must exist.\nExecutable grants authorize process execution as your OS user, not a sandbox.\nThe ready line names the endpoint clients pass to `devagotchi mcp --socket`: a Unix socket path\non Linux, or a \\\\.\\pipe\\ named-pipe path on Windows; the same endpoint is written to\n<state>/endpoint for `devagotchi mcp` to find on its own. The macOS provider is pending.";

/// The file beside the socket that names the endpoint, for clients that know only the
/// state directory (`devagotchi mcp` with no `--socket`).
pub const ENDPOINT_FILE: &str = "endpoint";

/// The broker's command line: `[run] --state-dir ABS_PATH --config ABS_JSON`, or `--help`.
pub fn cli(args: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let args: Vec<String> = match args.first().map(String::as_str) {
        Some("run") => args[1..].to_vec(),
        _ => args,
    };
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let (mut state, mut config) = (None, None);
    if args.len() != 4 {
        eprintln!("devagotchi broker: expected --state-dir ABS_PATH --config ABS_JSON; use --help");
        return ExitCode::from(64);
    }
    for [flag, raw] in args.as_chunks::<2>().0 {
        let value = PathBuf::from(raw);
        if !value.is_absolute() {
            eprintln!("devagotchi broker: all paths must be absolute");
            return ExitCode::from(64);
        }
        match flag.as_str() {
            "--state-dir" if state.is_none() => state = Some(value),
            "--config" if config.is_none() => config = Some(value),
            _ => {
                eprintln!("devagotchi broker: unknown or repeated option {flag}; use --help");
                return ExitCode::from(64);
            }
        }
    }
    let (Some(state), Some(config)) = (state, config) else {
        eprintln!("devagotchi broker: --state-dir and --config are both required");
        return ExitCode::from(64);
    };
    match serve_blocking(state, config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("devagotchi broker: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Run the broker on a runtime of its own until it is told to stop.
pub fn serve_blocking(state: PathBuf, config: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(state, config))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub async fn serve(_state: PathBuf, _config: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    Err("unsupported_platform: native execution providers exist for Linux and Windows; the macOS provider is pending".into())
}

/// Operating-system requests to stop, besides Ctrl+C.
#[cfg(target_os = "linux")]
struct Termination(tokio::signal::unix::Signal);

#[cfg(target_os = "linux")]
impl Termination {
    fn new() -> std::io::Result<Self> {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map(Self)
    }

    async fn recv(&mut self) {
        self.0.recv().await;
    }
}

/// Operating-system requests to stop, besides Ctrl+C: Ctrl+Break, console
/// close, and system shutdown.
#[cfg(windows)]
struct Termination {
    ctrl_break: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl Termination {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_shutdown};
        Ok(Self {
            ctrl_break: ctrl_break()?,
            close: ctrl_close()?,
            shutdown: ctrl_shutdown()?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.ctrl_break.recv() => {}
            _ = self.close.recv() => {}
            _ = self.shutdown.recv() => {}
        }
    }
}

/// Serve one broker: bind the endpoint, open the journal, answer IPC until Ctrl+C or a
/// termination signal, then settle the jobs. The endpoint is also written to
/// `<state>/endpoint` once bound.
#[cfg(any(target_os = "linux", windows))]
pub async fn serve(state: PathBuf, config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use devagotchi_contracts::*;
    use devagotchi_core::broker::{Broker, BrokerConfig};
    use devagotchi_ipc::{self as ipc, StateGuard};
    use std::{io::Read, sync::Arc};

    let metadata = std::fs::metadata(&config_path)?;
    if !metadata.is_file() || metadata.len() > MAX_FRAME_BYTES as u64 {
        return Err("Config must be a regular file no larger than 64 KiB".into());
    }
    // Read with a cap even if the file grows after metadata inspection.
    let mut file = std::fs::File::open(config_path)?;
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, (MAX_FRAME_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("Config exceeds 64 KiB".into());
    }
    let config: BrokerConfig = serde_json::from_slice(&bytes)?;
    let mut guard = StateGuard::acquire(&state)?;
    // Bind before opening the journal, so a foreign endpoint prevents reconciliation.
    let mut listener = guard.bind().await?;
    let broker = Broker::open(config, &state)?;
    write_endpoint_file(&state, guard.endpoint());
    eprintln!("devagotchi broker ready: {}", guard.endpoint().display());

    let permits = Arc::new(tokio::sync::Semaphore::new(32));
    let mut connections = tokio::task::JoinSet::new();
    let mut terminate = Termination::new()?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = joined { eprintln!("IPC connection task failed: {error}"); }
            }
            accepted = listener.accept() => {
                let mut stream = accepted?;
                let permit = match Arc::clone(&permits).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => { drop(stream); continue; }
                };
                let broker = Arc::clone(&broker);
                connections.spawn(async move {
                    let _permit = permit;
                    let result: BridgeResult<ResponseBody> = async {
                        ipc::verify_peer(&stream)?;
                        let bytes = ipc::read_frame(&mut stream, MAX_FRAME_BYTES).await?;
                        let request: WireRequest = serde_json::from_slice(&bytes)
                            .map_err(|e| BridgeError::new("invalid_request", e.to_string()))?;
                        if request.api_version != API_VERSION {
                            return Err(BridgeError::new("api_version", "Supported local broker API version is 1"));
                        }
                        broker.handle(request.request).await
                    }.await;
                    let response = match result { Ok(value) => WireResponse::Ok(value), Err(error) => WireResponse::Error(error) };
                    match serde_json::to_vec(&response) {
                        Ok(bytes) => {
                            if let Err(error) = ipc::write_frame(&mut stream, &bytes, ipc::MAX_RESPONSE_BYTES).await {
                                eprintln!("IPC response could not be delivered: {error}");
                            }
                        }
                        Err(error) => eprintln!("IPC response serialization failed: {error}"),
                    }
                });
            }
        }
    }
    drop(listener);
    broker.shutdown().await;
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    eprintln!("devagotchi broker stopped");
    let _ = std::fs::remove_file(state.join(ENDPOINT_FILE));
    Ok(())
}

/// Best effort, owner-only: a stale file only ever points at an endpoint that refuses.
#[cfg(any(target_os = "linux", windows))]
fn write_endpoint_file(state: &std::path::Path, endpoint: &std::path::Path) {
    let path = state.join(ENDPOINT_FILE);
    let tmp = state.join(".endpoint.tmp");
    let text = format!("{}\n", endpoint.display());
    let written = std::fs::write(&tmp, text).and_then(|_| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &path)
    });
    if let Err(error) = written {
        eprintln!("devagotchi broker: could not write {}: {error}", path.display());
    }
}
