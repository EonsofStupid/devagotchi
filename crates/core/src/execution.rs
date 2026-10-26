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
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::{Instant, sleep, sleep_until, timeout};

const FRAME_BYTES: usize = 1024;
const GROUP_SETTLE: Duration = Duration::from_millis(1000);
const ROOT_WAIT: Duration = Duration::from_millis(2000);
const PIPE_DRAIN: Duration = Duration::from_millis(1000);

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
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // All destructive signalling is explicit while the owned root identity is
    // held. Unexpected task/runtime failure remains Unknown; a drop handler must
    // not turn a lost-ownership condition into an unverified PID-based kill.
    platform::configure(&mut command);
    let mut child = match command.spawn() {
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
    let tree = match platform::OwnedTree::adopt(&mut child).await {
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
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let termination = tree.terminate().await;
        let _ = timeout(ROOT_WAIT, child.wait()).await;
        shared.finish(&id, OperationState::Unknown, None, false, format!("Piped output handles were missing; attempted owned-group termination: {termination:?}"));
        return;
    };
    let mut stdout_task = tokio::spawn(drain_pipe(shared.clone(), id.clone(), "stdout", stdout));
    let mut stderr_task = tokio::spawn(drain_pipe(shared.clone(), id.clone(), "stderr", stderr));
    let mut reason = None;
    let mut observation_error = None;
    loop {
        if let Some(stop) = *control.borrow() {
            reason = Some(stop);
            break;
        }
        if Instant::now() >= deadline {
            reason = Some(StopReason::Deadline);
            break;
        }
        match tree.root_exited(&child) {
            Ok(true) => match tree.has_executing_members() {
                // Keep the root unreaped while its ordinary inherited children
                // still execute. They remain owned, cancellable, and subject to
                // the same deadline even after the root program has exited.
                Ok(false) => break,
                Ok(true) => {}
                Err(error) => {
                    observation_error = Some(error.to_string());
                    reason = Some(StopReason::ObservationFailure);
                    break;
                }
            },
            Ok(false) => {}
            Err(error) => {
                observation_error = Some(error.to_string());
                break;
            }
        }
        tokio::select! {
            _ = control.changed() => {},
            _ = sleep_until(deadline) => {},
            _ = sleep(Duration::from_millis(20)) => {},
        }
    }
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
    let output_complete = match timeout(PIPE_DRAIN, async {
        tokio::join!(&mut stdout_task, &mut stderr_task)
    })
    .await
    {
        Ok((Ok(stdout), Ok(stderr))) => {
            for error in [stdout.error, stderr.error].into_iter().flatten() {
                pipe_detail.push_str(&format!(" {error}."));
            }
            stdout.complete && stderr.complete
        }
        Ok((stdout, stderr)) => {
            pipe_detail = format!(
                " Output reader did not settle normally: stdout={stdout:?}, stderr={stderr:?}."
            );
            false
        }
        Err(_) => {
            stdout_task.abort();
            stderr_task.abort();
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
