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
    "Usage: devagotchi start [--workspace DIR] [--grant PATH]... [--state-dir DIR] [--config FILE]

Runs your broker in the foreground (Ctrl+C stops it; jobs are settled first). The first run
writes the broker's config with the workspace (default: your home directory) and the
toolchains found on PATH (git, rg, python3/python, node, npm, npx, cargo, rustup, go, uv), plus
any --grant; later runs keep the file. Then point your agent at `devagotchi mcp`. A grant
must be what the broker runs directly: an executable file (Windows: an .exe or .com image;
npm and npx are .cmd scripts there, so they stay out).

Defaults: state in ~/.local/state/devagotchi (Windows: %LOCALAPPDATA%\\devagotchi\\state),
config in ~/.config/devagotchi/broker.json (Windows: %APPDATA%\\devagotchi); DEVAGOTCHI_STATE_DIR
and DEVAGOTCHI_CONFIG_DIR override them.";

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
}

fn parse(args: Vec<OsString>) -> Result<Options, String> {
    let mut options = Options {
        state: None,
        config: None,
        workspace: None,
        grants: Vec::new(),
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
    let config = match options
        .config
        .map(absolute)
        .or_else(|| paths::config_dir().map(|d| d.join("broker.json")))
    {
        Some(config) => config,
        None => {
            eprintln!(
                "devagotchi start: no home directory in the environment; pass --config or set DEVAGOTCHI_CONFIG_DIR"
            );
            return ExitCode::from(64);
        }
    };
    // Where no native execution provider exists there is nothing to prepare: the broker
    // answers `unsupported_platform` itself, before any config is written.
    #[cfg(any(target_os = "linux", windows))]
    {
        if let Err(error) = private_dir(&state) {
            eprintln!(
                "devagotchi start: cannot create {}: {error}",
                state.display()
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
        if let Some(parent) = config.parent() {
            account_line(&parent.join(devagotchi_hub::account::FILE_NAME));
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
    let config = serde_json::json!({
        "workspace": workspace,
        "allowed_executables": executables,
        "output_limit_bytes": 262_144,
        "max_operations": 64,
        "max_concurrent": 4,
    });
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())? + "\n";
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!(
        "workspace {}, {} executable grant{}{}",
        workspace.display(),
        executables.len(),
        if executables.len() == 1 { "" } else { "s" },
        if executables.is_empty() {
            " (none of the usual toolchains is on PATH; add some with --grant)".to_string()
        } else {
            format!(
                " ({})",
                executables
                    .iter()
                    .filter_map(|e| e.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
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
