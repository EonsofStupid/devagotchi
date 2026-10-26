//! Linux process ownership: the root's inherited process group.
//!
//! The root stays unreaped while it is observed and signalled, so its PID and
//! group identity cannot be reused by an unrelated process.
//!
//! Exits are events, not polls: a pidfd (Linux 5.3+) becomes readable when the process it
//! names exits, so the broker sleeps until the root ends, then until every executing
//! member of its group has ended, re-reading /proc only when one of them did. A kernel or
//! sandbox without pidfd_open falls back to looking every 20 ms, as before.
// Native OS calls live here and nowhere else in this crate's dependents.
#![allow(unsafe_code)]

use devagotchi_contracts::{BridgeError, BridgeResult, OperationState};
use crate::execution::GROUP_SETTLE;
use std::fs::Metadata;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::time::Duration;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

/// Longest wait between SIGTERM and SIGKILL; it ends early once the group has left.
const TERM_GRACE: Duration = Duration::from_millis(200);
/// Members watched at once. Settlement re-reads the group once all of them have ended, so
/// a larger group is watched in turns.
const MAX_WATCHED: usize = 64;
/// The look interval when this kernel offers no pidfd.
const NO_PIDFD_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) const SCOPE: &str = "Linux root process and inherited process group; excludes processes that leave the group. This is not OS containment.";
pub(crate) const RUNNING_DETAIL: &str = "Root spawned with exact argv, cleared environment, null stdin, and a new Linux process group. Executable behavior is not sandboxed.";
pub(crate) const TERMINATION_VERIFIED_DETAIL: &str = " SIGTERM then SIGKILL were issued while the root PID was still owned; no executing members remained in its inherited process group.";
pub(crate) const OBSERVATION_FAILED_DETAIL: &str =
    "Process-group observation failed and requested cancellation. ";
pub(crate) const TERMINATION_UNVERIFIED_DETAIL: &str = " Group termination was not verified";
pub(crate) const PIPE_TIMEOUT_DETAIL: &str = " Output pipes did not close within one second of root settlement; a descendant may still hold them open. Reading was stopped, output is incomplete, and processes outside the owned group are not tracked or terminated.";
pub(crate) const RUNNING_DETAIL_ACCOUNT: &str = "Root spawned as the broker's account with its environment, null stdin, and a new Linux process group. Executable behavior is not sandboxed.";
pub(crate) const EXECUTABLE_RULE: &str = "";

/// The shell a `command` line runs in: the account's `$SHELL` when it is one of the
/// Bourne family, else bash, else `/bin/sh`. A login shell of `nologin` or `false` (a
/// service account) is never used.
pub(crate) fn shell_command(command: &str) -> (std::path::PathBuf, Vec<String>) {
    let bourne = |path: &Path| {
        matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("bash" | "zsh" | "sh" | "dash" | "ksh")
        ) && std::fs::metadata(path).is_ok_and(|metadata| is_directly_executable(path, &metadata))
    };
    let shell = std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute() && bourne(path))
        .or_else(|| Some(std::path::PathBuf::from("/bin/bash")).filter(|path| bourne(path)))
        .unwrap_or_else(|| std::path::PathBuf::from("/bin/sh"));
    (shell, vec!["-c".to_owned(), command.to_owned()])
}

