//! `devagotchi connect`: point this machine's AI clients at Devagotchi in one command.
//! Claude Code, Codex and Claude Desktop each get a `devagotchi` MCP server that runs
//! this binary (`mcp --hub`: the hosted hub as the account `devagotchi login` signed in
//! with; `mcp`: this machine's own broker). Every file is backed up beside itself and
//! replaced whole; then the server is started once and asked for its tools, so the
//! answer says whether it works rather than that a file was written.

use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const USAGE: &str = "Usage: devagotchi connect [claude-code|codex|claude-desktop|all] [--hub [URL] | --local] [--no-check]

Points your AI clients at Devagotchi. Each gets an MCP server named `devagotchi` that runs
this binary:
  --hub [URL]  (the default once `devagotchi login` has signed you in) the hosted hub, as your
               account: every machine you linked, the boards and approvals
  --local      this machine's own broker (`devagotchi start` must be running)
Clients (default: every one found here):
  claude-code     `claude mcp add --scope user devagotchi -- devagotchi mcp …`
  codex           [mcp_servers.devagotchi] in ~/.codex/config.toml (CODEX_HOME honoured)
  claude-desktop  mcpServers.devagotchi in claude_desktop_config.json
Each file is backed up beside itself (.bak-<time>) and written whole. Then the server is
started once and asked for its tools (--no-check skips that). Restart a client that is
open to pick the server up.";

/// How long Devagotchi may hold one tool call (a wait is at most 50 s, then the answer).
const TOOL_TIMEOUT_SECS: u64 = 120;
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// The line written above Devagotchi's table in Codex's config, replaced with it.
const CODEX_MARK: &str =
    "# Devagotchi: your machines, boards and approvals (written by `devagotchi connect`).";

