use std::{collections::VecDeque, future::Future, pin::Pin, sync::Mutex};

use devagotchi_contracts::{OperationView, OutputFrame, OutputPage, StartOutput};

use super::*;

/// A broker that answers from a script, and remembers what it was asked.
#[derive(Default)]
struct Script {
    replies: Mutex<VecDeque<Option<Result<ResponseBody, BridgeError>>>>,
    asked: Mutex<Vec<String>>,
}

type Reply = Pin<Box<dyn Future<Output = Result<ResponseBody, BridgeError>> + Send>>;

impl Script {
    fn new(replies: Vec<Option<Result<ResponseBody, BridgeError>>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            asked: Mutex::default(),
        }
    }

    /// `None` in the script is a reply that never comes.
    fn call(&self, body: RequestBody) -> Reply {
        let method = serde_json::to_value(&body).unwrap()["method"]
            .as_str()
            .unwrap()
            .to_owned();
        self.asked.lock().unwrap().push(method);
        match self.replies.lock().unwrap().pop_front() {
            Some(Some(reply)) => Box::pin(async move { reply }),
            Some(None) => Box::pin(std::future::pending()),
            None => panic!("asked more than the script holds"),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

fn view(state: OperationState, exit_code: Option<i32>) -> OperationView {
    OperationView {
        operation_id: "op-1".into(),
        executable: String::new(),
        args: Vec::new(),
        cwd: "/w".into(),
        deadline_ms: 300_000,
        state,
        created_at_ms: 0,
        updated_at_ms: 0,
        exit_code,
        detail: None,
        ownership_scope: "process_group".into(),
        cancellation_requested: false,
        output_complete: true,
    }
}

fn started(state: OperationState) -> Option<Result<ResponseBody, BridgeError>> {
    Some(Ok(ResponseBody::Start(StartOutput {
        operation: view(state, None),
        replayed: false,
    })))
}

fn waited(state: OperationState, exit_code: Option<i32>) -> Option<Result<ResponseBody, BridgeError>> {
    Some(Ok(ResponseBody::Operation(view(state, exit_code))))
}

fn page(texts: &[&str], next_cursor: u64, has_more: bool, gap: bool) -> Option<Result<ResponseBody, BridgeError>> {
    Some(Ok(ResponseBody::Output(OutputPage {
        operation_id: "op-1".into(),
        frames: texts
            .iter()
            .enumerate()
            .map(|(i, text)| OutputFrame {
                sequence: i as u64 + 1,
                stream: "stdout".into(),
                bytes_base64: String::new(),
                text: (*text).into(),
            })
            .collect(),
        next_cursor,
        gap,
        earliest_retained_sequence: 1,
        latest_sequence: next_cursor,
        has_more,
        operation: view(OperationState::Succeeded, Some(0)),
    })))
}

fn start_input() -> StartInput {
    StartInput {
        idempotency_key: "run-test".into(),
        executable: String::new(),
        args: Vec::new(),
        command: Some("echo hi".into()),
        cwd: None,
        deadline_ms: 300_000,
    }
}

fn never() -> std::future::Pending<()> {
    std::future::pending()
}

#[tokio::test]
async fn a_command_that_finishes_comes_back_with_all_its_output_in_order() {
    let script = Script::new(vec![
        started(OperationState::Running),
        waited(OperationState::Succeeded, Some(0)),
        page(&["one ", "two "], 2, true, false),
        page(&["three"], 3, false, false),
    ]);
    let ran = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap();
    assert!(matches!(ran.state, OperationState::Succeeded));
    assert_eq!(ran.exit_code, Some(0));
    assert_eq!(ran.output, "one two three");
    assert!(!ran.truncated);
    assert_eq!(
        script.asked(),
        ["operation_start", "operation_wait", "operation_read", "operation_read"]
    );
}

#[tokio::test]
async fn a_command_already_done_at_start_is_not_waited_for() {
    let script = Script::new(vec![
        started(OperationState::Failed),
        page(&["boom"], 1, false, false),
    ]);
    let ran = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap();
    assert!(matches!(ran.state, OperationState::Failed));
    assert_eq!(script.asked(), ["operation_start", "operation_read"]);
}

#[tokio::test]
async fn a_command_still_running_after_the_wait_says_where_to_follow_it() {
    let script = Script::new(vec![
        started(OperationState::Running),
        waited(OperationState::Running, None),
        page(&["building…"], 7, false, false),
    ]);
    let ran = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap();
    assert!(matches!(ran.state, OperationState::Running));
    assert_eq!(ran.operation_id, "op-1");
    assert_eq!(ran.output, "building…");
    assert_eq!(ran.cursor, 7, "follow on after the output already returned");
}

#[tokio::test]
async fn stopping_the_wait_leaves_the_command_running_and_names_it() {
    let script = Script::new(vec![started(OperationState::Running), None]);
    let failed = run(
        |body| script.call(body),
        start_input(),
        30,
        std::future::ready(()),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&failed, Failed::Cancelled { operation_id } if operation_id == "op-1"),
        "{failed:?}"
    );
    assert_eq!(script.asked(), ["operation_start", "operation_wait"]);
}

#[tokio::test]
async fn a_refused_start_started_nothing_and_a_later_failure_names_the_operation() {
    let refused = BridgeError::new("command_not_allowed", "granted policy");
    let script = Script::new(vec![Some(Err(refused))]);
    let failed = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap_err();
    assert!(
        matches!(&failed, Failed::NotStarted(error) if error.code == "command_not_allowed"),
        "{failed:?}"
    );

    let script = Script::new(vec![
        started(OperationState::Succeeded),
        Some(Err(BridgeError::new("ipc_io", "broker went away"))),
    ]);
    let failed = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap_err();
    assert!(
        matches!(&failed, Failed::Started { operation_id, error } if operation_id == "op-1" && error.code == "ipc_io"),
        "{failed:?}"
    );

    let script = Script::new(vec![waited(OperationState::Running, None)]);
    let failed = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap_err();
    assert!(
        matches!(&failed, Failed::NotStarted(error) if error.code == "unexpected_broker_response"),
        "{failed:?}"
    );
}

#[tokio::test]
async fn long_output_keeps_its_end_and_says_so() {
    let long = "x".repeat(OUTPUT_LIMIT) + "the end";
    let script = Script::new(vec![
        started(OperationState::Succeeded),
        page(&[long.as_str()], 1, false, false),
    ]);
    let ran = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap();
    assert!(ran.truncated);
    assert_eq!(ran.output.len(), OUTPUT_LIMIT);
    assert!(ran.output.ends_with("the end"));

    // Output the broker no longer kept is a cut too.
    let script = Script::new(vec![
        started(OperationState::Succeeded),
        page(&["later lines"], 9, false, true),
    ]);
    let ran = run(|body| script.call(body), start_input(), 30, never())
        .await
        .unwrap();
    assert!(ran.truncated);
}

#[test]
fn the_tail_keeps_the_end_on_a_character_boundary() {
    assert_eq!(tail("short".into(), 10), ("short".into(), false));
    let (kept, cut) = tail("abcdef".into(), 3);
    assert_eq!((kept.as_str(), cut), ("def", true));
    // "é" is two bytes: a cut inside it moves forward to the next boundary.
    let (kept, cut) = tail("aébc".into(), 3);
    assert!(cut);
    assert_eq!(kept, "bc");
}

#[test]
fn bounds_refuse_zero_and_too_much() {
    assert_eq!(bounded(None, 30, 50, "wait_seconds").unwrap(), 30);
    assert_eq!(bounded(Some(50), 30, 50, "wait_seconds").unwrap(), 50);
    assert!(bounded(Some(0), 30, 50, "wait_seconds").is_err());
    assert!(bounded(Some(51), 30, 50, "wait_seconds").is_err());
}
