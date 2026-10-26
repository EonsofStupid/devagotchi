//! The one binary, driven as a person would: `devagotchi start` with nothing but an
//! environment, then `devagotchi mcp` finding that broker on its own, then the board.
//! Linux only for the broker (the macOS provider is pending; Windows has its own install
//! check); the help tree runs everywhere.

use std::process::Command;

fn devagotchi() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devagotchi"))
}

#[test]
fn help_names_every_command_and_each_answers_help() {
    let out = devagotchi().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for word in [
        "start", "mcp", "connect", "board", "broker", "hub", "link", "login", "logout", "whoami",
        "version",
    ] {
        assert!(
            text.contains(&format!("\n  {word}")),
            "{word} missing from:\n{text}"
        );
    }
    for command in [
        "start", "mcp", "connect", "board", "broker", "hub", "link", "login", "logout", "whoami",
    ] {
        let out = devagotchi().args([command, "--help"]).output().unwrap();
        assert!(
            out.status.success(),
            "{command} --help: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "{command} --help printed nothing");
    }
    let out = devagotchi().arg("version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("devagotchi {}", env!("CARGO_PKG_VERSION"))
    );
    let out = devagotchi().arg("nonsense").output().unwrap();
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn mcp_hub_without_an_account_answers_on_the_wire_and_refuses_bad_flags() {
    use std::io::{BufRead, BufReader, Write};
    let dir = tempfile::tempdir().unwrap();
    let out = devagotchi()
        .args(["mcp", "--hub", "--socket"])
        .env("DEVAGOTCHI_CONFIG_DIR", dir.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(64),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Not signed in: the front still serves MCP, answering each request with the reason.
    let mut front = devagotchi()
        .args(["mcp", "--hub", "https://hub.example"])
        .env("DEVAGOTCHI_CONFIG_DIR", dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = front.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
    )
    .unwrap();
    drop(stdin);
    let mut line = String::new();
    BufReader::new(front.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["id"], 1);
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("devagotchi login"),
        "{line}"
    );
    assert!(front.wait().unwrap().success());
}

#[test]
fn mcp_without_a_broker_says_what_to_run() {
    let dir = tempfile::tempdir().unwrap();
    let out = devagotchi()
        .arg("mcp")
        .env("DEVAGOTCHI_STATE_DIR", dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(69));
    assert!(String::from_utf8_lossy(&out.stderr).contains("devagotchi start"));
}

#[cfg(target_os = "linux")]
#[test]
fn start_then_mcp_then_board_from_one_binary() {
    use std::{
        io::{BufRead, BufReader, Write},
        path::Path,
        process::Stdio,
        time::{Duration, Instant},
    };

    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("state");
    let config_dir = home.path().join("config");
    let project = home.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let env = [
        ("DEVAGOTCHI_STATE_DIR", state.as_os_str().to_owned()),
        ("DEVAGOTCHI_CONFIG_DIR", config_dir.as_os_str().to_owned()),
        ("HOME", home.path().as_os_str().to_owned()),
    ];

    // start: writes the config, creates the state directory, runs until told to stop.
    let mut broker = devagotchi()
        .args(["start", "--workspace"])
        .arg(home.path())
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = broker.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let started = Instant::now();
    let mut lines = Vec::new();
    let endpoint_file = state.join("endpoint");
    while !endpoint_file.is_file() {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) {
            lines.push(line);
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "broker never wrote its endpoint:\n{}",
            lines.join("\n")
        );
        assert!(
            broker.try_wait().unwrap().is_none(),
            "broker exited:\n{}",
            lines.join("\n")
        );
    }
    let endpoint = std::fs::read_to_string(&endpoint_file).unwrap();
    assert!(Path::new(endpoint.trim()).is_absolute(), "{endpoint}");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_dir.join("broker.json")).unwrap())
            .unwrap();
    assert_eq!(
        config["workspace"],
        serde_json::json!(std::fs::canonicalize(home.path()).unwrap())
    );

    // mcp: no --socket, finds the broker through the endpoint file; a real MCP exchange.
    let mut adapter = devagotchi()
        .arg("mcp")
        .envs(env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = adapter.stdin.take().unwrap();
    let mut stdout = BufReader::new(adapter.stdout.take().unwrap());
    let mut rpc = |id: u64, method: &str, params: serde_json::Value| -> serde_json::Value {
        let message =
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).unwrap() > 0, "adapter closed");
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    };
    let init = rpc(
        1,
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "one-binary-test", "version": "0" } }),
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"], "desktop-bridge",
        "{init}"
    );
    let tools = rpc(2, "tools/list", serde_json::json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"operation_start"), "{names:?}");
    drop(rpc);
    drop(stdin);
    let _ = adapter.wait();

    // board: the same binary, the project's board, as an agent.
    let out = devagotchi()
        .args(["board", "init", "--project", "demo"])
        .current_dir(&project)
        .envs(env.iter().cloned())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = devagotchi()
        .args(["board", "new", "prove the one binary"])
        .current_dir(&project)
        .envs(env.iter().cloned())
        .env("DEVAGOTCHI_ACTOR", "claude")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = devagotchi()
        .args(["board", "prime", "--json"])
        .current_dir(&project)
        .envs(env.iter().cloned())
        .env("DEVAGOTCHI_ACTOR", "claude")
        .output()
        .unwrap();
    let prime: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(prime["ready"].as_array().unwrap().len(), 1, "{prime}");

    // The account, from the same config directory: nothing on file, the local choice, forgotten.
    let whoami = |expect: &str| {
        let out = devagotchi()
            .arg("whoami")
            .envs(env.iter().cloned())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains(expect), "{text}");
    };
    whoami("not signed in");
    let out = devagotchi()
        .args(["login", "--local"])
        .envs(env.iter().cloned())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(config_dir.join("account.json").is_file());
    whoami("runs locally");
    let out = devagotchi()
        .arg("logout")
        .envs(env.iter().cloned())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!config_dir.join("account.json").exists());
    whoami("not signed in");

    // Stop the broker the way a terminal does; it settles and removes the endpoint file.
    Command::new("kill")
        .args(["-INT", &broker.id().to_string()])
        .status()
        .unwrap();
    let status = broker.wait().unwrap();
    assert!(status.success(), "broker exit {status}");
    assert!(!endpoint_file.exists(), "endpoint file left behind");
}

