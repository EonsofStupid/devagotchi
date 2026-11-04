//! Process orchestration for one admitted operation: spawn the exact argv,
//! drain output into the broker's retained frames, and settle the owned process
//! tree on exit, cancellation, or deadline. Native mechanics live in
//! `platform`; admission and the operation lifecycle live in `crate::broker`.

use crate::broker::Shared;
use devagotchi_contracts::*;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

const FRAME_BYTES: usize = 1024;
const GROUP_SETTLE: Duration = Duration::from_millis(1000);
const ROOT_WAIT: Duration = Duration::from_millis(2000);
const PIPE_DRAIN: Duration = Duration::from_millis(1000);

pub mod detached;
pub(crate) mod platform;

/// The canonical, validated inputs of one admitted process.
#[derive(Clone)]
pub(crate) struct ExecutionSpec {
    pub(crate) executable: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) deadline_ms: u64,
    /// Run with the broker account's own environment (the `account` policy) rather than a
    /// cleared one.
    pub(crate) account_environment: bool,
    /// Keep input open, on a pseudo-terminal or a pipe.
    pub(crate) interactive: Option<InteractiveInput>,
}

/// One step of input, applied in order: bytes to the program, a signal to its group (a
/// pipe's Ctrl-C), the end of its input (a pipe's Ctrl-D), or a new terminal size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputStep {
    Bytes(Vec<u8>),
    Interrupt,
    Quit,
    Close,
    Resize { cols: u16, rows: u16 },
}

/// Steps for the running job, answered once all are applied or one fails.
pub(crate) struct InputRequest {
    pub(crate) steps: Vec<InputStep>,
    pub(crate) reply: oneshot::Sender<BridgeResult<()>>,
}

/// Where an interactive job's input goes.
enum InputSink {
    Pipe {
        stdin: Option<ChildStdin>,
        signals: platform::Signaller,
    },
    Terminal {
        io: platform::TerminalIo,
        terminal: platform::Terminal,
    },
}

fn input_failed(error: std::io::Error) -> BridgeError {
    match error.kind() {
        std::io::ErrorKind::BrokenPipe => BridgeError::new(
            "input_closed",
            "The program no longer reads its input (it closed it or exited)",
        ),
        _ => BridgeError::new("input_failed", format!("Input could not be delivered: {error}")),
    }
}

impl InputSink {
    async fn apply(&mut self, step: InputStep) -> BridgeResult<()> {
        match (self, step) {
            (Self::Pipe { stdin, .. }, InputStep::Bytes(bytes)) => {
                let Some(stdin) = stdin else {
                    return Err(BridgeError::new("input_closed", "This operation's input was closed (ctrl_d)"));
                };
                stdin.write_all(&bytes).await.map_err(input_failed)?;
                stdin.flush().await.map_err(input_failed)
            }
            (Self::Pipe { signals, .. }, InputStep::Interrupt) => signals.interrupt(),
            (Self::Pipe { signals, .. }, InputStep::Quit) => signals.quit(),
            (Self::Pipe { stdin, .. }, InputStep::Close) => {
                stdin.take();
                Ok(())
            }
            (Self::Terminal { io, .. }, InputStep::Bytes(bytes)) => {
                io.write_all(&bytes).await.map_err(input_failed)
            }
            (Self::Terminal { terminal, .. }, InputStep::Resize { cols, rows }) => terminal
                .resize(cols, rows)
                .map_err(|error| BridgeError::new("input_failed", format!("Resize failed: {error}"))),
            (_, step) => Err(BridgeError::new(
                "invalid_request",
                format!("{step:?} does not apply to this operation's input"),
            )),
        }
    }
}

/// Apply each request's steps in order, one request at a time, so two requests never
/// interleave. Ends when the broker drops its sender or the job stops it.
async fn serve_input(mut requests: mpsc::Receiver<InputRequest>, mut sink: InputSink) {
    while let Some(request) = requests.recv().await {
        let mut result = Ok(());
        for step in request.steps {
            if let Err(error) = sink.apply(step).await {
                result = Err(error);
                break;
            }
        }
        let _ = request.reply.send(result);
    }
}

