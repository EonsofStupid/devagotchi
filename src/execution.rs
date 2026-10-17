//! Linux execution proof: durable admissions, independent output cursors, and
//! owned process-group cancellation. Executable grants are not a sandbox.

use crate::contracts::*;
use crate::journal::{Journal, JournalFrame, JournalOperation};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{Notify, watch};
use tokio::time::{Instant, sleep, sleep_until, timeout};

const FRAME_BYTES: usize = 1024;
const MAX_FRAMES: usize = 1024;
const MAX_PAGE_FRAMES: usize = 64;
const TERM_GRACE: Duration = Duration::from_millis(200);
const GROUP_SETTLE: Duration = Duration::from_millis(1000);
const ROOT_WAIT: Duration = Duration::from_millis(2000);
const PIPE_DRAIN: Duration = Duration::from_millis(1000);
const SCOPE: &str = "Linux root process and inherited process group; excludes processes that leave the group. This is not OS containment.";

fn default_output_limit() -> usize {
    262_144
}
fn default_max_operations() -> usize {
    64
}
fn default_max_concurrent() -> usize {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    pub workspace: String,
    pub allowed_executables: Vec<String>,
    #[serde(default = "default_output_limit")]
    pub output_limit_bytes: usize,
    #[serde(default = "default_max_operations")]
    pub max_operations: usize,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

#[derive(Clone)]
struct ExecutionSpec {
    executable: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    deadline_ms: u64,
}

#[derive(Debug, Clone, Copy)]
enum StopReason {
    Cancelled,
    Deadline,
    Shutdown,
    PersistenceFailure,
    ObservationFailure,
}

struct Operation {
    record: JournalOperation,
    retained_bytes: usize,
    active: bool,
    control: Option<watch::Sender<Option<StopReason>>>,
    persistence_fault: Option<String>,
}

struct State {
    journal: Journal,
    operations: HashMap<String, Operation>,
    keys: HashMap<String, String>,
    active_count: usize,
    closing: bool,
    persistence_fault: Option<BridgeError>,
}

struct Shared {
    config: BrokerConfig,
    workspace: PathBuf,
    executables: HashSet<PathBuf>,
    config_identity: String,
    state: Mutex<State>,
    settlement: Notify,
}

pub struct Broker {
    shared: Arc<Shared>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn invalid(message: impl Into<String>) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn canonical_existing(value: &str, kind: &str, code: &str) -> BridgeResult<PathBuf> {
    let path = Path::new(value);
    if !path.is_absolute() || value.as_bytes().contains(&0) {
        return Err(BridgeError::new(
            code,
            format!("{kind} must be an absolute path without NUL bytes"),
        ));
    }
    path.canonicalize()
        .map_err(|error| BridgeError::new(code, format!("Cannot canonicalize {kind}: {error}")))
}

fn path_string(path: &Path) -> BridgeResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Canonical paths must be representable as UTF-8"))
}

impl Broker {
    pub fn open(config: BrokerConfig, state_dir: &Path) -> BridgeResult<Arc<Self>> {
        if !cfg!(target_os = "linux") {
            return Err(BridgeError::new(
                "unsupported_platform",
                "This proof supports Linux only",
            ));
        }
        if !(1024..=16 * 1024 * 1024).contains(&config.output_limit_bytes)
            || !(1..=4096).contains(&config.max_operations)
            || !(1..=64).contains(&config.max_concurrent)
            || config.max_concurrent > config.max_operations
            || config.allowed_executables.len() > 128
        {
            return Err(BridgeError::new(
                "invalid_config",
                "output_limit_bytes must be 1024..=16777216, max_operations 1..=4096, max_concurrent 1..=64 and no greater than max_operations; at most 128 executable grants",
            ));
        }
        let workspace = canonical_existing(&config.workspace, "workspace", "invalid_config")?;
        if !workspace.is_dir() {
            return Err(BridgeError::new(
                "invalid_config",
                "Workspace must be an existing directory",
            ));
        }
        let mut executables = HashSet::new();
        for grant in &config.allowed_executables {
            let executable = canonical_existing(grant, "executable grant", "invalid_config")?;
            let metadata = executable
                .metadata()
                .map_err(|error| BridgeError::new("invalid_config", error.to_string()))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(BridgeError::new(
                    "invalid_config",
                    "Each executable grant must resolve to an executable regular file",
                ));
            }
            executables.insert(executable);
        }
        let mut grants = executables
            .iter()
            .map(|path| path_string(path))
            .collect::<BridgeResult<Vec<_>>>()?;
        grants.sort();
        let identity_bytes = serde_json::to_vec(&(
            "desktop-bridge-execution-v1",
            path_string(&workspace)?,
            grants,
            config.output_limit_bytes,
            config.max_operations,
            config.max_concurrent,
        ))
        .map_err(|error| BridgeError::new("invalid_config", error.to_string()))?;
        let config_identity = hex::encode(Sha256::digest(identity_bytes));
        let mut journal = Journal::open(state_dir)?;
        let recovered = journal.load()?;
        let mut operations = HashMap::new();
        let mut keys = HashMap::new();
        for mut record in recovered {
            if !record.view.state.terminal() {
                record.view.state = OperationState::Unknown;
                record.view.updated_at_ms = now_ms();
                record.view.output_complete = false;
                record.view.detail = Some("Broker restarted before settlement. Execution effects and process/resource status are unknown; processes may still be running. Admission does not prove whether spawning occurred. This operation will never be replayed automatically, and no recovered PID will be signalled.".into());
                journal.update(&record.view)?;
            }
            let mut retained_bytes: usize =
                record.frames.iter().map(|frame| frame.bytes.len()).sum();
            while retained_bytes > config.output_limit_bytes || record.frames.len() > MAX_FRAMES {
                if let Some(frame) = record.frames.pop_front() {
                    retained_bytes -= frame.bytes.len();
                }
            }
            let earliest = record
                .frames
                .front()
                .map_or(record.latest_sequence + 1, |frame| frame.sequence);
            journal.prune(&record.view.operation_id, earliest)?;
            keys.insert(record.key.clone(), record.view.operation_id.clone());
            operations.insert(
                record.view.operation_id.clone(),
                Operation {
                    record,
                    retained_bytes,
                    active: false,
                    control: None,
                    persistence_fault: None,
                },
            );
        }
        Ok(Arc::new(Self {
            shared: Arc::new(Shared {
                config,
                workspace,
                executables,
                config_identity,
                state: Mutex::new(State {
                    journal,
                    operations,
                    keys,
                    active_count: 0,
                    closing: false,
                    persistence_fault: None,
                }),
                settlement: Notify::new(),
            }),
        }))
    }

    pub async fn handle(&self, request: RequestBody) -> BridgeResult<ResponseBody> {
        match request {
            RequestBody::OperationStart(input) => self.start(input).map(ResponseBody::Start),
            RequestBody::OperationGet(input) => {
                self.get(&input.operation_id).map(ResponseBody::Operation)
            }
            RequestBody::OperationRead(input) => self.read(input).map(ResponseBody::Output),
            RequestBody::OperationCancel(input) => self
                .cancel(&input.operation_id)
                .map(ResponseBody::Operation),
            RequestBody::OperationList(input) => self.list(input).map(ResponseBody::Operations),
        }
    }

    fn normalize(&self, input: &StartInput) -> BridgeResult<(ExecutionSpec, String)> {
        if !(1..=128).contains(&input.idempotency_key.len()) {
            return Err(invalid("idempotency_key must contain 1..=128 UTF-8 bytes"));
        }
        if input.args.len() > 128
            || input.args.iter().any(|arg| arg.as_bytes().contains(&0))
            || input.args.iter().map(String::len).sum::<usize>() > 16 * 1024
        {
            return Err(invalid(
                "args must contain at most 128 entries and 16384 total UTF-8 bytes, without NUL bytes",
            ));
        }
        if !(1..=300_000).contains(&input.deadline_ms) {
            return Err(invalid("deadline_ms must be 1..=300000"));
        }
        let executable = canonical_existing(&input.executable, "executable", "invalid_request")?;
        if !self.shared.executables.contains(&executable) {
            return Err(BridgeError::new(
                "executable_not_allowed",
                "The canonical executable path has no operator grant",
            ));
        }
        let metadata = executable
            .metadata()
            .map_err(|error| invalid(error.to_string()))?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(invalid("Executable is not an executable regular file"));
        }
        let cwd = match &input.cwd {
            Some(cwd) => canonical_existing(cwd, "cwd", "invalid_request")?,
            None => self.shared.workspace.clone(),
        };
        if !cwd.is_dir() {
            return Err(invalid("cwd must be an existing directory"));
        }
        if !cwd.starts_with(&self.shared.workspace) {
            return Err(BridgeError::new(
                "cwd_outside_workspace",
                "The canonical cwd is outside the configured workspace",
            ));
        }
        let bytes = serde_json::to_vec(&(
            &self.shared.config_identity,
            path_string(&executable)?,
            &input.args,
            path_string(&cwd)?,
            input.deadline_ms,
        ))
        .map_err(|error| invalid(error.to_string()))?;
        Ok((
            ExecutionSpec {
                executable,
                args: input.args.clone(),
                cwd,
                deadline_ms: input.deadline_ms,
            },
            hex::encode(Sha256::digest(bytes)),
        ))
    }

    // No await occurs from committed admission through spawning the independent job.
    fn start(&self, input: StartInput) -> BridgeResult<StartOutput> {
        let (spec, request_digest) = self.normalize(&input)?;
        let mut state = self.shared.lock();
        if let Some(id) = state.keys.get(&input.idempotency_key) {
            let operation = &state.operations[id];
            if operation.record.request_digest != request_digest
                || operation.record.config_identity != self.shared.config_identity
            {
                return Err(BridgeError::new(
                    "idempotency_conflict",
                    "The retry key is already bound to different canonical inputs or broker configuration",
                ));
            }
            return Ok(StartOutput {
                operation: operation.record.view.clone(),
                replayed: true,
            });
        }
        if let Some(error) = &state.persistence_fault {
            return Err(error.clone());
        }
        if state.closing {
            return Err(BridgeError::new(
                "shutting_down",
                "Broker admission is closed",
            ));
        }
        if state.operations.len() >= self.shared.config.max_operations {
            return Err(BridgeError::new(
                "operation_limit_reached",
                "The durable operation admission limit has been reached",
            ));
        }
        if state.active_count >= self.shared.config.max_concurrent {
            return Err(BridgeError::new(
                "concurrency_limit_reached",
                "The active operation limit has been reached",
            ));
        }
        let timestamp = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let view = OperationView {
            operation_id: id.clone(), executable: path_string(&spec.executable)?, args: spec.args.clone(),
            cwd: path_string(&spec.cwd)?, deadline_ms: spec.deadline_ms, state: OperationState::Accepted,
            created_at_ms: timestamp, updated_at_ms: timestamp, exit_code: None,
            detail: Some("Admission committed; the independent broker job owns execution. Executable grants and cwd checks do not sandbox the executable.".into()),
            ownership_scope: SCOPE.into(), cancellation_requested: false, output_complete: false,
        };
        let record = JournalOperation {
            key: input.idempotency_key.clone(),
            request_digest,
            config_identity: self.shared.config_identity.clone(),
            view: view.clone(),
            latest_sequence: 0,
            frames: VecDeque::new(),
        };
        if let Err(error) = state.journal.insert(&record) {
            state.fault(error.clone());
            return Err(error);
        }
        let (control, receiver) = watch::channel(None);
        state.keys.insert(input.idempotency_key, id.clone());
        state.operations.insert(
            id.clone(),
            Operation {
                record,
                retained_bytes: 0,
                active: true,
                control: Some(control),
                persistence_fault: None,
            },
        );
        state.active_count += 1;
        let shared = self.shared.clone();
        let deadline = Instant::now() + Duration::from_millis(spec.deadline_ms);
        let job_id = id.clone();
        let job_shared = shared.clone();
        let job = tokio::spawn(async move {
            run_job(job_shared, job_id, spec, receiver, deadline).await;
        });
        // A panic in a job cannot silently strand an active registry entry.
        tokio::spawn(async move {
            if let Err(error) = job.await {
                shared.finish(&id, OperationState::Unknown, None, false,
                    format!("Owned job did not settle normally: {error}. Process effects and resource status are unknown."));
            }
        });
        Ok(StartOutput {
            operation: view,
            replayed: false,
        })
    }

    fn get(&self, id: &str) -> BridgeResult<OperationView> {
        let id = validate_id(id)?;
        self.shared
            .lock()
            .operations
            .get(&id)
            .map(|operation| operation.record.view.clone())
            .ok_or_else(|| BridgeError::new("not_found", "No operation has this ID"))
    }

    fn list(&self, input: ListInput) -> BridgeResult<OperationsPage> {
        if !(1..=64).contains(&input.limit) {
            return Err(invalid("limit must be 1..=64"));
        }
        let state = self.shared.lock();
        let mut operations: Vec<_> = state
            .operations
            .values()
            .map(|operation| &operation.record.view)
            .collect();
        operations.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then_with(|| right.operation_id.cmp(&left.operation_id))
        });
        let offset = usize::try_from(input.offset)
            .unwrap_or(usize::MAX)
            .min(operations.len());
        let end = offset
            .saturating_add(input.limit as usize)
            .min(operations.len());
        let summaries = operations[offset..end]
            .iter()
            .map(|operation| {
                // JSON quoting makes control characters and argument boundaries visible;
                // this preview is plain text and is never parsed back into a command.
                let mut display = std::iter::once(&operation.executable)
                    .chain(operation.args.iter())
                    .map(|part| serde_json::to_string(part).expect("Strings are serializable"))
                    .collect::<Vec<_>>()
                    .join(" ");
                if display.len() > 512 {
                    let mut boundary = 509;
                    while !display.is_char_boundary(boundary) {
                        boundary -= 1;
                    }
                    display.truncate(boundary);
                    display.push('…');
                }
                OperationSummary {
                    operation_id: operation.operation_id.clone(),
                    state: operation.state.clone(),
                    created_at_ms: operation.created_at_ms,
                    updated_at_ms: operation.updated_at_ms,
                    display_command: display,
                }
            })
            .collect();
        Ok(OperationsPage {
            operations: summaries,
            next_offset: (end < operations.len()).then_some(end as u64),
        })
    }

    fn cancel(&self, id: &str) -> BridgeResult<OperationView> {
        let id = validate_id(id)?;
        let mut state = self.shared.lock();
        let operation = state
            .operations
            .get_mut(&id)
            .ok_or_else(|| BridgeError::new("not_found", "No operation has this ID"))?;
        if !operation.active {
            return Ok(operation.record.view.clone());
        }
        operation.record.view.cancellation_requested = true;
        operation.record.view.updated_at_ms = now_ms();
        let view = operation.record.view.clone();
        let control = operation.control.clone();
        if let Err(error) = state.journal.update(&view) {
            state.fault(error.clone());
            return Err(error);
        }
        if let Some(control) = control {
            let _ = control.send(Some(StopReason::Cancelled));
        }
        Ok(view)
    }

    fn read(&self, input: ReadInput) -> BridgeResult<OutputPage> {
        let id = validate_id(&input.operation_id)?;
        if !(1024..=MAX_OUTPUT_PAGE_BYTES).contains(&input.max_bytes) {
            return Err(invalid("max_bytes must be 1024..=16384"));
        }
        let state = self.shared.lock();
        let operation = state
            .operations
            .get(&id)
            .ok_or_else(|| BridgeError::new("not_found", "No operation has this ID"))?;
        let record = &operation.record;
        if input.cursor > record.latest_sequence {
            return Err(invalid(
                "cursor is ahead of this operation's latest sequence",
            ));
        }
        let earliest = record
            .frames
            .front()
            .map_or(record.latest_sequence + 1, |frame| frame.sequence);
        let gap = input.cursor.saturating_add(1) < earliest;
        let mut bytes = 0;
        let mut frames = Vec::new();
        let mut next_cursor = input.cursor;
        for frame in record
            .frames
            .iter()
            .filter(|frame| frame.sequence > input.cursor)
        {
            if frames.len() >= MAX_PAGE_FRAMES || bytes + frame.bytes.len() > input.max_bytes {
                break;
            }
            bytes += frame.bytes.len();
            next_cursor = frame.sequence;
            frames.push(OutputFrame {
                sequence: frame.sequence,
                stream: frame.stream.clone(),
                bytes_base64: BASE64.encode(&frame.bytes),
                text: frame.text.clone(),
            });
        }
        Ok(OutputPage {
            operation_id: id,
            frames,
            next_cursor,
            gap,
            earliest_retained_sequence: earliest,
            latest_sequence: record.latest_sequence,
            has_more: next_cursor < record.latest_sequence,
            operation: record.view.clone(),
        })
    }

    pub async fn shutdown(&self) {
        {
            let mut state = self.shared.lock();
            state.closing = true;
            let ids: Vec<_> = state
                .operations
                .iter()
                .filter(|(_, operation)| operation.active)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                let operation = state
                    .operations
                    .get_mut(&id)
                    .expect("ID came from this locked registry");
                operation.record.view.cancellation_requested = true;
                operation.record.view.updated_at_ms = now_ms();
                let view = operation.record.view.clone();
                let control = operation.control.clone();
                if let Err(error) = state.journal.update(&view) {
                    state.fault(error);
                }
                if let Some(control) = control {
                    let _ = control.send(Some(StopReason::Shutdown));
                }
            }
        }
        let settled = async {
            loop {
                let notified = self.shared.settlement.notified();
                if self.shared.lock().active_count == 0 {
                    return;
                }
                notified.await;
            }
        };
        if timeout(Duration::from_secs(8), settled).await.is_err() {
            let mut state = self.shared.lock();
            let ids: Vec<_> = state
                .operations
                .iter()
                .filter(|(_, operation)| operation.active)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                let operation = state
                    .operations
                    .get_mut(&id)
                    .expect("ID came from this locked registry");
                operation.record.view.state = OperationState::Unknown;
                operation.record.view.output_complete = false;
                operation.record.view.updated_at_ms = now_ms();
                operation.record.view.detail = Some("Graceful shutdown settlement exceeded its bound; process effects and resource status remain unknown.".into());
                let view = operation.record.view.clone();
                if let Err(error) = state.journal.update(&view) {
                    state.fault(error);
                }
            }
            eprintln!(
                "Broker shutdown: one or more owned jobs did not settle within eight seconds; state is unknown."
            );
        }
    }
}