/// A Linux grant or request target must be a regular file with an execute bit.
pub(crate) fn is_directly_executable(_path: &Path, metadata: &Metadata) -> bool {
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// Starts the root in its own process group, detached from the broker's
/// terminal signals.
pub(crate) fn configure(command: &mut Command) {
    command.as_std_mut().process_group(0);
}

/// Why a spawned root could not be taken into ownership.
pub(crate) struct AdoptFailure {
    pub(crate) state: OperationState,
    pub(crate) output_complete: bool,
    pub(crate) detail: String,
}

/// The root's PID and inherited process group, held while the root is unreaped, and a
/// pidfd for the root when the kernel offers one.
pub(crate) struct OwnedTree {
    pid: u32,
    root: Option<AsyncFd<OwnedFd>>,
}

impl OwnedTree {
    pub(crate) async fn adopt(child: &mut Child) -> Result<Self, AdoptFailure> {
        let Some(pid) = child.id() else {
            return Err(AdoptFailure {
                state: OperationState::Unknown,
                output_complete: false,
                detail: "Spawn returned no process identity; resource status cannot be certified."
                    .into(),
            });
        };
        // The root is this broker's unreaped child, so its PID names it alone.
        let root = match pidfd(pid) {
            Ok(root) => root,
            Err(error) if pidfd_unsupported(&error) => None,
            Err(error) => {
                return Err(AdoptFailure {
                    state: OperationState::Unknown,
                    output_complete: false,
                    detail: format!("Cannot open a pidfd for the owned root: {error}"),
                });
            }
        };
        Ok(Self { pid, root })
    }

    /// Returns when the root has exited, without reaping it.
    pub(crate) async fn root_exit(&self, _child: &mut Child) -> BridgeResult<()> {
        let Some(root) = &self.root else {
            while !root_exited_without_reaping(self.pid)? {
                sleep(NO_PIDFD_INTERVAL).await;
            }
            return Ok(());
        };
        loop {
            let mut ready = root.readable().await.map_err(observation)?;
            if root_exited_without_reaping(self.pid)? {
                return Ok(());
            }
            ready.clear_ready();
        }
    }

    /// Returns when no member of the owned group is executing. It reads /proc, sleeps
    /// until every member it found has exited, and reads again: a member may have started
    /// another before it ended.
    pub(crate) async fn members_settled(&self) -> BridgeResult<()> {
        loop {
            let members = group_members(self.pid)?;
            if members.is_empty() {
                return Ok(());
            }
            if self.root.is_none() {
                sleep(NO_PIDFD_INTERVAL).await;
                continue;
            }
            let mut watched = Vec::new();
            for member in members.into_iter().take(MAX_WATCHED) {
                let Some(fd) = pidfd(member).map_err(observation)? else {
                    continue;
                };
                // Opened by number: keep it only if that number still names a member.
                if member_state(member, self.pid)? {
                    watched.push(fd);
                }
            }
            for fd in &watched {
                fd.readable().await.map_err(observation)?.retain_ready();
            }
        }
    }

    /// SIGTERM, up to a short grace for the group to leave, SIGKILL, then verified
    /// settlement of the group.
    pub(crate) async fn terminate(&self) -> BridgeResult<()> {
        let term_error = signal_group(self.pid, libc::SIGTERM).err();
        let _ = timeout(TERM_GRACE, self.members_settled()).await;
        let kill_error = signal_group(self.pid, libc::SIGKILL).err();
        if let Some(error) = term_error.or(kill_error) {
            return Err(error);
        }
        match timeout(GROUP_SETTLE, self.members_settled()).await {
            Ok(settled) => settled,
            Err(_) => Err(BridgeError::new(
                "termination_unverified",
                "Executing members remained in the owned process group after SIGKILL",
            )),
        }
    }
}

fn observation(error: io::Error) -> BridgeError {
    BridgeError::new("process_observation_error", error.to_string())
}

/// A pidfd for `pid`, registered with the runtime: readable once that process has exited,
/// and naming that process only, whatever later happens to the number. None when no such
/// process exists any more.
fn pidfd(pid: u32) -> io::Result<Option<AsyncFd<OwnedFd>>> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(None),
            _ => Err(error),
        };
    }
    // pidfd_open returned a new descriptor that nothing else owns, and the OwnedFd keeps
    // it open, unreplaced, for as long as the runtime has it registered.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    Ok(Some(unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }?))
}

/// No pidfd here: a kernel before 5.3, or a sandbox that filters the call.
fn pidfd_unsupported(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM | libc::EACCES))
}

// WNOWAIT observes the root without releasing its PID. The same owned child is
// not reaped until all process-group signals have been sent, preventing PID reuse
// from retargeting a later group signal at an unrelated process.
fn root_exited_without_reaping(pid: u32) -> BridgeResult<bool> {
    let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut information,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(BridgeError::new(
            "process_observation_error",
            format!("Cannot observe the owned unreaped root: {error}"),
        ));
    }
    Ok(unsafe { information.si_pid() } != 0)
}

fn signal_group(pid: u32, signal: i32) -> BridgeResult<()> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(BridgeError::new(
            "process_signal_error",
            "Invalid owned process group",
        ));
    }
    let result = unsafe { libc::kill(-(pid as i32), signal) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(BridgeError::new(
                "process_signal_error",
                format!("Cannot signal the owned process group: {error}"),
            ));
        }
    }
    Ok(())
}

/// The executing members of the owned group, read from /proc while the root remains
/// unreaped. Zombies have ended execution and cannot run or create more processes.
fn group_members(group: u32) -> BridgeResult<Vec<u32>> {
    let entries = std::fs::read_dir("/proc")
        .map_err(|error| BridgeError::new("process_observation_error", error.to_string()))?;
    let mut members = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| BridgeError::new("process_observation_error", error.to_string()))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if member_state(pid, group)? {
            members.push(pid);
        }
    }
    Ok(members)
}

