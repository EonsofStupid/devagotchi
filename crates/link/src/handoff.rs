//! Handing a checked update to something that outlives this link and the broker it replaces
//! (docs/spec-machine-update.md, U3 step 3). The link never installs anything itself.
//!
//! Windows: `powershell.exe` runs the updater script (`update-windows.ps1`) on a hidden
//! console of its own, in a new process group, outside any job of ours (breakaway is asked
//! for and dropped when a job refuses it), its output to `<root>\logs\update-<version>.log`: a file, never a pipe back to the link, which kept the
//! 2026-11-05 updater waiting forever. The script runs the installer in upgrade mode and
//! re-runs it with the previous version when the new one does not come up.
//!
//! Linux: the services run as an unprivileged account and the binaries are root's, so the
//! link only writes `<state>/update-request.json`; the root `devagotchi-update.path` unit the
//! installer put in place starts `devagotchi-update.service`, which does the rest.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::update::Status;

/// What a link leaves for the one that replaces it: the version it asked for.
const PENDING: &str = "update-pending.json";

#[derive(Deserialize)]
struct Pending {
    version: String,
}

/// Record that an update to `version` was handed off, or clear the record (None).
pub fn pending(state_dir: &Path, version: Option<&str>) -> Result<(), String> {
    let path = state_dir.join(PENDING);
    match version {
        Some(version) => {
            let record = serde_json::json!({ "version": version }).to_string();
            std::fs::write(&path, record).map_err(|e| format!("write {PENDING}: {e}"))
        }
        None => std::fs::remove_file(&path).map_err(|e| format!("remove {PENDING}: {e}")),
    }
}

/// What the link that just started tells the hub about an update the previous one handed
/// off: `done` when this is the build it asked for, else `failed` with why from the
/// updater's log. None when no update was pending. The record is used once.
pub fn outcome(state_dir: &Path, build: &str) -> Option<Status> {
    let path = state_dir.join(PENDING);
    let pending: Option<Pending> = serde_json::from_slice(&std::fs::read(&path).ok()?).ok();
    let _ = std::fs::remove_file(&path);
    let pending = pending?;
    if pending.version == build {
        return Some(Status {
            state: "done",
            detail: None,
        });
    }
    // The updater says why on one line before it rolls back, which starts this link; the
    // log's last line otherwise.
    let failed = format!("devagotchi update to {} failed: ", pending.version);
    let last = log_path(state_dir, &pending.version)
        .and_then(|log| std::fs::read(log).ok())
        .and_then(|log| {
            let text = String::from_utf8_lossy(&log).into_owned();
            let mut lines = text.lines().rev().map(str::trim).filter(|l| !l.is_empty());
            let why = text
                .lines()
                .rev()
                .find_map(|l| l.trim().strip_prefix(failed.as_str()));
            why.or_else(|| lines.next())
                .map(|l| l.chars().take(300).collect())
        });
    Some(Status {
        state: "failed",
        detail: Some(last.unwrap_or_else(|| format!("{} did not come up", pending.version))),
    })
}

/// This machine's archive and installer as the hub lists them, or None where the hub hands
/// out no build.
pub fn files() -> Option<(String, &'static str)> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    if cfg!(windows) {
        Some((
            format!("devagotchi-{arch}-pc-windows-msvc.zip"),
            "install-devagotchi.ps1",
        ))
    } else if cfg!(target_os = "linux") {
        Some((
            format!("devagotchi-{arch}-unknown-linux-gnu.tar.xz"),
            "install-devagotchi.sh",
        ))
    } else {
        None
    }
}

/// The installation root on Windows: the installer keeps the link's state in
/// `<root>\link-state`.
#[cfg(windows)]
fn root(state_dir: &Path) -> Option<&Path> {
    state_dir.parent()
}

/// The prefix on Linux: this binary runs from `<prefix>/versions/<version>/devagotchi`.
#[cfg(target_os = "linux")]
fn prefix() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.parent()?.to_path_buf())
}

/// Where the updater writes how the update to `version` went.
#[cfg(windows)]
pub fn log_path(state_dir: &Path, version: &str) -> Option<PathBuf> {
    root(state_dir).map(|root| root.join("logs").join(format!("update-{version}.log")))
}

#[cfg(target_os = "linux")]
pub fn log_path(_state_dir: &Path, version: &str) -> Option<PathBuf> {
    prefix().map(|prefix| prefix.join("update").join(format!("update-{version}.log")))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn log_path(_state_dir: &Path, _version: &str) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
pub fn start(state_dir: &Path, dir: &Path, version: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let root = root(state_dir).ok_or("the link's state has no installation root")?;
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("create {}: {e}", logs.display()))?;
    let script = dir.join("update-devagotchi.ps1");
    std::fs::write(&script, include_str!("update-windows.ps1"))
        .map_err(|e| format!("write {}: {e}", script.display()))?;
    let log = log_path(state_dir, version).ok_or("no log path")?;
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let powershell = Path::new(&system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let spawn = |flags: u32| -> std::io::Result<()> {
        let out = std::fs::File::create(&log)?;
        let err = out.try_clone()?;
        Command::new(&powershell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script)
            .arg("-Installer")
            .arg(dir.join("install-devagotchi.ps1"))
            .args(["-Version", version, "-ArtifactDir"])
            .arg(dir)
            .arg("-Root")
            .arg(root)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | flags)
            .spawn()
            .map(drop)
    };
    // A job that does not allow breakaway refuses the flag (access denied); the link is not
    // in the broker's job, so without it the updater still outlives what it stops.
    spawn(CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(0))
        .map_err(|e| format!("start the updater: {e}"))
}

#[cfg(target_os = "linux")]
pub fn start(state_dir: &Path, dir: &Path, version: &str) -> Result<(), String> {
    let request = serde_json::json!({ "version": version, "dir": dir });
    let path = state_dir.join("update-request.json");
    let tmp = state_dir.join(".update-request.json.tmp");
    std::fs::write(&tmp, request.to_string())
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn start(_state_dir: &Path, _dir: &Path, _version: &str) -> Result<(), String> {
    Err("this machine has no updater; reinstall it to update".into())
}
