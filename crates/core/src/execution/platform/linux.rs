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
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
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
/// An operation on a pseudo-terminal: its root leads a new session, and job control may put
/// its children in process groups of their own, so the session is what is owned.
pub(crate) const SCOPE_TERMINAL: &str = "Linux root process and the session it leads on a broker-owned pseudo-terminal, every process group in it included; excludes processes that start a session of their own. This is not OS containment.";
pub(crate) const RUNNING_DETAIL: &str = "Root spawned with exact argv, cleared environment, null stdin, and a new Linux process group. Executable behavior is not sandboxed.";
pub(crate) const TERMINATION_VERIFIED_DETAIL: &str = " SIGTERM then SIGKILL were issued while the root PID was still owned; no executing members remained in its inherited process group.";
pub(crate) const OBSERVATION_FAILED_DETAIL: &str =
    "Process-group observation failed and requested cancellation. ";
pub(crate) const TERMINATION_UNVERIFIED_DETAIL: &str = " Group termination was not verified";
pub(crate) const PIPE_TIMEOUT_DETAIL: &str = " Output pipes did not close within one second of root settlement; a descendant may still hold them open. Reading was stopped, output is incomplete, and processes outside the owned group are not tracked or terminated.";
pub(crate) const RUNNING_DETAIL_ACCOUNT: &str = "Root spawned as the broker's account with its environment, null stdin, and a new Linux process group. Executable behavior is not sandboxed.";

/// Both input modes are available on Linux.
pub(crate) fn supports_interactive(_mode: devagotchi_contracts::InputMode) -> bool {
    true
}

/// What a running operation's detail says: who it runs as, and where its input comes from.
pub(crate) fn running_detail(account: bool, mode: Option<devagotchi_contracts::InputMode>) -> String {
    use devagotchi_contracts::InputMode;
    let who = if account {
        "as the broker's account with its environment"
    } else {
        "with exact argv and a cleared environment"
    };
    match mode {
        None if account => RUNNING_DETAIL_ACCOUNT.into(),
        None => RUNNING_DETAIL.into(),
        Some(InputMode::Pipe) => format!("Root spawned {who}, stdin a broker-held pipe for operation_input, and a new Linux process group. Executable behavior is not sandboxed."),
        Some(InputMode::Terminal) => format!("Root spawned {who} plus TERM=xterm-256color, leading a new session whose controlling terminal is a broker-owned pseudo-terminal (stdin, stdout and stderr). Executable behavior is not sandboxed."),
    }
}
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

/// What an owned tree is: the root's process group, or (on a pseudo-terminal) the session
/// it leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Group,
    Session,
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
    scope: Scope,
}

impl OwnedTree {
    pub(crate) async fn adopt(child: &mut Child, scope: Scope) -> Result<Self, AdoptFailure> {
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
        Ok(Self { pid, root, scope })
    }

    /// Signals the root's process group on the operation's behalf (Ctrl-C on a pipe). Only
    /// while the root is unreaped, which the job guarantees by stopping input first.
    pub(crate) fn signaller(&self) -> Signaller {
        Signaller(self.pid)
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
            let members = members(self.pid, self.scope)?;
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
                if member_state(member, self.pid, self.scope)? {
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
        let term_error = self.signal_all(libc::SIGTERM).err();
        let _ = timeout(TERM_GRACE, self.members_settled()).await;
        let kill_error = self.signal_all(libc::SIGKILL).err();
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

    /// Every owned member: the group with one call, a session member by member (job control
    /// gives a terminal's pipelines groups of their own). A member is signalled through a
    /// pidfd opened and then checked to be a member, so a number reused in between is never
    /// signalled; without pidfds, by number right after the check.
    fn signal_all(&self, signal: i32) -> BridgeResult<()> {
        if self.scope == Scope::Group {
            return signal_group(self.pid, signal);
        }
        for member in members(self.pid, self.scope)? {
            match pidfd(member) {
                Ok(Some(fd)) => {
                    if !member_state(member, self.pid, self.scope)? {
                        continue;
                    }
                    // SAFETY: the pidfd is open and names exactly that process.
                    let sent = unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            fd.get_ref().as_raw_fd(),
                            signal,
                            std::ptr::null::<libc::siginfo_t>(),
                            0u32,
                        )
                    };
                    if sent == -1 {
                        let error = io::Error::last_os_error();
                        if error.raw_os_error() != Some(libc::ESRCH) {
                            return Err(BridgeError::new(
                                "process_signal_error",
                                format!("Cannot signal an owned session member: {error}"),
                            ));
                        }
                    }
                }
                Ok(None) => {}
                Err(error) if pidfd_unsupported(&error) => {
                    if member_state(member, self.pid, self.scope)? {
                        // SAFETY: plain kill(2) of a member just checked to be one.
                        unsafe { libc::kill(member as libc::pid_t, signal) };
                    }
                }
                Err(error) => return Err(observation(error)),
            }
        }
        Ok(())
    }
}

