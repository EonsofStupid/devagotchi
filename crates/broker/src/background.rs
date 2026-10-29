//! This user's broker in the background, so an AI client's whole setup is `devagotchi mcp`
//! (as Desktop Commander's is one `npx` line): when no broker answers, `devagotchi mcp`
//! runs `devagotchi start --background`, which starts the same `devagotchi start` detached
//! and returns once it answers.
//!
//! - Still one broker per user: the state directory is made by the IPC layer that checks
//!   it, exactly as the broker makes it, and a start finds a broker that already holds it
//!   (starting up, or running and not answering yet) and waits for that one instead.
//! - Detached (`devagotchi_core::execution::detached`): a session of its own (Linux), or no
//!   console, its own process group and out of the caller's job where allowed (Windows);
//!   it inherits nothing but the null device and its log, so an AI client's pipes are never
//!   held open; it runs in its state directory with a pared-down environment. The process
//!   that started it exits once it answers, so closing an AI client, its terminal or its
//!   process tree leaves the broker and its jobs running.
//! - It writes to `<state>/broker.log` (owner-only).
//! - A broker that stops at once is reported at once, with what it wrote to its log (only
//!   what this start's broker wrote: the log is appended to by every start).
//! - Nothing is recorded about the process: a broker is found by asking its endpoint, never
//!   by a remembered PID.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use devagotchi_contracts::{ListInput, RequestBody};

/// The background broker's output, in its state directory.
pub const LOG_FILE: &str = "broker.log";
/// How long a background start may take before the caller gives up and says why.
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

/// The last `lines` lines of `<state>/broker.log`, to say why a background start failed.
pub fn log_tail(state: &Path, lines: usize) -> String {
    log_tail_since(state, 0, lines)
}

/// How long the log is now: a start reads only what was written after it, since the log
/// is appended to by every start (an earlier one that lost a race says so in it).
fn log_len(state: &Path) -> u64 {
    std::fs::metadata(state.join(LOG_FILE))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// The last `lines` lines written to the log after its first `since` bytes.
fn log_tail_since(state: &Path, since: u64, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = Vec::new();
    if let Ok(mut file) = std::fs::File::open(state.join(LOG_FILE))
        && file.seek(SeekFrom::Start(since)).is_ok()
    {
        let _ = file.read_to_end(&mut bytes);
    }
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Why a background start did not end with a broker answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartFailure {
    /// The broker it started stopped; the end of its log says why.
    Stopped(String),
    /// A broker holds the state directory but did not answer within the wait.
    NotAnswering,
    /// Nothing answered within the wait; the end of the log.
    TimedOut(String),
    /// The state directory or the process could not be set up.
    Setup(String),
}

impl std::fmt::Display for StartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartFailure::Stopped(log) => {
                write!(f, "your broker stopped as it started:\n{log}")
            }
            StartFailure::NotAnswering => write!(
                f,
                "your broker is running but did not answer within {} s; it may be busy or stuck",
                START_WITHIN.as_secs()
            ),
            StartFailure::TimedOut(log) => write!(
                f,
                "your broker did not answer within {} s:\n{log}",
                START_WITHIN.as_secs()
            ),
            StartFailure::Setup(why) => write!(f, "your broker could not be started: {why}"),
        }
    }
}

