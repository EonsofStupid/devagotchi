//! Windows process ownership: a broker-owned Job Object per root.
//!
//! The root is created suspended, assigned to a new Job Object, and only then
//! resumed, so it cannot run or create a child outside the job. The job permits
//! no breakaway and terminates its members when the broker's job handle
//! closes. Observation and termination use that job handle, never a PID.
// Native OS calls live here and nowhere else in this crate's dependents.
#![allow(unsafe_code)]

use devagotchi_contracts::{BridgeError, BridgeResult, OperationState};
use crate::execution::GROUP_SETTLE;
use std::ffi::OsStr;
use std::fs::Metadata;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr::null;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep, timeout};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, FALSE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    WaitForSingleObject,
};

pub(crate) const SCOPE: &str = "Windows root process and every process in the broker-owned Job Object it was assigned to before it ran. Breakaway is not permitted, and closing the broker's job handle terminates remaining members. Work a member hands to a service or another process outside the job is not owned. This is not OS containment.";
pub(crate) const RUNNING_DETAIL: &str = "Root created suspended with exact argv (Microsoft C runtime quoting), an environment cleared except SystemRoot, null stdin, and no console window, then assigned to a broker-owned Job Object before it ran. Executable behavior is not sandboxed.";
pub(crate) const TERMINATION_VERIFIED_DETAIL: &str = " TerminateJobObject was issued through the broker's job handle; no active processes remained in the job.";
pub(crate) const OBSERVATION_FAILED_DETAIL: &str =
    "Job Object observation failed and requested cancellation. ";
pub(crate) const TERMINATION_UNVERIFIED_DETAIL: &str = " Job Object termination was not verified";
pub(crate) const PIPE_TIMEOUT_DETAIL: &str = " Output pipes did not close within one second of root settlement; a process holding them may have received them outside the job. Reading was stopped, output is incomplete, and processes outside the owned Job Object are not tracked or terminated.";
pub(crate) const RUNNING_DETAIL_ACCOUNT: &str = "Root created suspended as the broker's account with its environment, null stdin, and no console window, then assigned to a broker-owned Job Object before it ran. Executable behavior is not sandboxed.";

/// The shell a `command` line runs in: Windows PowerShell from System32, given the line
/// as `-EncodedCommand` (UTF-16LE, base64) so no quoting layer can change it.
pub(crate) fn shell_command(command: &str) -> (std::path::PathBuf, Vec<String>) {
    use base64::Engine as _;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let shell = std::path::Path::new(&root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let utf16: Vec<u8> = command.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    (
        shell,
        [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(std::iter::once(encoded))
        .collect(),
    )
}

pub(crate) const EXECUTABLE_RULE: &str = " (Windows grants must be .exe or .com images; batch files run through cmd.exe parsing, so grant cmd.exe explicitly)";

/// Exit code given to members the broker terminates.
const TERMINATED_EXIT_CODE: u32 = 1;

/// A Windows grant or request target must be a regular file that
/// `CreateProcessW` runs directly as an image.
pub(crate) fn is_directly_executable(path: &Path, metadata: &Metadata) -> bool {
    metadata.is_file()
        && path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com")
            })
}

/// Creates the root suspended and without a console window. Without a console
/// of its own the root and its descendants never receive the broker console's
/// Ctrl+C. Windows system libraries need `SystemRoot`, so it alone survives the
/// cleared environment.
pub(crate) fn configure(command: &mut Command) {
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
}

/// Why a spawned root could not be taken into ownership.
pub(crate) struct AdoptFailure {
    pub(crate) state: OperationState,
    pub(crate) output_complete: bool,
    pub(crate) detail: String,
}

/// The broker's handle to the Job Object that owns one root and every
/// process created inside it.
pub(crate) struct OwnedTree {
    job: OwnedHandle,
}

