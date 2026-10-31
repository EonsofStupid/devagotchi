//! `run`: one call that starts a command, waits a bounded time for it, and returns its
//! output, over whatever reaches the broker (this machine's local IPC, or the hub's link to
//! a machine). It is the broker's own operation underneath, so a command that outlasts the
//! wait keeps running and stays observable with the operation tools; nothing here owns a job.

use std::future::Future;

use devagotchi_contracts::{
    BridgeError, MAX_OUTPUT_PAGE_BYTES, OperationState, ReadInput, RequestBody, ResponseBody,
    StartInput, WaitInput,
};

use crate::unexpected;

/// Default and largest wait for a result. MCP clients give up on a call after about a
/// minute, so the wait stays under it; the command itself may run longer.
pub const DEFAULT_WAIT_S: u64 = 30;
pub const MAX_WAIT_S: u64 = 50;
/// Default and largest run time before the broker stops the command.
pub const DEFAULT_TIMEOUT_S: u64 = 300;
pub const MAX_TIMEOUT_S: u64 = 300;
/// Output handed back in one answer: the end of it, which is where results and errors are.
pub const OUTPUT_LIMIT: usize = 32 * 1024;

/// What a run came to.
#[derive(Debug)]
pub struct Ran {
    pub operation_id: String,
    pub state: OperationState,
    pub exit_code: Option<i32>,
    /// stdout and stderr together, in the order written; at most [`OUTPUT_LIMIT`] bytes.
    pub output: String,
    /// The start of the output was cut, here or by the broker's retention.
    pub truncated: bool,
    /// The cursor after the last output read: where to follow a command still running
    /// without seeing that output again.
    pub cursor: u64,
}

/// Why a run did not come back with output.
#[derive(Debug)]
pub enum Failed {
    /// Nothing was started.
    NotStarted(BridgeError),
    /// The command was started; then this went wrong. It may still be running.
    Started {
        operation_id: String,
        error: BridgeError,
    },
    /// The caller stopped waiting. The command keeps running.
    Cancelled { operation_id: String },
}

/// `value` within 1..=`max`, or `default` when absent.
pub fn bounded(value: Option<u64>, default: u64, max: u64, name: &str) -> Result<u64, BridgeError> {
    match value.unwrap_or(default) {
        n if (1..=max).contains(&n) => Ok(n),
        _ => Err(BridgeError::new(
            "invalid_request",
            format!("{name} must be 1..={max}"),
        )),
    }
}

/// Keep the last `limit` bytes of `text`, cut at a character boundary.
pub fn tail(text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), true)
}

/// Start `start` through `call`, wait up to `wait_s` seconds for it to finish (one held
/// request: the broker answers the moment it does), then read its output. `cancelled`
/// resolving during the wait stops waiting and leaves the command running.
pub async fn run<C, F>(
    call: C,
    start: StartInput,
    wait_s: u64,
    cancelled: impl Future<Output = ()>,
) -> Result<Ran, Failed>
where
    C: Fn(RequestBody) -> F,
    F: Future<Output = Result<ResponseBody, BridgeError>>,
{
    let operation = match call(RequestBody::OperationStart(start)).await {
        Ok(ResponseBody::Start(output)) => output.operation,
        Ok(other) => return Err(Failed::NotStarted(unexpected("start", &other))),
        Err(error) => return Err(Failed::NotStarted(error)),
    };
    let id = operation.operation_id;
    let started = |error| Failed::Started {
        operation_id: id.clone(),
        error,
    };
    let mut state = operation.state;
    let mut exit_code = operation.exit_code;
    if !state.terminal() {
        let wait = call(RequestBody::OperationWait(WaitInput {
            operation_id: id.clone(),
            output_after: None,
            timeout_ms: wait_s * 1000,
        }));
        let view = tokio::select! {
            biased;
            _ = cancelled => return Err(Failed::Cancelled { operation_id: id.clone() }),
            view = wait => view,
        };
        match view {
            Ok(ResponseBody::Operation(view)) => {
                state = view.state;
                exit_code = view.exit_code;
            }
            Ok(other) => return Err(started(unexpected("operation", &other))),
            Err(error) => return Err(started(error)),
        }
    }
    let mut text = String::new();
    let mut gap = false;
    let mut cursor = 0;
    let mut follow;
    loop {
        let page = match call(RequestBody::OperationRead(ReadInput {
            operation_id: id.clone(),
            cursor,
            max_bytes: MAX_OUTPUT_PAGE_BYTES,
        }))
        .await
        {
            Ok(ResponseBody::Output(page)) => page,
            Ok(other) => return Err(started(unexpected("output", &other))),
            Err(error) => return Err(started(error)),
        };
        gap |= page.gap;
        follow = page.next_cursor;
        for frame in &page.frames {
            text.push_str(&frame.text);
        }
        if text.len() > 4 * OUTPUT_LIMIT {
            text = tail(text, OUTPUT_LIMIT).0;
            gap = true;
        }
        if !page.has_more || page.next_cursor == cursor {
            break;
        }
        cursor = page.next_cursor;
    }
    let (output, cut) = tail(text, OUTPUT_LIMIT);
    Ok(Ran {
        operation_id: id,
        state,
        exit_code,
        output,
        truncated: cut || gap,
        cursor: follow,
    })
}

#[cfg(test)]
mod tests;