/// Why the broker asked an operation's process tree to stop.
#[derive(Debug, Clone, Copy)]
pub(crate) enum StopReason {
    Cancelled,
    Deadline,
    Shutdown,
    PersistenceFailure,
    ObservationFailure,
}

/// Stateful UTF-8 decoding. Raw bytes are retained separately and never transformed.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8], eof: bool) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::new();
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(text) => {
                    output.push_str(text);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    // from_utf8 validated this exact prefix.
                    output.push_str(
                        std::str::from_utf8(&self.pending[consumed..consumed + valid])
                            .expect("validated UTF-8 prefix"),
                    );
                    consumed += valid;
                    match error.error_len() {
                        Some(invalid) => {
                            output.push('\u{fffd}');
                            consumed += invalid;
                        }
                        None if eof => {
                            output.push('\u{fffd}');
                            consumed = self.pending.len();
                        }
                        None => break,
                    }
                }
            }
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        output
    }
}

#[derive(Debug)]
struct PipeResult {
    complete: bool,
    error: Option<String>,
}

async fn drain_pipe<R: AsyncRead + Unpin>(
    shared: Arc<Shared>,
    id: String,
    stream: &'static str,
    mut pipe: R,
) -> PipeResult {
    let mut decoder = Utf8Decoder::default();
    let mut buffer = [0_u8; FRAME_BYTES];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) => {
                let text = decoder.push(&[], true);
                shared.output(&id, stream, Vec::new(), text);
                return PipeResult {
                    complete: true,
                    error: None,
                };
            }
            Ok(length) => {
                let bytes = buffer[..length].to_vec();
                let text = decoder.push(&bytes, false);
                shared.output(&id, stream, bytes, text);
            }
            Err(error) => {
                return PipeResult {
                    complete: false,
                    error: Some(format!("{stream} read failed: {error}")),
                };
            }
        }
    }
}

