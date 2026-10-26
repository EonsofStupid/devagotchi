//! Windows process ownership: a broker-owned Job Object per root.
//!
//! The root is created suspended, assigned to a new Job Object, and only then
//! resumed, so it cannot run or create a child outside the job. The job permits
//! no breakaway and terminates its members when the broker's job handle
//! closes. Observation and termination use that job handle, never a PID.
//!
//! Exits are events, not polls: the root's exit is the wait on its process handle, and
//! the job reports to its own I/O completion port as members start and exit, so the
//! broker queries the job only when a member's exit was reported. Windows documents job
//! notifications as not guaranteed to arrive, so while members outlive the root the job
//! is also queried once a second.
// Native OS calls live here and nowhere else in this crate's dependents.
#![allow(unsafe_code)]

use devagotchi_contracts::{BridgeError, BridgeResult, OperationState};
use crate::execution::GROUP_SETTLE;
use std::ffi::OsStr;
use std::fs::Metadata;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::time::timeout;
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, FALSE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::SystemServices::{
    JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS, JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO,
    JOB_OBJECT_MSG_EXIT_PROCESS,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, INFINITE, OpenThread, ResumeThread,
    THREAD_SUSPEND_RESUME,
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
/// The completion key the job's notifications carry.
const JOB_KEY: usize = 1;
/// The completion key that tells a job's listener thread to end.
const STOP_KEY: usize = 2;
/// Windows does not guarantee job notifications arrive: while members outlive the root,
/// the job is also queried this often.
const MISSED_NOTICE_BACKSTOP: Duration = Duration::from_secs(1);

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
/// process created inside it, and the job's completion-port listener.
pub(crate) struct OwnedTree {
    job: OwnedHandle,
    notices: JobNotices,
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
            Ok(owned) => owned,
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
        let (job, notices) = job;
        let tree = Self { job, notices };
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

    /// Returns when the root has exited: the wait on its process handle, which the
    /// runtime registers with the system thread pool. The job, not this handle, owns the
    /// tree, so nothing after this needs the root's handle.
    pub(crate) async fn root_exit(&self, child: &mut Child) -> BridgeResult<()> {
        child
            .wait()
            .await
            .map(|_| ())
            .map_err(|error| observation(format!("Cannot observe the owned root: {error}")))
    }

    /// Returns when no process in the job is active. The job is queried now and again
    /// each time it reports a member's exit (or, as a backstop for a lost notice, after a
    /// second without one).
    pub(crate) async fn members_settled(&self) -> BridgeResult<()> {
        let mut exits = self.notices.exits.clone();
        loop {
            exits.borrow_and_update();
            if !self.has_executing_members()? {
                return Ok(());
            }
            match timeout(MISSED_NOTICE_BACKSTOP, exits.changed()).await {
                Ok(Ok(())) | Err(_) => {}
                // The listener ended: no more notices, so the backstop alone remains.
                Ok(Err(_)) => tokio::time::sleep(MISSED_NOTICE_BACKSTOP).await,
            }
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
        match timeout(GROUP_SETTLE, self.members_settled()).await {
            Ok(settled) => settled,
            Err(_) => Err(BridgeError::new(
                "termination_unverified",
                "Active processes remained in the owned Job Object after TerminateJobObject",
            )),
        }
    }
}

/// A job's I/O completion port and the thread that waits on it. Each exit notice (and the
/// job's active-process-zero notice) advances `exits`.
struct JobNotices {
    port: Arc<OwnedHandle>,
    exits: watch::Receiver<u64>,
}

impl JobNotices {
    /// Create the port, associate `job` with it, and start its listener.
    fn listen(job: &OwnedHandle) -> io::Result<Self> {
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        let port = Arc::new(unsafe { OwnedHandle::from_raw_handle(port) });
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: JOB_KEY as *mut core::ffi::c_void,
            CompletionPort: port.as_raw_handle(),
        };
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectAssociateCompletionPortInformation,
                (&association as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        let (sender, exits) = watch::channel(0_u64);
        let listening = Arc::clone(&port);
        std::thread::Builder::new()
            .name("devagotchi-job".into())
            .spawn(move || listen(&listening, &sender))?;
        Ok(Self { port, exits })
    }
}

impl Drop for JobNotices {
    fn drop(&mut self) {
        // Ends the listener; the port closes when both it and this handle are gone.
        unsafe { PostQueuedCompletionStatus(self.port.as_raw_handle(), 0, STOP_KEY, null()) };
    }
}

/// Wait on the job's port until told to stop, advancing `exits` on each member exit.
fn listen(port: &OwnedHandle, exits: &watch::Sender<u64>) {
    loop {
        let mut message = 0_u32;
        let mut key = 0_usize;
        let mut overlapped: *mut OVERLAPPED = null_mut();
        let ok = unsafe {
            GetQueuedCompletionStatus(port.as_raw_handle(), &mut message, &mut key, &mut overlapped, INFINITE)
        };
        if ok == FALSE || key == STOP_KEY {
            return;
        }
        if key == JOB_KEY
            && matches!(
                message,
                JOB_OBJECT_MSG_EXIT_PROCESS
                    | JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS
                    | JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO
            )
        {
            exits.send_modify(|count| *count = count.wrapping_add(1));
        }
    }
}

fn observation(message: String) -> BridgeError {
    BridgeError::new("process_observation_error", message)
}

/// Creates an unnamed, non-inheritable Job Object that kills its members when
/// its last handle closes and permits no breakaway, starts listening to its
/// notices, then assigns `process`.
fn owned_job(process: RawHandle) -> io::Result<(OwnedHandle, JobNotices)> {
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
    let notices = JobNotices::listen(&job)?;
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok((job, notices))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::time::Instant;

    fn system32(name: &str) -> String {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot is set on Windows");
        Path::new(&root).join("System32").join(name).display().to_string()
    }

    /// `cmd /d /c <line>`, created suspended as the broker creates every root.
    fn spawn(line: &str) -> Child {
        let mut command = Command::new(system32("cmd.exe"));
        command
            .args(["/d", "/c"])
            .raw_arg(line)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure(&mut command);
        command.spawn().unwrap()
    }

    fn ping(seconds: u32) -> String {
        format!("{} -n {} 127.0.0.1 >nul", system32("PING.EXE"), seconds + 1)
    }

    #[tokio::test]
    async fn the_root_exit_is_the_wait_on_its_handle() {
        let mut child = spawn(&ping(1));
        let tree = OwnedTree::adopt(&mut child).await.ok().unwrap();
        let began = Instant::now();
        tree.root_exit(&mut child).await.unwrap();
        assert!(began.elapsed() >= Duration::from_millis(500), "{:?}", began.elapsed());
        tree.members_settled().await.unwrap();
        assert!(!tree.has_executing_members().unwrap());
    }

    #[tokio::test]
    async fn the_job_reports_members_that_outlive_the_root() {
        // `start /b` runs ping inside the job, after the root has already exited.
        let mut child = spawn(&format!("start \"\" /b {} & exit 0", ping(2)));
        let tree = OwnedTree::adopt(&mut child).await.ok().unwrap();
        tree.root_exit(&mut child).await.unwrap();
        assert!(tree.has_executing_members().unwrap(), "a member outlives the root");
        timeout(Duration::from_secs(30), tree.members_settled())
            .await
            .expect("settled")
            .unwrap();
        assert!(!tree.has_executing_members().unwrap());
        // The member's exit was reported through the job's completion port.
        let mut exits = tree.notices.exits.clone();
        timeout(Duration::from_secs(10), exits.wait_for(|count| *count > 0))
            .await
            .expect("a job exit notice arrived")
            .unwrap();
    }

    #[tokio::test]
    async fn termination_ends_every_member_and_verifies_it() {
        let mut child = spawn(&format!("start \"\" /b {} & {}", ping(30), ping(30)));
        let tree = OwnedTree::adopt(&mut child).await.ok().unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let began = Instant::now();
        tree.terminate().await.unwrap();
        assert!(began.elapsed() < GROUP_SETTLE, "{:?}", began.elapsed());
        assert!(!tree.has_executing_members().unwrap());
        // TerminateJobObject can empty the job before its listener has read the notices
        // (the first query already finds no member); they still arrive.
        let mut exits = tree.notices.exits.clone();
        timeout(Duration::from_secs(10), exits.wait_for(|count| *count > 0))
            .await
            .expect("a job exit notice arrived")
            .unwrap();
    }
}
