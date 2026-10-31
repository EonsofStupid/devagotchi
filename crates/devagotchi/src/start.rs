//! `devagotchi start`: this user's broker, with no paths to remember. The first run writes
//! `broker.json` (the workspace and the toolchains found on PATH), every run creates the
//! state directory and runs the broker in the foreground until Ctrl+C. `devagotchi mcp`
//! finds the broker through `<state>/endpoint`.

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
};

use crate::paths;

pub const USAGE: &str =
    "Usage: devagotchi start [--background] [--workspace DIR] [--grant PATH]... [--state-dir DIR] [--config FILE]

Runs your broker in the foreground (Ctrl+C stops it; jobs are settled first). With --background
it starts the broker detached, writing to <state>/broker.log, and returns once it answers (at
once when one already does); `devagotchi mcp` does this on its own when no broker answers. The first run
writes the broker's config: it runs what your account can (shell lines, any program, files
anywhere you can reach), starting in the workspace (default: your home directory); the
toolchains found on PATH (git, rg, python3/python, node, npm, npx, cargo, rustup, go, uv) and
any --grant are listed too, for a config set back to `exec_policy` `granted`, which runs
only those, inside the workspace. Later runs keep the file. Then point your agent at
`devagotchi mcp`. A grant must be what the broker runs directly: an executable file (Windows:
an .exe or .com image; npm and npx are .cmd scripts there, so they stay out).

Defaults: state in ~/.local/state/devagotchi (Windows: %LOCALAPPDATA%\\devagotchi\\state),
config in ~/.config/devagotchi/broker.json (Windows: %APPDATA%\\devagotchi); DEVAGOTCHI_STATE_DIR
and DEVAGOTCHI_CONFIG_DIR override them, and DEVAGOTCHI_BROKER_CONFIG names the config file
alone (your sign-in stays in the config directory).";

/// Executables worth granting when they are on PATH, in the order they are listed
/// (`python` is what Windows installs; a Store alias for both resolves to one grant).
#[cfg(any(target_os = "linux", windows))]
const TOOLCHAINS: &[&str] = &[
    "git", "rg", "python3", "python", "node", "npm", "npx", "cargo", "rustup", "go", "uv",
];

struct Options {
    state: Option<PathBuf>,
    config: Option<PathBuf>,
    workspace: Option<PathBuf>,
    grants: Vec<PathBuf>,
    background: bool,
}

fn parse(args: Vec<OsString>) -> Result<Options, String> {
    let mut options = Options {
        state: None,
        config: None,
        workspace: None,
        grants: Vec::new(),
        background: false,
    };
    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        let flag = flag.to_string_lossy().into_owned();
        let mut value = || {
            iter.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--background" => options.background = true,
            "--state-dir" => options.state = Some(value()?),
            "--config" => options.config = Some(value()?),
            "--workspace" => options.workspace = Some(value()?),
            "--grant" => options.grants.push(value()?),
            other => {
                return Err(format!(
                    "unknown option {other}; see devagotchi start --help"
                ));
            }
        }
    }
    Ok(options)
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

