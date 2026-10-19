//! The broker runs independently from the MCP client process.

#[cfg(any(target_os = "linux", windows))]
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bridge-broker: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
    eprintln!("bridge-broker: unsupported_platform: native execution providers exist for Linux and Windows; the macOS provider is pending");
    std::process::exit(1);
}

/// Operating-system requests to stop, besides Ctrl+C.
#[cfg(unix)]
struct Termination(tokio::signal::unix::Signal);

#[cfg(unix)]
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
        Ok(Self { ctrl_break: ctrl_break()?, close: ctrl_close()?, shutdown: ctrl_shutdown()? })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.ctrl_break.recv() => {}
            _ = self.close.recv() => {}
            _ = self.shutdown.recv() => {}
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use desktop_bridge::{contracts::*, execution::{Broker, BrokerConfig}, ipc::{self, StateGuard}};
    use std::{io::Read, path::PathBuf, sync::Arc};

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!("Usage: bridge-broker --state-dir ABS_PATH --config ABS_JSON\n\nStarts a private per-user local broker. The state parent directory must exist.\nExecutable grants authorize process execution as your OS user, not a sandbox.\nThe ready line names the endpoint clients pass as --socket: a Unix socket path on Linux,\nor a \\\\.\\pipe\\ named-pipe path on Windows. The macOS provider is pending.");
        return Ok(());
    }
    let mut state = None;
    let mut config = None;
    if args.len() != 4 { return Err("Expected --state-dir ABS_PATH --config ABS_JSON; use --help".into()); }
    for pair in args.chunks_exact(2) {
        let value = PathBuf::from(&pair[1]);
        if !value.is_absolute() { return Err("All CLI paths must be absolute".into()); }
        match pair[0].as_str() {
            "--state-dir" if state.is_none() => state = Some(value),
            "--config" if config.is_none() => config = Some(value),
            _ => return Err("Unknown or repeated CLI option".into()),
        }
    }
    let state = state.ok_or("Missing --state-dir")?;
    let config_path = config.ok_or("Missing --config")?;
    let metadata = std::fs::metadata(&config_path)?;
    if !metadata.is_file() || metadata.len() > MAX_FRAME_BYTES as u64 { return Err("Config must be a regular file no larger than 64 KiB".into()); }
    // Read with a cap even if the file grows after metadata inspection.
    let mut file = std::fs::File::open(config_path)?;
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, (MAX_FRAME_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FRAME_BYTES { return Err("Config exceeds 64 KiB".into()); }
    let config: BrokerConfig = serde_json::from_slice(&bytes)?;
    let mut guard = StateGuard::acquire(&state)?;
    // Bind before opening the journal, so a foreign endpoint prevents reconciliation.
    let mut listener = guard.bind().await?;
    let broker = Broker::open(config, &state)?;
    eprintln!("bridge-broker ready: {}", guard.endpoint().display());

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
    eprintln!("bridge-broker stopped");
    Ok(())
}