/// Whether `pid` is an executing member of `group` now; a process that is gone is not.
fn member_state(pid: u32, group: u32) -> BridgeResult<bool> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        // A process that exits while its stat is read reports ESRCH.
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(false),
        Err(error) => {
            return Err(BridgeError::new(
                "process_observation_error",
                format!("Cannot inspect /proc process groups: {error}"),
            ));
        }
    };
    let Some(end) = stat.rfind(')') else {
        return Err(BridgeError::new(
            "process_observation_error",
            "Malformed /proc process stat",
        ));
    };
    let fields: Vec<_> = stat[end + 1..].split_whitespace().take(3).collect();
    if fields.len() != 3 {
        return Err(BridgeError::new(
            "process_observation_error",
            "Incomplete /proc process stat",
        ));
    }
    Ok(fields[2].parse::<u32>().ok() == Some(group) && fields[0] != "Z" && fields[0] != "X")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::time::Instant;

    fn spawn(script: &str) -> Child {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure(&mut command);
        command.spawn().unwrap()
    }

    async fn owned(child: &mut Child) -> OwnedTree {
        let tree = OwnedTree::adopt(child).await.ok().unwrap();
        assert!(tree.root.is_some(), "this kernel should offer pidfd_open (Linux 5.3+)");
        tree
    }

    #[tokio::test]
    async fn a_pidfd_is_readable_when_the_root_exits_and_not_before() {
        let mut child = spawn("sleep 0.5");
        let tree = owned(&mut child).await;
        let root = tree.root.as_ref().unwrap();
        assert!(
            timeout(Duration::from_millis(100), root.readable()).await.is_err(),
            "readable while the root still runs"
        );
        let began = Instant::now();
        tree.root_exit(&mut child).await.unwrap();
        assert!(began.elapsed() < Duration::from_secs(10));
        // Observed without reaping: the status is still there to collect.
        assert!(root_exited_without_reaping(tree.pid).unwrap());
        assert!(child.wait().await.unwrap().success());
    }

    #[tokio::test]
    async fn members_that_outlive_the_root_are_waited_for_by_their_exits() {
        // More members than are watched at once: they are watched in turns.
        let mut child = spawn("i=0; while [ $i -lt 100 ]; do sleep 0.4 & i=$((i+1)); done; exit 0");
        let tree = owned(&mut child).await;
        tree.root_exit(&mut child).await.unwrap();
        assert!(!group_members(tree.pid).unwrap().is_empty(), "members outlive the root");
        let began = Instant::now();
        timeout(Duration::from_secs(20), tree.members_settled())
            .await
            .expect("settled")
            .unwrap();
        assert!(group_members(tree.pid).unwrap().is_empty());
        assert!(began.elapsed() < Duration::from_secs(20));
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn termination_ends_root_and_members_and_verifies_it() {
        let mut child = spawn("sleep 30 & sleep 30");
        let tree = owned(&mut child).await;
        while group_members(tree.pid).unwrap().len() < 2 {
            sleep(Duration::from_millis(10)).await;
        }
        tree.terminate().await.unwrap();
        assert!(group_members(tree.pid).unwrap().is_empty());
        assert!(!child.wait().await.unwrap().success());
    }

    #[tokio::test]
    async fn a_member_that_ignores_sigterm_is_killed_after_the_grace() {
        let mut child = spawn("trap '' TERM; sleep 30 & wait");
        let tree = owned(&mut child).await;
        while group_members(tree.pid).unwrap().len() < 2 {
            sleep(Duration::from_millis(10)).await;
        }
        let began = Instant::now();
        tree.terminate().await.unwrap();
        // The root ignored SIGTERM, so the grace ran out before SIGKILL ended it.
        assert!(began.elapsed() >= TERM_GRACE, "{:?}", began.elapsed());
        assert!(group_members(tree.pid).unwrap().is_empty());
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn a_descendant_that_leaves_the_group_does_not_hold_settlement() {
        // setsid starts a new session: outside the owned group, as SCOPE says.
        let mut child = spawn("setsid sleep 3 & sleep 0.2; exit 0");
        let tree = owned(&mut child).await;
        tree.root_exit(&mut child).await.unwrap();
        let began = Instant::now();
        tree.members_settled().await.unwrap();
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());
        child.wait().await.unwrap();
    }
}