/// Signals an operation's root group for its input (Ctrl-C and Ctrl-\\ on a pipe).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Signaller(u32);

impl Signaller {
    pub(crate) fn interrupt(&self) -> BridgeResult<()> {
        signal_group(self.0, libc::SIGINT)
    }

    pub(crate) fn quit(&self) -> BridgeResult<()> {
        signal_group(self.0, libc::SIGQUIT)
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
#[cfg(test)]
fn group_members(group: u32) -> BridgeResult<Vec<u32>> {
    members(group, Scope::Group)
}

/// The executing members of the root's group or session, read from /proc while the root
/// remains unreaped.
fn members(root: u32, scope: Scope) -> BridgeResult<Vec<u32>> {
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
        if member_state(pid, root, scope)? {
            members.push(pid);
        }
    }
    Ok(members)
}

/// Whether `pid` is an executing member of the root's group (or session) now; a process
/// that is gone is not.
fn member_state(pid: u32, root: u32, scope: Scope) -> BridgeResult<bool> {
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
    // state, ppid, pgrp, session
    let fields: Vec<_> = stat[end + 1..].split_whitespace().take(4).collect();
    if fields.len() != 4 {
        return Err(BridgeError::new(
            "process_observation_error",
            "Incomplete /proc process stat",
        ));
    }
    let owner = match scope {
        Scope::Group => fields[2],
        Scope::Session => fields[3],
    };
    Ok(owner.parse::<u32>().ok() == Some(root) && fields[0] != "Z" && fields[0] != "X")
}

/// The broker's side (the master) of a pseudo-terminal an operation runs on.
pub(crate) struct Terminal {
    master: Arc<AsyncFd<std::fs::File>>,
}

impl Terminal {
    /// A new pseudo-terminal of `cols` by `rows`, and its program side for the root.
    pub(crate) fn open(cols: u16, rows: u16) -> io::Result<(Self, OwnedFd)> {
        use std::os::unix::fs::OpenOptionsExt;
        // std opens with O_CLOEXEC: no program inherits the master.
        let master = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open("/dev/ptmx")?;
        let fd = master.as_raw_fd();
        // SAFETY: `fd` is the open ptmx descriptor `master` owns.
        if unsafe { libc::grantpt(fd) } != 0 || unsafe { libc::unlockpt(fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: the buffer and its length are this array's.
        let failed = unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) };
        if failed != 0 {
            return Err(io::Error::from_raw_os_error(failed));
        }
        // SAFETY: ptsname_r succeeded, so it wrote a NUL-terminated name into the buffer.
        let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
        let program_side = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(std::ffi::OsStr::from_bytes(path.to_bytes()))?;
        set_size(fd, cols, rows)?;
        // SAFETY: F_GETFL and F_SETFL on a descriptor `master` owns.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `master` is an open File the AsyncFd owns from here on; its descriptor stays
        // open and unchanged until the AsyncFd drops it.
        let master = unsafe { AsyncFd::register(master) }.map_err(io::Error::from)?;
        Ok((
            Self {
                master: Arc::new(master),
            },
            program_side.into(),
        ))
    }

    /// Reads what the program writes; writes what is typed to it.
    pub(crate) fn io(&self) -> TerminalIo {
        TerminalIo(self.master.clone())
    }

    /// The program's foreground process group gets SIGWINCH from the kernel.
    pub(crate) fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        set_size(self.master.get_ref().as_raw_fd(), cols, rows)
    }
}

fn set_size(fd: i32, cols: u16, rows: u16) -> io::Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ reads one winsize through the pointer, which outlives the call.
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &raw const size) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The master side, as a stream. Once every program side has closed, a read fails with
/// EIO on Linux: that is the end of the program's output, read here as end of file.
pub(crate) struct TerminalIo(Arc<AsyncFd<std::fs::File>>);