/// Start `program args…` (a `devagotchi start` with explicit paths) detached, unless a
/// broker already holds the state directory, and wait until a broker answers there.
pub fn start_and_wait(
    program: &Path,
    args: &[OsString],
    state: &Path,
    within: Duration,
) -> Result<PathBuf, StartFailure> {
    let deadline = Instant::now() + within;
    let since = log_len(state);
    let mut started = if prepare_state(state).map_err(StartFailure::Setup)? {
        Some(spawn(program, args, state).map_err(|e| StartFailure::Setup(e.to_string()))?)
    } else {
        None
    };
    let mut held_by_another = started.is_none();
    loop {
        if let Some(endpoint) = answering(state) {
            return Ok(endpoint);
        }
        if let Some(child) = &mut started
            && child
                .try_wait()
                .map_err(|e| StartFailure::Setup(e.to_string()))?
                .is_some()
        {
            started = None;
            let tail = log_tail_since(state, since, 20);
            // Another start won the state directory first: wait for that broker instead.
            if !tail.contains("broker_already_running") {
                return Err(StartFailure::Stopped(tail));
            }
            held_by_another = true;
        }
        if Instant::now() >= deadline {
            return Err(if held_by_another {
                StartFailure::NotAnswering
            } else {
                StartFailure::TimedOut(log_tail_since(state, since, 20))
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(any(target_os = "linux", windows))]
fn spawn(
    program: &Path,
    args: &[OsString],
    state: &Path,
) -> std::io::Result<devagotchi_core::execution::detached::Detached> {
    use devagotchi_core::execution::detached;
    let env = detached::environment(std::env::vars_os());
    detached::spawn(&detached::DetachedSpec {
        program,
        args,
        env: &env,
        cwd: state,
        log: &state.join(LOG_FILE),
    })
}

/// Where no execution provider exists there is no broker to start in the background.
#[cfg(not(any(target_os = "linux", windows)))]
fn spawn(_program: &Path, _args: &[OsString], _state: &Path) -> std::io::Result<Never> {
    Err(std::io::Error::other(
        "unsupported_platform: the macOS provider is pending",
    ))
}

#[cfg(not(any(target_os = "linux", windows)))]
enum Never {}

#[cfg(not(any(target_os = "linux", windows)))]
impl Never {
    fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        match *self {}
    }
}

/// Make the state directory exactly as the broker does, through the IPC layer that checks
/// it (on Windows it must be made owner-only by the operator's own SID, which a plain
/// directory made by an administrator is not), before the log is written into it. False
/// when a broker already holds it.
fn prepare_state(state: &Path) -> Result<bool, String> {
    if let Some(parent) = state.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    match devagotchi_ipc::StateGuard::acquire(state) {
        Ok(guard) => {
            drop(guard);
            Ok(true)
        }
        Err(error) if error.code == "broker_already_running" => Ok(false),
        Err(error) => Err(error.to_string()),
    }
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
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_broker_that_stops_at_once_is_reported_at_once_with_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let begun = Instant::now();
        let failure = start_and_wait(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "echo 'config: no such file' >&2; exit 2".into(),
            ],
            &state,
            START_WITHIN,
        )
        .unwrap_err();
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "{:?}",
            begun.elapsed()
        );
        assert_eq!(
            failure,
            StartFailure::Stopped("config: no such file".into())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_earlier_lost_race_in_the_log_does_not_hide_a_new_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        devagotchi_ipc::StateGuard::acquire(&state)
            .map(drop)
            .unwrap();
        // What a start that lost the race to another broker left behind, minutes ago.
        std::fs::write(
            state.join(LOG_FILE),
            "devagotchi start: broker_already_running: another broker holds the state\n",
        )
        .unwrap();
        let begun = Instant::now();
        let failure = start_and_wait(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "echo 'config: no such file' >&2; exit 2".into(),
            ],
            &state,
            START_WITHIN,
        )
        .unwrap_err();
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "{:?}",
            begun.elapsed()
        );
        assert_eq!(
            failure,
            StartFailure::Stopped("config: no such file".into())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nothing_is_started_while_a_broker_holds_the_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let held = devagotchi_ipc::StateGuard::acquire(&state).unwrap();
        let marker = dir.path().join("started");
        let failure = start_and_wait(
            Path::new("/bin/sh"),
            &["-c".into(), format!("touch {}", marker.display()).into()],
            &state,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(failure, StartFailure::NotAnswering);
        assert!(!marker.exists(), "a second broker was started");
        drop(held);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_state_and_its_log_are_the_users_alone_and_the_process_runs_there() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let failure = start_and_wait(
            Path::new("/bin/sh"),
            &["-c".into(), "pwd".into()],
            &state,
            START_WITHIN,
        )
        .unwrap_err();
        let StartFailure::Stopped(log) = failure else {
            panic!("{failure:?}")
        };
        assert_eq!(
            log.trim(),
            std::fs::canonicalize(&state).unwrap().to_str().unwrap()
        );
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&state.join(LOG_FILE)), 0o600);
    }
}