/// What `devagotchi connect` needs from the machine, looked up by the caller so tests
/// can supply their own.
pub struct Machine {
    /// This binary, absolute.
    pub exe: PathBuf,
    pub home: Option<PathBuf>,
    /// `CODEX_HOME`, when set.
    pub codex_home: Option<PathBuf>,
    /// Where Claude Desktop keeps its config directory's parent (Windows `%APPDATA%`,
    /// macOS `~/Library/Application Support`, Linux `$XDG_CONFIG_HOME` or `~/.config`).
    pub app_config: Option<PathBuf>,
    /// `PATH`, to find the `claude` and `codex` commands.
    pub path: Option<OsString>,
    /// Whether `devagotchi login` has signed this machine in.
    pub signed_in: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Client {
    ClaudeCode,
    Codex,
    ClaudeDesktop,
}

impl Client {
    const ALL: [Client; 3] = [Client::ClaudeCode, Client::Codex, Client::ClaudeDesktop];
    fn name(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::Codex => "Codex",
            Client::ClaudeDesktop => "Claude Desktop",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Target {
    Hub(Option<String>),
    Local,
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    clients: Option<Vec<Client>>,
    target: Option<Target>,
    check: bool,
}

fn parse(args: &[OsString]) -> Result<Option<Options>, String> {
    let mut options = Options { clients: None, target: None, check: true };
    let mut args = args.iter().map(|a| a.to_string_lossy().into_owned()).peekable();
    while let Some(arg) = args.next() {
        let one = |client: Client, options: &mut Options| {
            options.clients.get_or_insert_with(Vec::new).push(client);
        };
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "claude-code" => one(Client::ClaudeCode, &mut options),
            "codex" => one(Client::Codex, &mut options),
            "claude-desktop" => one(Client::ClaudeDesktop, &mut options),
            "all" => options.clients = Some(Client::ALL.to_vec()),
            "--local" if options.target.is_none() => options.target = Some(Target::Local),
            "--hub" if options.target.is_none() => {
                let url = args.next_if(|next| !next.starts_with('-') && next.contains("://"));
                if let Some(url) = &url
                    && !url.starts_with("https://")
                {
                    return Err(format!("--hub takes an https:// URL, got {url}"));
                }
                options.target = Some(Target::Hub(url));
            }
            "--no-check" => options.check = false,
            "--local" | "--hub" => return Err("give either --hub or --local, once".into()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Some(options))
}

/// `devagotchi connect …`
pub fn cli(args: Vec<OsString>, machine: Machine) -> ExitCode {
    let options = match parse(&args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(why) => {
            eprintln!("devagotchi connect: {why}\n\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    let target = match options.target {
        Some(target) => target,
        None if machine.signed_in => Target::Hub(None),
        None => {
            eprintln!(
                "devagotchi connect: this machine is not signed in. Run `devagotchi login` first (then connect uses the hosted hub), or `devagotchi connect --local` for this machine's broker alone."
            );
            return ExitCode::from(64);
        }
    };
    if matches!(target, Target::Hub(_)) && !machine.signed_in {
        eprintln!("devagotchi connect: --hub needs an account: run `devagotchi login` first");
        return ExitCode::from(64);
    }
    let args = server_args(&target);
    let explicit = options.clients.is_some();
    let clients: Vec<Client> = match options.clients {
        Some(clients) => clients,
        None => Client::ALL.into_iter().filter(|c| present(*c, &machine)).collect(),
    };
    if clients.is_empty() {
        eprintln!(
            "devagotchi connect: found no Claude Code (`claude`), Codex (~/.codex or `codex`) or Claude Desktop here; name one to set it up anyway"
        );
        return ExitCode::from(1);
    }
    let shown = format!("{} {}", machine.exe.display(), args.join(" "));
    println!("Devagotchi MCP server: {shown}");
    let mut failed = false;
    for client in clients {
        let done = match client {
            Client::ClaudeCode => claude_code(&machine, &args),
            Client::Codex => codex(&machine, &args),
            Client::ClaudeDesktop => claude_desktop(&machine, &args),
        };
        match done {
            Ok(what) => println!("  {}: {what}", client.name()),
            Err(why) => {
                // A client that was only looked for is reported, not counted as a failure.
                failed |= explicit;
                println!("  {}: not set up: {why}", client.name());
            }
        }
    }
    if options.check {
        match check(&machine.exe, &args) {
            Ok(tools) => println!(
                "Check: Devagotchi answered with {} tools ({}). Restart any client that is open.",
                tools.len(),
                tools.join(", ")
            ),
            Err(why) => {
                failed = true;
                println!("Check: Devagotchi did not answer: {why}");
            }
        }
    }
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn server_args(target: &Target) -> Vec<String> {
    match target {
        Target::Local => vec!["mcp".into()],
        Target::Hub(None) => vec!["mcp".into(), "--hub".into()],
        Target::Hub(Some(url)) => vec!["mcp".into(), "--hub".into(), url.clone()],
    }
}

fn present(client: Client, machine: &Machine) -> bool {
    match client {
        Client::ClaudeCode => find_command("claude", machine.path.as_deref()).is_some(),
        Client::Codex => {
            codex_dir(machine).is_some_and(|dir| dir.is_dir())
                || find_command("codex", machine.path.as_deref()).is_some()
        }
        Client::ClaudeDesktop => desktop_file(machine).and_then(|f| f.parent().map(Path::is_dir)).unwrap_or(false),
    }
}

/// A command on PATH, as the shell would find it (Windows: with its PATHEXT extension).
fn find_command(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let extensions: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(path?).find_map(|dir| {
        extensions
            .iter()
            .map(|ext| dir.join(format!("{name}{ext}")))
            .find(|candidate| candidate.is_file())
    })
}

fn codex_dir(machine: &Machine) -> Option<PathBuf> {
    machine.codex_home.clone().or_else(|| machine.home.as_ref().map(|h| h.join(".codex")))
}

fn desktop_file(machine: &Machine) -> Option<PathBuf> {
    machine.app_config.as_ref().map(|d| d.join("Claude").join("claude_desktop_config.json"))
}

fn claude_code(machine: &Machine, args: &[String]) -> Result<String, String> {
    let claude = find_command("claude", machine.path.as_deref())
        .ok_or("the `claude` command is not on PATH")?;
    let run = |extra: &[&str]| -> Result<std::process::Output, String> {
        let mut command = if claude.extension().is_some_and(|e| e == "cmd" || e == "bat") {
            let mut c = Command::new("cmd");
            c.arg("/d").arg("/c").arg(&claude);
            c
        } else {
            Command::new(&claude)
        };
        command.args(extra).stdin(Stdio::null());
        command.output().map_err(|e| format!("could not run {}: {e}", claude.display()))
    };
    // Replace rather than add twice; a missing entry is not an error.
    let _ = run(&["mcp", "remove", "--scope", "user", "devagotchi"]);
    let exe = machine.exe.to_string_lossy().into_owned();
    let mut add = vec!["mcp", "add", "--scope", "user", "devagotchi", "--", exe.as_str()];
    add.extend(args.iter().map(String::as_str));
    let out = run(&add)?;
    if !out.status.success() {
        return Err(format!(
            "`claude mcp add` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok("added to your user settings (`claude mcp list` shows it)".into())
}

fn codex(machine: &Machine, args: &[String]) -> Result<String, String> {
    let dir = codex_dir(machine).ok_or("no home directory to find ~/.codex in")?;
    let file = dir.join("config.toml");
    let old = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let new = codex_config(old.as_deref().unwrap_or(""), &machine.exe, args);
    let backup = replace(&file, &new, old.is_some())?;
    Ok(format!("{} ([mcp_servers.devagotchi]{})", file.display(), backed_up(backup)))
}

fn claude_desktop(machine: &Machine, args: &[String]) -> Result<String, String> {
    let file = desktop_file(machine).ok_or("no application config directory")?;
    let old = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let new = desktop_config(old.as_deref(), &machine.exe, args)
        .map_err(|why| format!("{}: {why}; not changed", file.display()))?;
    let backup = replace(&file, &new, old.is_some())?;
    Ok(format!("{} (mcpServers.devagotchi{})", file.display(), backed_up(backup)))
}

fn backed_up(backup: Option<PathBuf>) -> String {
    backup.map(|b| format!("; backup {}", b.display())).unwrap_or_default()
}

/// Codex's config with Devagotchi's server table replaced (or added); every other line kept.
fn codex_config(existing: &str, exe: &Path, args: &[String]) -> String {
    let ours = |header: &str| {
        let name = header.trim_start_matches('[').trim_end_matches(']').trim();
        let name = name.split('#').next().unwrap_or("").trim();
        ["mcp_servers.devagotchi", "mcp_servers.\"devagotchi\""]
            .iter()
            .any(|table| name == *table || name.starts_with(&format!("{table}.")))
    };
    let mut kept = Vec::new();
    let mut skipping = false;
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            skipping = ours(trimmed);
        }
        if !skipping && line != CODEX_MARK {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    let args = args.iter().map(|a| toml_string(a)).collect::<Vec<_>>().join(", ");
    out.push_str(&format!(
        "{CODEX_MARK}\n[mcp_servers.devagotchi]\ncommand = {}\nargs = [{args}]\n# Devagotchi holds a wait for up to 50 s before it answers.\ntool_timeout_sec = {TOOL_TIMEOUT_SECS}\n",
        toml_string(&exe.to_string_lossy())
    ));
    out
}

/// A TOML string: literal (no escapes, so Windows paths read as typed) unless it holds a `'`.
fn toml_string(value: &str) -> String {
    if !value.contains('\'') && !value.contains('\n') {
        return format!("'{value}'");
    }
    serde_json::to_string(value).expect("a string serializes")
}

/// Claude Desktop's config with `mcpServers.devagotchi` set; everything else kept.
fn desktop_config(existing: Option<&str>, exe: &Path, args: &[String]) -> Result<String, String> {
    let mut config: serde_json::Value = match existing {
        Some(text) if !text.trim().is_empty() => {
            serde_json::from_str(text).map_err(|e| format!("not JSON ({e})"))?
        }
        _ => serde_json::json!({}),
    };
    let root = config.as_object_mut().ok_or("not a JSON object")?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("mcpServers is not an object")?;
    servers.insert(
        "devagotchi".into(),
        serde_json::json!({"command": exe.to_string_lossy(), "args": args}),
    );
    Ok(format!("{}\n", serde_json::to_string_pretty(&config).expect("JSON serializes")))
}

/// Write `text` to `file` whole (beside it, then renamed over it), backing up what was
/// there; returns the backup's path.
fn replace(file: &Path, text: &str, existed: bool) -> Result<Option<PathBuf>, String> {
    let dir = file.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let backup = if existed {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let backup = file.with_extension(format!(
            "{}.bak-{stamp}",
            file.extension().and_then(|e| e.to_str()).unwrap_or("")
        ));
        std::fs::copy(file, &backup).map_err(|e| format!("backup {}: {e}", backup.display()))?;
        Some(backup)
    } else {
        None
    };
    let temp = file.with_extension("devagotchi-new");
    std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
    #[cfg(unix)]
    if existed {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(file) {
            let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(meta.permissions().mode()));
        }
    }
    std::fs::rename(&temp, file).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(backup)
}

/// Start the server as a client would and ask for its tools.
fn check(exe: &Path, args: &[String]) -> Result<Vec<String>, String> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut send = |message: serde_json::Value| -> Result<(), String> {
        writeln!(stdin, "{message}").map_err(|e| format!("could not talk to the server: {e}"))
    };
    let result = (|| {
        send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "devagotchi-connect", "version": env!("CARGO_PKG_VERSION")}}}))?;
        answer(&rx, 1)?;
        send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))?;
        let list = answer(&rx, 2)?;
        let tools: Vec<String> = list["result"]["tools"]
            .as_array()
            .map(|tools| tools.iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if tools.is_empty() {
            return Err("it listed no tools".to_string());
        }
        Ok(tools)
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn answer(rx: &mpsc::Receiver<String>, id: i64) -> Result<serde_json::Value, String> {
    let until = std::time::Instant::now() + CHECK_TIMEOUT;
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        let line = rx
            .recv_timeout(left)
            .map_err(|_| format!("no answer within {} s", CHECK_TIMEOUT.as_secs()))?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if message["id"].as_i64() != Some(id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(error["message"].as_str().unwrap_or("an error").to_string());
        }
        return Ok(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> Vec<String> {
        server_args(&Target::Hub(None))
    }

    #[test]
    fn arguments_pick_clients_and_one_target() {
        let parse = |args: &[&str]| parse(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(
            parse(&["codex", "--hub", "https://hub.example"]).unwrap().unwrap(),
            Options {
                clients: Some(vec![Client::Codex]),
                target: Some(Target::Hub(Some("https://hub.example".into()))),
                check: true
            }
        );
        let local = parse(&["--local", "--no-check"]).unwrap().unwrap();
        assert_eq!((local.clients, local.target, local.check), (None, Some(Target::Local), false));
        assert!(parse(&["--hub", "--local"]).is_err());
        assert!(parse(&["--hub", "http://plain.example"]).is_err());
        assert!(parse(&["cursor"]).is_err());
        assert_eq!(parse(&["--help"]).unwrap(), None);
    }

    #[test]
    fn codex_keeps_every_other_line_and_replaces_only_its_own_table() {
        let existing = "model = \"gpt-5\"\n\n[mcp_servers.github]\ncommand = \"gh-mcp\"\n\n[mcp_servers.devagotchi]\ncommand = \"/old/devagotchi\"\nargs = [\"mcp\"]\n\n[mcp_servers.devagotchi.env]\nX = \"1\"\n\n[profiles.fast]\nmodel = \"o4-mini\"\n";
        let out = codex_config(existing, Path::new("/usr/local/bin/devagotchi"), &hub());
        assert!(out.starts_with("model = \"gpt-5\"\n\n[mcp_servers.github]\ncommand = \"gh-mcp\"\n"), "{out}");
        assert!(out.contains("[profiles.fast]\nmodel = \"o4-mini\""), "{out}");
        assert!(!out.contains("/old/devagotchi") && !out.contains("X = \"1\""), "{out}");
        assert_eq!(out.matches("[mcp_servers.devagotchi]").count(), 1, "{out}");
        assert!(out.ends_with("[mcp_servers.devagotchi]\ncommand = '/usr/local/bin/devagotchi'\nargs = ['mcp', '--hub']\n# Devagotchi holds a wait for up to 50 s before it answers.\ntool_timeout_sec = 120\n"), "{out}");
        // Running it again changes nothing.
        assert_eq!(codex_config(&out, Path::new("/usr/local/bin/devagotchi"), &hub()), out);
        // A new file holds only the server.
        assert!(codex_config("", Path::new("/d"), &hub()).starts_with("# Devagotchi"));
    }

    #[test]
    fn windows_paths_stay_as_typed_and_quotes_are_escaped() {
        assert_eq!(toml_string(r"C:\Users\jesse\AppData\Local\devagotchi\devagotchi.exe"), r"'C:\Users\jesse\AppData\Local\devagotchi\devagotchi.exe'");
        assert_eq!(toml_string("it's"), "\"it's\"");
    }

    #[test]
    fn claude_desktop_keeps_other_servers_and_refuses_a_broken_file() {
        let existing = r#"{"globalShortcut": "Ctrl+Space", "mcpServers": {"fs": {"command": "npx", "args": ["fs"]}}}"#;
        let out = desktop_config(Some(existing), Path::new("/bin/devagotchi"), &hub()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["globalShortcut"], "Ctrl+Space");
        assert_eq!(value["mcpServers"]["fs"]["command"], "npx");
        assert_eq!(value["mcpServers"]["devagotchi"], serde_json::json!({"command": "/bin/devagotchi", "args": ["mcp", "--hub"]}));
        assert!(desktop_config(Some("{not json"), Path::new("/d"), &hub()).is_err());
        assert!(desktop_config(None, Path::new("/d"), &hub()).unwrap().contains("devagotchi"));
    }

    #[test]
    fn replacing_a_file_backs_up_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("config.toml");
        assert_eq!(replace(&file, "one\n", false).unwrap(), None);
        let backup = replace(&file, "two\n", true).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "one\n");
        assert!(backup.file_name().unwrap().to_string_lossy().starts_with("config.toml.bak-"));
    }
}