fn validate_id(id: &str) -> BridgeResult<String> {
    uuid::Uuid::parse_str(id)
        .map(|id| id.to_string())
        .map_err(|_| invalid("operation_id must be a valid UUID"))
}

impl State {
    fn fault(&mut self, error: BridgeError) {
        if self.persistence_fault.is_none() {
            eprintln!("Broker admission blocked: {error}");
            self.persistence_fault = Some(error.clone());
        }
        for operation in self
            .operations
            .values_mut()
            .filter(|operation| operation.active)
        {
            operation.persistence_fault = Some(error.to_string());
            operation.record.view.state = OperationState::Unknown;
            operation.record.view.updated_at_ms = now_ms();
            operation.record.view.output_complete = false;
            operation.record.view.cancellation_requested = true;
            operation.record.view.detail = Some(format!(
                "Critical persistence failure: {error}. Further starts are blocked; cancellation has been requested. Durable effects and execution status cannot be certified."
            ));
            if let Some(control) = &operation.control {
                let _ = control.send(Some(StopReason::PersistenceFailure));
            }
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                guard.fault(BridgeError::new("persistence_error", "Internal registry was poisoned; admission is closed and execution state is unknown"));
                guard
            }
        }
    }

    fn running(&self, id: &str) {
        let mut state = self.lock();
        let Some(operation) = state.operations.get_mut(id) else {
            return;
        };
        if operation.persistence_fault.is_some() {
            return;
        }
        operation.record.view.state = OperationState::Running;
        operation.record.view.updated_at_ms = now_ms();
        operation.record.view.detail = Some("Root spawned with exact argv, cleared environment, null stdin, and a new Linux process group. Executable behavior is not sandboxed.".into());
        let view = operation.record.view.clone();
        if let Err(error) = state.journal.update(&view) {
            state.fault(error);
        }
    }

    fn mark_stop(&self, id: &str) {
        let mut state = self.lock();
        let Some(operation) = state.operations.get_mut(id) else {
            return;
        };
        if operation.record.view.cancellation_requested {
            return;
        }
        operation.record.view.cancellation_requested = true;
        operation.record.view.updated_at_ms = now_ms();
        let view = operation.record.view.clone();
        if let Err(error) = state.journal.update(&view) {
            state.fault(error);
        }
    }

    fn output(&self, id: &str, stream: &str, bytes: Vec<u8>, text: String) {
        if bytes.is_empty() && text.is_empty() {
            return;
        }
        let mut state = self.lock();
        let Some(operation) = state.operations.get_mut(id) else {
            return;
        };
        let sequence = operation.record.latest_sequence + 1;
        let frame = JournalFrame {
            sequence,
            stream: stream.into(),
            bytes,
            text,
        };
        operation.record.latest_sequence = sequence;
        operation.retained_bytes += frame.bytes.len();
        operation.record.frames.push_back(frame.clone());
        while operation.retained_bytes > self.config.output_limit_bytes
            || operation.record.frames.len() > MAX_FRAMES
        {
            if let Some(dropped) = operation.record.frames.pop_front() {
                operation.retained_bytes -= dropped.bytes.len();
            }
        }
        let earliest = operation
            .record
            .frames
            .front()
            .map_or(sequence + 1, |frame| frame.sequence);
        if state.persistence_fault.is_none() {
            if let Err(error) = state.journal.append(id, &frame, earliest) {
                state.fault(error);
            }
        }
    }

    fn finish(
        &self,
        id: &str,
        final_state: OperationState,
        exit_code: Option<i32>,
        output_complete: bool,
        detail: String,
    ) {
        let mut state = self.lock();
        let Some(operation) = state.operations.get_mut(id) else {
            return;
        };
        if !operation.active {
            return;
        }
        operation.record.view.state = if operation.persistence_fault.is_some() {
            OperationState::Unknown
        } else {
            final_state
        };
        operation.record.view.updated_at_ms = now_ms();
        operation.record.view.exit_code = exit_code;
        operation.record.view.output_complete =
            output_complete && operation.persistence_fault.is_none();
        operation.record.view.detail = Some(match &operation.persistence_fault {
            Some(error) => format!(
                "{detail} Critical persistence failure remains visible: {error}; durable completion cannot be certified."
            ),
            None => detail,
        });
        let view = operation.record.view.clone();
        if let Err(error) = state.journal.update(&view) {
            state.fault(error);
        }
        let operation = state
            .operations
            .get_mut(id)
            .expect("Operation remains in this locked registry");
        operation.active = false;
        operation.control = None;
        state.active_count = state.active_count.saturating_sub(1);
        drop(state);
        self.settlement.notify_waiters();
    }
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

