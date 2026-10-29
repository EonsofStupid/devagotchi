//! This user's broker in the background, so an AI client's whole setup is `devagotchi mcp`
//! (as Desktop Commander's is one `npx` line): when no broker answers, `devagotchi mcp`
//! runs `devagotchi start --background`, which starts the same `devagotchi start` detached
//! and returns once it answers.
//!
//! - It is still one broker per user: two starts racing meet at the broker's own instance
//!   lock, and the loser exits; both callers then find the winner through `<state>/endpoint`.
//! - Detached: its own process group (Unix), or no console, its own process group and out
//!   of the caller's job where the job allows it (Windows). The process that started it
//!   exits once it answers, so closing an AI client, its terminal or its process tree
//!   leaves the broker and its jobs running.
//! - It writes to `<state>/broker.log` (owner-only), since no terminal is attached.
//! - Nothing is recorded about the process: a broker is found by asking the endpoint, never
//!   by a remembered PID.

use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use devagotchi_contracts::{ListInput, RequestBody};

/// The background broker's output, in its state directory.
pub const LOG_FILE: &str = "broker.log";
/// How long a background start may take before the caller gives up and points at the log.
pub const START_WITHIN: Duration = Duration::from_secs(20);

/// The endpoint this user's broker answers on, when one does: the endpoint file names it,
/// and a real request over it is answered.
pub fn answering(state: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(state.join(crate::ENDPOINT_FILE)).ok()?;
    let endpoint = PathBuf::from(text.trim());
    if endpoint.as_os_str().is_empty() {
        return None;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let body = RequestBody::OperationList(ListInput {
        offset: 0,
        limit: 1,
    });
    let answered = runtime.block_on(async {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(2),
                devagotchi_ipc::request(&endpoint, body)
            )
            .await,
            Ok(Ok(_))
        )
    });
    answered.then_some(endpoint)
}

/// Wait for this user's broker to answer, up to `within`.
pub fn wait_until_answering(state: &Path, within: Duration) -> Option<PathBuf> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(endpoint) = answering(state) {
            return Some(endpoint);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Start `exe args…` detached from this process, its output appended to
/// `<state>/broker.log`. Returns once it is started, not once it answers.
pub fn spawn_detached(exe: &Path, args: &[OsString], state: &Path) -> io::Result<()> {
    private_dir(state)?;
    let log = open_log(&state.join(LOG_FILE))?;
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = spawn(command)?;
    // Reaped here, so a broker that stops at once (another start won) is not left a zombie
    // while this process lives.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(unix)]
fn spawn(mut command: Command) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;
    // Its own process group: a terminal's Ctrl+C, or a client signalling the adapter's
    // group, does not reach it.
    command.process_group(0);
    command.spawn()
}

#[cfg(windows)]
fn spawn(mut command: Command) -> io::Result<Child> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    match command.spawn() {
        Ok(child) => Ok(child),
        // Some launchers put their children in a job that may not be left: start inside it.
        Err(_) => {
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            command.spawn()
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn spawn(mut command: Command) -> io::Result<Child> {
    command.spawn()
}

fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_log(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_answers_without_an_endpoint_file_or_with_a_dead_one() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(answering(dir.path()), None);
        std::fs::write(dir.path().join(crate::ENDPOINT_FILE), "\n").unwrap();
        assert_eq!(answering(dir.path()), None, "an empty file names nothing");
        let dead = dir.path().join("gone.sock");
        std::fs::write(
            dir.path().join(crate::ENDPOINT_FILE),
            format!("{}\n", dead.display()),
        )
        .unwrap();
        assert_eq!(answering(dir.path()), None, "a stale file after a reboot");
        assert_eq!(
            wait_until_answering(dir.path(), Duration::from_millis(250)),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_log_and_its_directory_are_the_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        spawn_detached(Path::new("/bin/true"), &[], &state).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&state.join(LOG_FILE)), 0o600);
    }
}