pub fn run(args: Vec<OsString>) -> ExitCode {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("devagotchi start: {message}");
            return ExitCode::from(64);
        }
    };
    let Some(state) = options.state.map(absolute).or_else(paths::state_dir) else {
        eprintln!(
            "devagotchi start: no home directory in the environment; pass --state-dir and --config, or set DEVAGOTCHI_STATE_DIR and DEVAGOTCHI_CONFIG_DIR"
        );
        return ExitCode::from(64);
    };
    let config = match options.config.map(absolute).or_else(paths::broker_config) {
        Some(config) => config,
        None => {
            eprintln!(
                "devagotchi start: no home directory in the environment; pass --config or set DEVAGOTCHI_CONFIG_DIR"
            );
            return ExitCode::from(64);
        }
    };
    if options.background {
        return background(&state, &config, options.workspace, &options.grants);
    }
    // Where no native execution provider exists there is nothing to prepare: the broker
    // answers `unsupported_platform` itself, before any config is written.
    #[cfg(any(target_os = "linux", windows))]
    {
        // The broker makes the state directory itself (owner-only, through the IPC layer
        // that checks it); only its parent is made here. On Windows a directory made here
        // by an administrator would be owned by the Administrators group and refused.
        if let Some(parent) = state.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            eprintln!(
                "devagotchi start: cannot create {}: {error}",
                parent.display()
            );
            return ExitCode::FAILURE;
        }
        if let Some(parent) = config.parent()
            && let Err(error) = private_dir(parent)
        {
            eprintln!(
                "devagotchi start: cannot create {}: {error}",
                parent.display()
            );
            return ExitCode::FAILURE;
        }
        if config.is_file() {
            eprintln!(
                "devagotchi: config {} (kept; edit it, or delete it to write it again)",
                config.display()
            );
            if options.workspace.is_some() || !options.grants.is_empty() {
                eprintln!(
                    "devagotchi: --workspace and --grant only shape a new config; this one was kept"
                );
            }
        } else {
            match write_config(&config, options.workspace, &options.grants) {
                Ok(summary) => eprintln!("devagotchi: wrote {}: {summary}", config.display()),
                Err(message) => {
                    eprintln!("devagotchi start: {message}");
                    return ExitCode::FAILURE;
                }
            }
        }
        eprintln!("devagotchi: state {}", state.display());
        eprintln!(
            "devagotchi: your agent's MCP server command is `devagotchi mcp` (it finds this broker on its own)"
        );
        // D22: the default signs in; the choice not to is recorded, and until one of the
        // two is on file every run says so. Nothing local waits on it.
        // The sign-in lives in the config directory, wherever the broker's config is.
        if let Some(dir) = paths::config_dir().or_else(|| config.parent().map(Path::to_path_buf)) {
            account_line(&dir.join(devagotchi_hub::account::FILE_NAME));
        }
    }
    match devagotchi_broker::serve_blocking(state, config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("devagotchi start: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `devagotchi start --background`: the same start, detached, with every path explicit so
/// the broker it starts is this one whatever its environment; returns once it answers.
fn background(
    state: &Path,
    config: &Path,
    workspace: Option<PathBuf>,
    grants: &[PathBuf],
) -> ExitCode {
    use devagotchi_broker::background;
    if let Some(endpoint) = background::answering(state) {
        eprintln!(
            "devagotchi: your broker is already running: {}",
            endpoint.display()
        );
        return ExitCode::SUCCESS;
    }
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("devagotchi start: cannot find this binary: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut args: Vec<OsString> = vec![
        "start".into(),
        "--state-dir".into(),
        state.into(),
        "--config".into(),
        config.into(),
    ];
    if let Some(workspace) = workspace {
        args.extend(["--workspace".into(), absolute(workspace).into()]);
    }
    for grant in grants {
        args.extend(["--grant".into(), absolute(grant.clone()).into()]);
    }
    let log = state.join(background::LOG_FILE);
    match background::start_and_wait(&exe, &args, state, background::START_WITHIN) {
        Ok(endpoint) => {
            eprintln!(
                "devagotchi: your broker is running in the background: {} (log {})",
                endpoint.display(),
                log.display()
            );
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("devagotchi start: {failure}\n(log {})", log.display());
            ExitCode::FAILURE
        }
    }
}

/// One line about the account: who this machine is signed in as, that it runs without
/// one, or — neither on file — the two commands that settle it.
#[cfg(any(target_os = "linux", windows))]
fn account_line(path: &Path) {
    use devagotchi_hub::account::{Stored, read};
    match read(path) {
        Ok(Some(Stored::Account(account))) => eprintln!(
            "devagotchi: signed in as {} (account {})",
            account.display_name(),
            account.subject
        ),
        Ok(Some(Stored::Local { .. })) => {}
        Ok(None) => eprintln!(
            "devagotchi: not signed in — `devagotchi login` syncs your boards to devagotchi.com with a free account; `devagotchi login --local` keeps everything on this machine"
        ),
        Err(why) => eprintln!("devagotchi: {why}"),
    }
}

#[cfg(any(target_os = "linux", windows))]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The first config: the workspace, the toolchains on PATH and the explicit grants, all
/// as canonical paths in the broker's own form (so a grant must exist now, and must be
/// what the broker runs directly — its rule, `devagotchi_core::broker::is_directly_executable`).
#[cfg(any(target_os = "linux", windows))]
fn write_config(
    path: &Path,
    workspace: Option<PathBuf>,
    grants: &[PathBuf],
) -> Result<String, String> {
    use devagotchi_core::broker::{EXECUTABLE_RULE, is_directly_executable};

    let workspace = match workspace.map(absolute).or_else(paths::home) {
        Some(workspace) => workspace,
        None => return Err("no workspace: pass --workspace DIR".into()),
    };
    let workspace = dunce::canonicalize(&workspace)
        .map_err(|e| format!("workspace {}: {e}", workspace.display()))?;
    if !workspace.is_dir() {
        return Err(format!(
            "workspace {} is not a directory",
            workspace.display()
        ));
    }
    let mut executables: Vec<PathBuf> = Vec::new();
    let mut push = |candidate: PathBuf| {
        if !executables.contains(&candidate) {
            executables.push(candidate);
        }
    };
    for name in TOOLCHAINS {
        if let Some(found) = find_on_path(name) {
            push(found);
        }
    }
    for grant in grants {
        let grant = absolute(grant.clone());
        let real =
            dunce::canonicalize(&grant).map_err(|e| format!("grant {}: {e}", grant.display()))?;
        if !is_directly_executable(&real) {
            return Err(format!(
                "grant {} is not an executable regular file{EXECUTABLE_RULE}",
                real.display()
            ));
        }
        push(real);
    }
    // A person's own broker acts as them, as Desktop Commander does (trust by default,
    // ruled 30 Oct 2026); the grants stay listed for a config set back to `granted`.
    let config = serde_json::json!({
        "workspace": workspace,
        "exec_policy": "account",
        "allowed_executables": executables,
        "output_limit_bytes": 262_144,
        "max_operations": 64,
        "max_concurrent": 4,
    });
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())? + "\n";
    // A name of this process's own: two first starts racing never share a temporary file.
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    let listed = executables
        .iter()
        .filter_map(|e| e.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "workspace {}, runs as your account; {}",
        workspace.display(),
        if listed.is_empty() {
            "no usual toolchain on PATH to list for exec_policy granted".to_string()
        } else {
            format!("listed for exec_policy granted: {listed}")
        }
    ))
}