pub(crate) async fn run_job(
    shared: Arc<Shared>,
    id: String,
    spec: ExecutionSpec,
    mut control: watch::Receiver<Option<StopReason>>,
    deadline: Instant,
    input: Option<mpsc::Receiver<InputRequest>>,
) {
    let early_stop = *control.borrow();
    if early_stop.is_some() || Instant::now() >= deadline {
        let state = if early_stop.is_none() {
            OperationState::TimedOut
        } else {
            OperationState::Cancelled
        };
        shared.mark_stop(&id);
        shared.finish(
            &id,
            state,
            None,
            true,
            "Operation stopped after durable admission and before spawn; no process was launched."
                .into(),
        );
        return;
    }
    let mut command = Command::new(&spec.executable);
    command.args(&spec.args).current_dir(&spec.cwd);
    if !spec.account_environment {
        command.env_clear();
    }
    let mode = spec.interactive.as_ref().map(|interactive| interactive.mode);
    let mut terminal = None;
    let scope = match &spec.interactive {
        Some(InteractiveInput {
            mode: InputMode::Terminal,
            cols,
            rows,
        }) => {
            let opened = platform::Terminal::open(*cols, *rows).and_then(|(opened, program_side)| {
                platform::configure_terminal(&mut command, &program_side)?;
                Ok(opened)
            });
            match opened {
                Ok(opened) => terminal = Some(opened),
                Err(error) => {
                    shared.finish(
                        &id,
                        OperationState::Failed,
                        None,
                        true,
                        format!("The pseudo-terminal could not be set up; no process was launched: {error}"),
                    );
                    return;
                }
            }
            command.env("TERM", "xterm-256color");
            platform::Scope::Session
        }
        _ => {
            command
                .stdin(if mode == Some(InputMode::Pipe) {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // All destructive signalling is explicit while the owned root identity is
            // held. Unexpected task/runtime failure remains Unknown; a drop handler must
            // not turn a lost-ownership condition into an unverified PID-based kill.
            platform::configure(&mut command);
            platform::Scope::Group
        }
    };
    let spawned = command.spawn();
    // The broker's copies of a terminal's program side close here, so its master reads end
    // of file once no program holds that side any more.
    drop(command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            shared.finish(
                &id,
                OperationState::Failed,
                None,
                true,
                format!("Spawn failed before execution could be observed: {error}"),
            );
            return;
        }
    };
    let tree = match platform::OwnedTree::adopt(&mut child, scope).await {
        Ok(tree) => tree,
        Err(failure) => {
            shared.finish(
                &id,
                failure.state,
                None,
                failure.output_complete,
                failure.detail,
            );
            return;
        }
    };
    shared.running(&id);
    let mut readers: Vec<(&'static str, JoinHandle<PipeResult>)> = Vec::new();
    if let Some(terminal) = &terminal {
        // A terminal merges what the program writes to stdout and stderr: one stream.
        readers.push((
            "terminal",
            tokio::spawn(drain_pipe(shared.clone(), id.clone(), "stdout", terminal.io())),
        ));
    } else {
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let termination = tree.terminate().await;
            let _ = timeout(ROOT_WAIT, child.wait()).await;
            shared.finish(&id, OperationState::Unknown, None, false, format!("Piped output handles were missing; attempted owned-group termination: {termination:?}"));
            return;
        };
        readers.push(("stdout", tokio::spawn(drain_pipe(shared.clone(), id.clone(), "stdout", stdout))));
        readers.push(("stderr", tokio::spawn(drain_pipe(shared.clone(), id.clone(), "stderr", stderr))));
    }
    let sink = match (terminal, mode) {
        (Some(terminal), _) => Some(InputSink::Terminal {
            io: terminal.io(),
            terminal,
        }),
        (None, Some(InputMode::Pipe)) => Some(InputSink::Pipe {
            stdin: child.stdin.take(),
            signals: tree.signaller(),
        }),
        _ => None,
    };
    let mut input_task = match (input, sink) {
        (Some(requests), Some(sink)) => Some(tokio::spawn(serve_input(requests, sink))),
        _ => None,
    };
    let mut reason = None;
    let mut observation_error = None;
    // Sleep until something happens: a stop request, the deadline, the root's exit, then
    // (the root kept unreaped while its ordinary inherited children still execute; they
    // stay owned, cancellable and under the same deadline) the last member's exit.
    let mut root_done = false;
    let mut control_open = true;
    loop {
        if let Some(stop) = *control.borrow_and_update() {
            reason = Some(stop);
            break;
        }
        if Instant::now() >= deadline {
            reason = Some(StopReason::Deadline);
            break;
        }
        let observed = tokio::select! {
            changed = control.changed(), if control_open => {
                // A closed channel can carry no stop request; stop listening to it.
                control_open = changed.is_ok();
                continue;
            }
            _ = sleep_until(deadline) => continue,
            observed = async {
                if root_done { tree.members_settled().await } else { tree.root_exit(&mut child).await }
            } => observed,
        };
        match (observed, root_done) {
            (Ok(()), false) => root_done = true,
            (Ok(()), true) => break,
            (Err(error), false) => {
                observation_error = Some(error.to_string());
                break;
            }
            (Err(error), true) => {
                observation_error = Some(error.to_string());
                reason = Some(StopReason::ObservationFailure);
                break;
            }
        }
    }
    // Input ends here, before the root is released: a pipe's Ctrl-C signals the root's
    // group, which names it only while it is unreaped.
    if let Some(task) = input_task.take() {
        task.abort();
        let _ = task.await;
    }
    shared.input_closed(&id);
    let termination_error = if reason.is_some() {
        shared.mark_stop(&id);
        tree.terminate().await.err().map(|error| error.to_string())
    } else {
        None
    };
    // There is no tree termination after this point; wait may now release the root.
    let waited = timeout(ROOT_WAIT, child.wait()).await;
    let status: Option<ExitStatus> = match waited {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            observation_error = Some(format!("Root wait failed: {error}"));
            None
        }
        Err(_) => {
            observation_error =
                Some("Root exit was not observed within the bounded settlement wait".into());
            None
        }
    };
    let mut pipe_detail = String::new();
    let drained = timeout(PIPE_DRAIN, async {
        let mut results = Vec::new();
        for (name, reader) in &mut readers {
            results.push((*name, reader.await));
        }
        results
    })
    .await;
    let output_complete = match drained {
        Ok(results) => {
            let mut complete = true;
            for (name, result) in results {
                match result {
                    Ok(result) => {
                        if let Some(error) = result.error {
                            pipe_detail.push_str(&format!(" {error}."));
                        }
                        complete &= result.complete;
                    }
                    Err(error) => {
                        pipe_detail.push_str(&format!(
                            " Output reader did not settle normally: {name}={error:?}."
                        ));
                        complete = false;
                    }
                }
            }
            complete
        }
        Err(_) => {
            for (_, reader) in &readers {
                reader.abort();
            }
            pipe_detail = platform::PIPE_TIMEOUT_DETAIL.into();
            false
        }
    };
    let exit_code = status.as_ref().and_then(ExitStatus::code);
    let state = if observation_error.is_some() || termination_error.is_some() {
        OperationState::Unknown
    } else {
        match reason {
            Some(StopReason::Deadline) => OperationState::TimedOut,
            Some(StopReason::PersistenceFailure | StopReason::ObservationFailure) => {
                OperationState::Unknown
            }
            Some(StopReason::Cancelled | StopReason::Shutdown) => OperationState::Cancelled,
            None if status.as_ref().is_some_and(ExitStatus::success) => OperationState::Succeeded,
            None => OperationState::Failed,
        }
    };
    let mut detail = match reason {
        Some(StopReason::Deadline) => {
            "Execution deadline reached; cancellation was recorded. ".to_owned()
        }
        Some(StopReason::Cancelled) => "Cancellation was recorded. ".to_owned(),
        Some(StopReason::Shutdown) => "Broker shutdown requested cancellation. ".to_owned(),
        Some(StopReason::PersistenceFailure) => {
            "Critical persistence failure requested cancellation. ".to_owned()
        }
        Some(StopReason::ObservationFailure) => platform::OBSERVATION_FAILED_DETAIL.to_owned(),
        None => String::new(),
    };
    if let Some(status) = status {
        detail.push_str(&format!("Root exit observed: {status}."));
    }
    if reason.is_some() && termination_error.is_none() {
        detail.push_str(platform::TERMINATION_VERIFIED_DETAIL);
    }
    if let Some(error) = observation_error {
        detail.push_str(&format!(" {error}."));
    }
    if let Some(error) = termination_error {
        detail.push_str(&format!(
            "{}: {error}.",
            platform::TERMINATION_UNVERIFIED_DETAIL
        ));
    }
    detail.push_str(&pipe_detail);
    shared.finish(&id, state, exit_code, output_complete, detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_is_identical_across_every_split_boundary() {
        let original = "ASCII € 🐄 日本語\n";
        for first in 0..=original.len() {
            for second in first..=original.len() {
                let mut decoder = Utf8Decoder::default();
                let bytes = original.as_bytes();
                let mut result = decoder.push(&bytes[..first], false);
                result.push_str(&decoder.push(&bytes[first..second], false));
                result.push_str(&decoder.push(&bytes[second..], true));
                assert_eq!(result, original, "split at {first}/{second}");
                assert!(decoder.pending.is_empty());
            }
        }
    }

    #[test]
    fn utf8_invalid_and_truncated_sequences_are_explicit() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.push(&[0xe2], false), "");
        assert_eq!(decoder.push(&[0x82], false), "");
        assert_eq!(decoder.push(&[], true), "�");
        assert_eq!(decoder.push(&[0xff, 0, b'x'], true), "�\0x");
    }
}