impl AsyncRead for TerminalIo {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(context))?;
            let unfilled = buffer.initialize_unfilled();
            match guard.try_io(|master| {
                let mut file: &std::fs::File = master.get_ref();
                file.read(unfilled)
            }) {
                Ok(Ok(length)) => {
                    buffer.advance(length);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => {
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for TerminalIo {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(context))?;
            match guard.try_io(|master| {
                let mut file: &std::fs::File = master.get_ref();
                file.write(bytes)
            }) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Puts the root on the terminal: its stdin, stdout and stderr are the program side, and
/// it leads a new session whose controlling terminal that is.
pub(crate) fn configure_terminal(command: &mut Command, program_side: &OwnedFd) -> io::Result<()> {
    command
        .stdin(Stdio::from(program_side.try_clone()?))
        .stdout(Stdio::from(program_side.try_clone()?))
        .stderr(Stdio::from(program_side.try_clone()?));
    // SAFETY: between fork and exec this calls only async-signal-safe setsid and ioctl, and
    // allocates nothing.
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY, 0 as libc::c_int) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
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
        let tree = OwnedTree::adopt(child, Scope::Group).await.ok().unwrap();
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

/// A process started by [`spawn_detached`].
pub(crate) struct DetachedChild(std::process::Child);

impl DetachedChild {
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        use std::os::unix::process::ExitStatusExt;
        Ok(self
            .0
            .try_wait()?
            .map(|status| status.code().unwrap_or(128 + status.signal().unwrap_or(0))))
    }
}

/// Start a process in a session of its own, with only its stdin (the null device) and its
/// log open: every other descriptor the caller had, an AI client's pipes among them, is
/// closed when it starts (`detached.rs`).
pub(crate) fn spawn_detached(
    spec: &crate::execution::detached::DetachedSpec<'_>,
) -> std::io::Result<DetachedChild> {
    use std::os::unix::fs::OpenOptionsExt;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(spec.log)?;
    // An existing log keeps its mode on open; it is the user's alone either way.
    log.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let mut command = std::process::Command::new(spec.program);
    command
        .args(spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(spec.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: between fork and exec this calls only async-signal-safe functions
    // (setsid, syscall, sysconf, fcntl) and allocates nothing.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Everything above stderr closes on exec. CLOSE_RANGE_CLOEXEC (Linux 5.11+)
            // marks them in one call; older kernels get the same, one descriptor at a time.
            let marked = libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC as u32,
            );
            if marked != 0 {
                let max = match libc::sysconf(libc::_SC_OPEN_MAX) {
                    n if n > 0 => n.min(65_536) as libc::c_int,
                    _ => 4096,
                };
                for fd in 3..max {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }
    command.spawn().map(DetachedChild)
}

#[cfg(test)]
mod detached_tests {
    use super::*;
    use crate::execution::detached::DetachedSpec;
    use std::{ffi::OsString, os::fd::AsRawFd};

    #[test]
    fn a_detached_process_holds_nothing_of_its_callers_and_has_a_session_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        // A descriptor the caller left inheritable, as an AI client's pipe to it may be.
        let extra = std::fs::File::open("/dev/null").unwrap();
        let fd = extra.as_raw_fd();
        // SAFETY: clears close-on-exec on a descriptor this test owns.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        let log = dir.path().join("log");
        let script = format!(
            "[ -e /proc/self/fd/{fd} ] && echo leaked || echo closed; cut -d' ' -f6 /proc/$$/stat; echo \"secret=${{SECRET:-none}}\"; pwd"
        );
        let env = vec![(
            OsString::from("PATH"),
            std::env::var_os("PATH").unwrap_or_default(),
        )];
        let args = [OsString::from("-c"), OsString::from(script)];
        let mut child = spawn_detached(&DetachedSpec {
            program: Path::new("/bin/sh"),
            args: &args,
            env: &env,
            cwd: dir.path(),
            log: &log,
        })
        .unwrap();
        let began = std::time::Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(began.elapsed() < Duration::from_secs(10), "never exited");
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = out.lines().map(str::trim).collect();
        assert_eq!(lines.len(), 4, "{out}");
        assert_eq!(lines[0], "closed", "the caller's descriptor reached it: {out}");
        // SAFETY: getsid(0) reads this process's own session id.
        let ours = unsafe { libc::getsid(0) };
        assert_ne!(lines[1], ours.to_string(), "it shares the caller's session: {out}");
        assert_eq!(lines[2], "secret=none", "only the given environment: {out}");
        assert_eq!(
            lines[3],
            std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap()
        );
        let mode = std::fs::metadata(&log).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        drop(extra);
    }
}