/// The first `name` on PATH that the broker would run directly, canonical, or none. On
/// Windows that is `name.exe` or `name.com`: a `.cmd` or `.bat` on PATH is not a grant the
/// broker accepts, so it is not one `start` writes.
#[cfg(any(target_os = "linux", windows))]
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    find_in(name, &env::var_os("PATH")?)
}

#[cfg(any(target_os = "linux", windows))]
fn find_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    use devagotchi_core::broker::is_directly_executable;

    let candidates: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.com")]
    } else {
        vec![name.to_string()]
    };
    for dir in env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for candidate in &candidates {
            let full = dir.join(candidate);
            if is_directly_executable(&full)
                && let Ok(real) = dunce::canonicalize(&full)
                && is_directly_executable(&real)
            {
                return Some(real);
            }
        }
    }
    None
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;
    use devagotchi_core::broker::is_directly_executable;

    #[test]
    fn options_parse_and_reject_the_unknown() {
        let options = parse(
            [
                "--workspace",
                "w",
                "--grant",
                "/a",
                "--grant",
                "/b",
                "--state-dir",
                "/s",
            ]
            .map(OsString::from)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(options.workspace.as_deref(), Some(Path::new("w")));
        assert_eq!(options.grants.len(), 2);
        assert_eq!(options.state.as_deref(), Some(Path::new("/s")));
        assert!(options.config.is_none());
        assert!(parse(["--grant"].map(OsString::from).to_vec()).is_err());
        assert!(parse(["--bogus", "x"].map(OsString::from).to_vec()).is_err());
    }

    /// A file the broker runs directly on this platform: an execute bit, or the `.exe`
    /// name Windows judges by.
    fn fake_tool(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        });
        std::fs::write(&path, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn a_first_config_names_the_workspace_and_only_real_executables() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("broker.json");
        let summary = write_config(&config, Some(dir.path().to_path_buf()), &[]).unwrap();
        assert!(summary.contains("workspace"), "{summary}");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(parsed["max_concurrent"], 4);
        // A person's own broker runs what their account can, as Desktop Commander does
        // (trust by default, ruled 30 Oct 2026); an operator's install chooses its own.
        assert_eq!(parsed["exec_policy"], "account");
        assert!(parsed["allowed_executables"].is_array());
        for grant in parsed["allowed_executables"].as_array().unwrap() {
            assert!(
                is_directly_executable(Path::new(grant.as_str().unwrap())),
                "{grant}"
            );
        }
        // The guarantee: what `start` writes is what the broker opens.
        let broker_config: devagotchi_core::broker::BrokerConfig =
            serde_json::from_value(parsed).unwrap();
        let state = dir.path().join("state");
        private_dir(&state).unwrap();
        devagotchi_core::broker::Broker::open(broker_config, &state)
            .expect("the broker accepts the config start wrote");

        let not_executable = dir.path().join("notes.txt");
        std::fs::write(&not_executable, "x").unwrap();
        let refused = write_config(
            &dir.path().join("other.json"),
            Some(dir.path().to_path_buf()),
            std::slice::from_ref(&not_executable),
        )
        .expect_err("a plain file is refused as a grant");
        assert!(refused.contains("notes.txt"), "{refused}");
        let tool = fake_tool(dir.path(), "tool");
        let summary = write_config(
            &dir.path().join("granted.json"),
            Some(dir.path().to_path_buf()),
            std::slice::from_ref(&tool),
        )
        .unwrap();
        assert!(summary.contains("tool"), "{summary}");
    }

    #[test]
    fn path_lookup_finds_only_what_the_broker_runs_directly() {
        let dir = tempfile::tempdir().unwrap();
        let tool = fake_tool(dir.path(), "tool");
        // A script with the same name: never a grant, whatever PATHEXT would say.
        std::fs::write(dir.path().join("script.cmd"), "@echo off").unwrap();
        std::fs::write(dir.path().join("script.bat"), "@echo off").unwrap();
        let path = env::join_paths([dir.path().to_path_buf(), PathBuf::from("")]).unwrap();
        assert_eq!(
            find_in("tool", &path),
            Some(dunce::canonicalize(&tool).unwrap())
        );
        assert_eq!(find_in("script", &path), None);
        assert_eq!(find_in("no-such-tool-devagotchi", &path), None);
        #[cfg(unix)]
        assert!(find_on_path("sh").is_some_and(|p| p.is_absolute()));
    }
}