async fn run_job(
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
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // All destructive signalling is explicit while the owned root is unreaped.
    // Unexpected task/runtime failure remains Unknown; a drop handler must not
    // turn a lost-ownership condition into an unverified PID-based kill.
    command.as_std_mut().process_group(0);
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
    let Some(pid) = child.id() else {
        shared.finish(
            &id,
            OperationState::Unknown,
            None,
            false,
            "Spawn returned no process identity; resource status cannot be certified.".into(),
        );
        return;
    };
    shared.running(&id);
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let termination = end_owned_group(pid).await;
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
        match root_exited_without_reaping(pid) {
            Ok(true) => match group_has_executing_members(pid) {
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
        end_owned_group(pid)
            .await
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    // There are no group signals after this point; wait may now release the root PID.
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
            pipe_detail = " Output pipes did not close within one second of root settlement; a descendant may still hold them open. Reading was stopped, output is incomplete, and processes outside the owned group are not tracked or terminated.".into();
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
        Some(StopReason::ObservationFailure) => {
            "Process-group observation failed and requested cancellation. ".to_owned()
        }
        None => String::new(),
    };
    if let Some(status) = status {
        detail.push_str(&format!("Root exit observed: {status}."));
    }
    if reason.is_some() && termination_error.is_none() {
        detail.push_str(" SIGTERM then SIGKILL were issued while the root PID was still owned; no executing members remained in its inherited process group.");
    }
    if let Some(error) = observation_error {
        detail.push_str(&format!(" {error}."));
    }
    if let Some(error) = termination_error {
        detail.push_str(&format!(" Group termination was not verified: {error}."));
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

    #[test]
    fn unknown_config_fields_and_unsafe_limits_fail_closed() {
        assert!(
            serde_json::from_str::<BrokerConfig>(
                r#"{"workspace":"/tmp","allowed_executables":[],"unexpected":true}"#
            )
            .is_err()
        );
        let directory = tempfile::tempdir().unwrap();
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: vec![],
            output_limit_bytes: 8192,
            max_operations: 1,
            max_concurrent: 2,
        };
        assert!(Broker::open(config, directory.path()).is_err());
    }

    #[tokio::test]
    async fn critical_journal_failure_is_visible_and_blocks_new_execution() {
        let directory = tempfile::tempdir().unwrap();
        let executable = Path::new("/usr/bin/true").canonicalize().unwrap();
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: vec![executable.display().to_string()],
            output_limit_bytes: 8192,
            max_operations: 4,
            max_concurrent: 2,
        };
        let broker = Broker::open(config, directory.path()).unwrap();
        let injection =
            rusqlite::Connection::open(directory.path().join("operations.sqlite3")).unwrap();
        injection.execute_batch("CREATE TRIGGER injected_failure BEFORE UPDATE ON operations BEGIN SELECT RAISE(FAIL, 'test persistence failure'); END;").unwrap();
        let input = StartInput {
            idempotency_key: "before-fault".into(),
            executable: executable.display().to_string(),
            args: vec![],
            cwd: None,
            deadline_ms: 3000,
        };
        let start = broker.start(input.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let operation = broker.get(&start.operation.operation_id).unwrap();
            if matches!(operation.state, OperationState::Unknown) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Persistence failure must become visible"
            );
            sleep(Duration::from_millis(10)).await;
        }
        let mut second = input;
        second.idempotency_key = "after-fault".into();
        assert_eq!(broker.start(second).unwrap_err().code, "persistence_error");
        broker.shutdown().await;
        let operation = broker.get(&start.operation.operation_id).unwrap();
        assert!(matches!(operation.state, OperationState::Unknown));
        assert!(!operation.output_complete);
        assert!(operation.detail.unwrap().contains("persistence"));
    }

    #[tokio::test]
    async fn recovery_never_replays_unsettled_admissions() {
        let directory = tempfile::tempdir().unwrap();
        let executable = Path::new("/usr/bin/true").canonicalize().unwrap();
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: vec![executable.display().to_string()],
            output_limit_bytes: 8192,
            max_operations: 4,
            max_concurrent: 2,
        };
        let input = StartInput {
            idempotency_key: "survives-restart".into(),
            executable: executable.display().to_string(),
            args: vec![],
            cwd: None,
            deadline_ms: 1000,
        };
        let id = uuid::Uuid::new_v4().to_string();
        {
            let broker = Broker::open(config.clone(), directory.path()).unwrap();
            let (spec, digest) = broker.normalize(&input).unwrap();
            let record = JournalOperation {
                key: input.idempotency_key.clone(),
                request_digest: digest,
                config_identity: broker.shared.config_identity.clone(),
                view: OperationView {
                    operation_id: id.clone(),
                    executable: path_string(&spec.executable).unwrap(),
                    args: spec.args,
                    cwd: path_string(&spec.cwd).unwrap(),
                    deadline_ms: spec.deadline_ms,
                    state: OperationState::Running,
                    created_at_ms: 1,
                    updated_at_ms: 2,
                    exit_code: None,
                    detail: None,
                    ownership_scope: SCOPE.into(),
                    cancellation_requested: false,
                    output_complete: false,
                },
                latest_sequence: 0,
                frames: VecDeque::new(),
            };
            broker.shared.lock().journal.insert(&record).unwrap();
        }
        let broker = Broker::open(config, directory.path()).unwrap();
        let replay = broker.start(input).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.operation.operation_id, id);
        assert!(matches!(replay.operation.state, OperationState::Unknown));
        assert!(
            replay
                .operation
                .detail
                .unwrap()
                .contains("processes may still be running")
        );
        assert_eq!(broker.shared.lock().active_count, 0);
        broker.shutdown().await;
    }
}