/// `devagotchi connect` against a real broker: every client's config is written, Claude
/// Code through its own `claude mcp add` (a stand-in records the call; no real client is
/// touched), and the server is started once and lists its tools.
#[cfg(target_os = "linux")]
#[test]
fn connect_wires_every_client_and_checks_the_server_answers() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::fs::PermissionsExt,
        process::Stdio,
        time::{Duration, Instant},
    };

    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("state");
    let codex = home.path().join("codex");
    let xdg = home.path().join("xdg");
    let bin = home.path().join("bin");
    let calls = home.path().join("claude-calls");
    for dir in [&codex, &xdg.join("Claude"), &bin] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        bin.join("claude"),
        format!("#!/bin/sh\necho \"$*\" >> {}\n", calls.display()),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(codex.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
    let env = [
        ("DEVAGOTCHI_STATE_DIR", state.as_os_str().to_owned()),
        ("DEVAGOTCHI_CONFIG_DIR", home.path().join("config").into_os_string()),
        ("HOME", home.path().as_os_str().to_owned()),
        ("CODEX_HOME", codex.as_os_str().to_owned()),
        ("XDG_CONFIG_HOME", xdg.as_os_str().to_owned()),
        ("PATH", bin.as_os_str().to_owned()),
    ];

    // Not signed in and no target named: says what to run, changes nothing.
    let out = devagotchi().arg("connect").envs(env.iter().cloned()).output().unwrap();
    assert_eq!(out.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&out.stderr).contains("devagotchi login"));
    assert_eq!(std::fs::read_to_string(codex.join("config.toml")).unwrap(), "model = \"gpt-5\"\n");

    let mut broker = devagotchi()
        .args(["start", "--workspace"])
        .arg(home.path())
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = broker.stderr.take().unwrap();
    std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
    let started = Instant::now();
    while !state.join("endpoint").is_file() {
        assert!(started.elapsed() < Duration::from_secs(30), "broker never wrote its endpoint");
        std::thread::sleep(Duration::from_millis(50));
    }

    for round in 0..2 {
        let out = devagotchi()
            .args(["connect", "--local"])
            .envs(env.iter().cloned())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("answered with 4 tools"), "{text}");
        assert!(text.contains("Claude Code: added") && text.contains("Codex: ") && text.contains("Claude Desktop: "), "{text}");
        let config = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert!(config.starts_with("model = \"gpt-5\"\n"), "{config}");
        assert_eq!(config.matches("[mcp_servers.devagotchi]").count(), 1, "round {round}: {config}");
        assert!(config.contains("args = ['mcp']"), "{config}");
    }
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_devagotchi")).unwrap();
    let desktop: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(xdg.join("Claude").join("claude_desktop_config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        desktop["mcpServers"]["devagotchi"],
        serde_json::json!({"command": exe, "args": ["mcp"]})
    );
    let recorded = std::fs::read_to_string(&calls).unwrap();
    assert!(recorded.contains("mcp remove --scope user devagotchi"), "{recorded}");
    assert!(
        recorded.contains(&format!("mcp add --scope user devagotchi -- {} mcp", exe.display())),
        "{recorded}"
    );
    assert!(std::fs::read_dir(&codex).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("config.toml.bak-")));

    let _ = broker.kill();
    let _ = broker.wait();
}