impl OwnedTree {
    pub(crate) async fn adopt(child: &mut Child) -> Result<Self, AdoptFailure> {
        let (Some(process), Some(pid)) = (child.raw_handle(), child.id()) else {
            return Err(AdoptFailure {
                state: OperationState::Unknown,
                output_complete: false,
                detail: "Spawn returned no process handle; resource status cannot be certified."
                    .into(),
            });
        };
        let job = match owned_job(process) {
            Ok(job) => job,
            Err(error) => {
                // The root is still suspended: it has executed no program code.
                let _ = child.start_kill();
                let terminated = matches!(
                    timeout(crate::execution::ROOT_WAIT, child.wait()).await,
                    Ok(Ok(_))
                );
                return Err(if terminated {
                    AdoptFailure {
                        state: OperationState::Failed,
                        output_complete: true,
                        detail: format!(
                            "Job Object assignment failed before the root ran: {error}. The suspended root was terminated; no program code executed."
                        ),
                    }
                } else {
                    AdoptFailure {
                        state: OperationState::Unknown,
                        output_complete: false,
                        detail: format!(
                            "Job Object assignment failed before the root ran: {error}. Termination of the suspended root was not observed."
                        ),
                    }
                });
            }
        };
        let tree = Self { job };
        if let Err(error) = resume_threads(pid) {
            let termination = tree.terminate().await;
            let _ = timeout(crate::execution::ROOT_WAIT, child.wait()).await;
            return Err(match termination {
                Ok(()) => AdoptFailure {
                    state: OperationState::Failed,
                    output_complete: true,
                    detail: format!(
                        "The suspended root could not be resumed: {error}. Its Job Object was terminated with no active processes remaining."
                    ),
                },
                Err(termination) => AdoptFailure {
                    state: OperationState::Unknown,
                    output_complete: false,
                    detail: format!(
                        "The suspended root could not be resumed: {error}. Job Object termination was not verified: {termination}."
                    ),
                },
            });
        }
        Ok(tree)
    }

    /// Returns when the root has exited. (Increment 26d replaces this look-every-20-ms
    /// with the process handle's own wait.)
    pub(crate) async fn root_exit(&self, child: &mut Child) -> BridgeResult<()> {
        while !self.root_exited(child)? {
            sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    /// Returns when no process in the job is active. (Increment 26d replaces this with
    /// the job's completion-port notification.)
    pub(crate) async fn members_settled(&self) -> BridgeResult<()> {
        while self.has_executing_members()? {
            sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    /// Observes root exit through the owned process handle, which also keeps
    /// the root's identity from being reused.
    fn root_exited(&self, child: &Child) -> BridgeResult<bool> {
        let Some(process) = child.raw_handle() else {
            return Ok(true);
        };
        match unsafe { WaitForSingleObject(process, 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(observation(format!(
                "Cannot observe the owned root: {}",
                io::Error::last_os_error()
            ))),
            other => Err(observation(format!(
                "Unexpected wait result {other} for the owned root"
            ))),
        }
    }

    fn has_executing_members(&self) -> BridgeResult<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == FALSE
        {
            return Err(observation(format!(
                "Cannot query the owned Job Object: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(accounting.ActiveProcesses > 0)
    }

    /// Terminates every member through the job handle, then verifies that no
    /// active process remains. Windows offers no general graceful request for
    /// arbitrary console or GUI processes, so there is no grace period.
    pub(crate) async fn terminate(&self) -> BridgeResult<()> {
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), TERMINATED_EXIT_CODE) } == FALSE {
            return Err(BridgeError::new(
                "process_signal_error",
                format!(
                    "Cannot terminate the owned Job Object: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
        let deadline = Instant::now() + GROUP_SETTLE;
        loop {
            if !self.has_executing_members()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(BridgeError::new(
                    "termination_unverified",
                    "Active processes remained in the owned Job Object after TerminateJobObject",
                ));
            }
            sleep(Duration::from_millis(20)).await;
        }
    }
}

fn observation(message: String) -> BridgeError {
    BridgeError::new("process_observation_error", message)
}

/// Creates an unnamed, non-inheritable Job Object that kills its members when
/// its last handle closes and permits no breakaway, then assigns `process`.
fn owned_job(process: RawHandle) -> io::Result<OwnedHandle> {
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Resumes the threads of a root created with `CREATE_SUSPENDED`. The broker
/// holds the root's process handle, so `pid` cannot name another process.
fn resume_threads(pid: u32) -> io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let owner_field_end =
        std::mem::offset_of!(THREADENTRY32, th32OwnerProcessID) + size_of::<u32>();
    let mut resumed = 0_usize;
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != FALSE;
    while more {
        if entry.dwSize as usize >= owner_field_end && entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, FALSE, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            resumed += 1;
        }
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != FALSE;
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(error);
    }
    if resumed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the suspended root had no thread to resume",
        ));
    }
    Ok(())
}
