//! Linux process ownership: the root's inherited process group.
//!
//! The root stays unreaped while it is observed and signalled, so its PID and
//! group identity cannot be reused by an unrelated process.

use super::GROUP_SETTLE;
use crate::contracts::{BridgeError, BridgeResult, OperationState};
use std::fs::Metadata;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep};

/// Bounded wait between SIGTERM and SIGKILL.
const TERM_GRACE: Duration = Duration::from_millis(200);

pub(super) const SCOPE: &str = "Linux root process and inherited process group; excludes processes that leave the group. This is not OS containment.";
pub(super) const RUNNING_DETAIL: &str = "Root spawned with exact argv, cleared environment, null stdin, and a new Linux process group. Executable behavior is not sandboxed.";
pub(super) const TERMINATION_VERIFIED_DETAIL: &str = " SIGTERM then SIGKILL were issued while the root PID was still owned; no executing members remained in its inherited process group.";
pub(super) const OBSERVATION_FAILED_DETAIL: &str =
    "Process-group observation failed and requested cancellation. ";
pub(super) const TERMINATION_UNVERIFIED_DETAIL: &str = " Group termination was not verified";
pub(super) const PIPE_TIMEOUT_DETAIL: &str = " Output pipes did not close within one second of root settlement; a descendant may still hold them open. Reading was stopped, output is incomplete, and processes outside the owned group are not tracked or terminated.";
pub(super) const EXECUTABLE_RULE: &str = "";

/// A Linux grant or request target must be a regular file with an execute bit.
pub(super) fn is_directly_executable(_path: &Path, metadata: &Metadata) -> bool {
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// Starts the root in its own process group, detached from the broker's
/// terminal signals.
pub(super) fn configure(command: &mut Command) {
    command.as_std_mut().process_group(0);
}

/// Why a spawned root could not be taken into ownership.
pub(super) struct AdoptFailure {
    pub(super) state: OperationState,
    pub(super) output_complete: bool,
    pub(super) detail: String,
}

/// The root's PID and inherited process group, held while the root is unreaped.
pub(super) struct OwnedTree {
    pid: u32,
}

impl OwnedTree {
    pub(super) async fn adopt(child: &mut Child) -> Result<Self, AdoptFailure> {
        match child.id() {
            Some(pid) => Ok(Self { pid }),
            None => Err(AdoptFailure {
                state: OperationState::Unknown,
                output_complete: false,
                detail: "Spawn returned no process identity; resource status cannot be certified."
                    .into(),
            }),
        }
    }

    /// Observes root exit without reaping it.
    pub(super) fn root_exited(&self, _child: &Child) -> BridgeResult<bool> {
        root_exited_without_reaping(self.pid)
    }

    pub(super) fn has_executing_members(&self) -> BridgeResult<bool> {
        group_has_executing_members(self.pid)
    }

    /// SIGTERM, a bounded grace, SIGKILL, then verified settlement of the group.
    pub(super) async fn terminate(&self) -> BridgeResult<()> {
        end_owned_group(self.pid).await
    }
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

/// A Linux /proc observation while the root remains unreaped. Zombies have
/// ended execution and cannot run or create more processes.
fn group_has_executing_members(group: u32) -> BridgeResult<bool> {
    let entries = std::fs::read_dir("/proc")
        .map_err(|error| BridgeError::new("process_observation_error", error.to_string()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| BridgeError::new("process_observation_error", error.to_string()))?;
        if entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
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
        if fields[2].parse::<u32>().ok() == Some(group) && fields[0] != "Z" && fields[0] != "X" {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn end_owned_group(pid: u32) -> BridgeResult<()> {
    let term_error = signal_group(pid, libc::SIGTERM).err();
    sleep(TERM_GRACE).await;
    let kill_error = signal_group(pid, libc::SIGKILL).err();
    if let Some(error) = term_error.or(kill_error) {
        return Err(error);
    }
    let deadline = Instant::now() + GROUP_SETTLE;
    loop {
        if !group_has_executing_members(pid)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(BridgeError::new(
                "termination_unverified",
                "Executing members remained in the owned process group after SIGKILL",
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}
