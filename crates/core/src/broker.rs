//! Broker admission and the operation lifecycle: durable admissions,
//! idempotent retries, independent output cursors, explicit cancellation,
//! bounded shutdown, and restart recovery. Each admitted operation's process
//! is run by `crate::execution`. Executable grants are not a sandbox.

use devagotchi_contracts::*;
use crate::execution::platform::{self, SCOPE};
use crate::execution::{ExecutionSpec, StopReason, run_job};
use crate::journal::{Journal, JournalFrame, JournalOperation};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, watch};
use tokio::time::{Instant, timeout};

const MAX_FRAMES: usize = 1024;
const MAX_PAGE_FRAMES: usize = 64;

fn default_output_limit() -> usize {
    262_144
}
fn default_max_operations() -> usize {
    64
}
fn default_max_concurrent() -> usize {
    4
}

/// What a broker will run. `granted` (the default): only the operator's
/// `allowed_executables`, with exact arguments, in the workspace. `account`: anything its
/// own OS account can run, including a `command` line for the machine's shell, in any
/// directory, with the account's environment; the account's permissions are the boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecPolicy {
    #[default]
    Granted,
    Account,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    pub workspace: String,
    #[serde(default)]
    pub allowed_executables: Vec<String>,
    #[serde(default)]
    pub exec_policy: ExecPolicy,
    #[serde(default = "default_output_limit")]
    pub output_limit_bytes: usize,
    #[serde(default = "default_max_operations")]
    pub max_operations: usize,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
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

pub(crate) struct Shared {
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

/// An operation ID the broker does not hold: never admitted here, or finished and retired
/// to keep history bounded.
fn not_found() -> BridgeError {
    BridgeError::new(
        "not_found",
        "No operation has this ID here. Finished operations are retired, oldest first, once the broker holds its configured max_operations.",
    )
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
    // Windows canonical paths drop the verbatim prefix when that is lossless, so
    // child working directories and displayed paths stay ordinary drive paths.
    dunce::canonicalize(path)
        .map_err(|error| BridgeError::new(code, format!("Cannot canonicalize {kind}: {error}")))
}

fn path_string(path: &Path) -> BridgeResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Canonical paths must be representable as UTF-8"))
}

/// Whether this broker would accept `path` as an executable grant or request target: a
/// regular file the platform runs directly (Linux: an execute bit; Windows: an `.exe` or
/// `.com` image). `devagotchi start` asks before it writes a grant, so a config it writes
/// is one `Broker::open` accepts.
pub fn is_directly_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| platform::is_directly_executable(path, &metadata))
}

/// The platform's addition to "an executable regular file", for messages about a refused
/// grant.
pub const EXECUTABLE_RULE: &str = platform::EXECUTABLE_RULE;

impl Broker {
    pub fn open(config: BrokerConfig, state_dir: &Path) -> BridgeResult<Arc<Self>> {
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
            if !platform::is_directly_executable(&executable, &metadata) {
                return Err(BridgeError::new(
                    "invalid_config",
                    format!(
                        "Each executable grant must resolve to an executable regular file{}",
                        platform::EXECUTABLE_RULE
                    ),
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
            config.exec_policy,
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
        let account = self.shared.config.exec_policy == ExecPolicy::Account;
        let (executable, args) = match &input.command {
            Some(command) => {
                if !account {
                    return Err(BridgeError::new(
                        "command_not_allowed",
                        "This machine's broker runs only operator-granted programs with exact arguments: pass executable and args instead of command",
                    ));
                }
                if !input.executable.is_empty() || !input.args.is_empty() {
                    return Err(invalid(
                        "Give either command, or executable with args, not both",
                    ));
                }
                if command.trim().is_empty() || command.len() > 16 * 1024 || command.contains('\0')
                {
                    return Err(invalid(
                        "command must be 1..=16384 UTF-8 bytes without NUL bytes",
                    ));
                }
                platform::shell_command(command)
            }
            None => {
                let executable =
                    canonical_existing(&input.executable, "executable", "invalid_request")?;
                if !account && !self.shared.executables.contains(&executable) {
                    return Err(BridgeError::new(
                        "executable_not_allowed",
                        "The canonical executable path has no operator grant",
                    ));
                }
                let metadata = executable
                    .metadata()
                    .map_err(|error| invalid(error.to_string()))?;
                if !platform::is_directly_executable(&executable, &metadata) {
                    return Err(invalid(format!(
                        "Executable is not an executable regular file{}",
                        platform::EXECUTABLE_RULE
                    )));
                }
                (executable, input.args.clone())
            }
        };
        let cwd = match &input.cwd {
            Some(cwd) => canonical_existing(cwd, "cwd", "invalid_request")?,
            None => self.shared.workspace.clone(),
        };
        if !cwd.is_dir() {
            return Err(invalid("cwd must be an existing directory"));
        }
        if !account && !cwd.starts_with(&self.shared.workspace) {
            return Err(BridgeError::new(
                "cwd_outside_workspace",
                "The canonical cwd is outside the configured workspace",
            ));
        }
        let bytes = serde_json::to_vec(&(
            &self.shared.config_identity,
            path_string(&executable)?,
            &args,
            path_string(&cwd)?,
            input.deadline_ms,
        ))
        .map_err(|error| invalid(error.to_string()))?;
        Ok((
            ExecutionSpec {
                executable,
                args,
                cwd,
                deadline_ms: input.deadline_ms,
                account_environment: account,
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
        if state.journal.was_retired(&input.idempotency_key)? {
            return Err(BridgeError::new(
                "idempotency_key_retired",
                "This retry key belonged to an operation that finished and was retired from the broker's history. It is never run again; use a new key for new work.",
            ));
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
            // History is bounded, not a lifetime quota: the oldest finished operation makes
            // room. Only when every retained operation is still running is admission refused.
            state.retire_oldest_finished()?;
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
            detail: Some(if spec.account_environment {
                "Admission committed; the independent broker job owns execution. It runs as the broker's own account with that account's environment; this is not a sandbox.".into()
            } else {
                "Admission committed; the independent broker job owns execution. Executable grants and cwd checks do not sandbox the executable.".into()
            }),
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
            .ok_or_else(|| not_found())
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
        let operation = state.operations.get_mut(&id).ok_or_else(|| not_found())?;
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
        let operation = state.operations.get(&id).ok_or_else(|| not_found())?;
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
    /// Make room for one admission by retiring the operation that finished longest ago.
    /// A retired operation's ID is no longer found and its key is never run again.
    fn retire_oldest_finished(&mut self) -> BridgeResult<()> {
        let oldest = self
            .operations
            .iter()
            .filter(|(_, operation)| !operation.active && operation.record.view.state.terminal())
            .min_by_key(|(_, operation)| {
                (
                    operation.record.view.updated_at_ms,
                    operation.record.view.created_at_ms,
                )
            })
            .map(|(id, _)| id.clone());
        let Some(id) = oldest else {
            return Err(BridgeError::new(
                "operation_limit_reached",
                "Every operation the broker holds is still running; wait for one to finish or cancel one",
            ));
        };
        let record = &self.operations[&id].record;
        self.journal
            .retire(&id, &record.key, &record.request_digest, now_ms())?;
        if let Some(operation) = self.operations.remove(&id) {
            self.keys.remove(&operation.record.key);
        }
        Ok(())
    }

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

    pub(crate) fn running(&self, id: &str) {
        let mut state = self.lock();
        let Some(operation) = state.operations.get_mut(id) else {
            return;
        };
        if operation.persistence_fault.is_some() {
            return;
        }
        operation.record.view.state = OperationState::Running;
        operation.record.view.updated_at_ms = now_ms();
        operation.record.view.detail = Some(
            if self.config.exec_policy == ExecPolicy::Account {
                platform::RUNNING_DETAIL_ACCOUNT
            } else {
                platform::RUNNING_DETAIL
            }
            .into(),
        );
        let view = operation.record.view.clone();
        if let Err(error) = state.journal.update(&view) {
            state.fault(error);
        }
    }

    pub(crate) fn mark_stop(&self, id: &str) {
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

    pub(crate) fn output(&self, id: &str, stream: &str, bytes: Vec<u8>, text: String) {
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

    pub(crate) fn finish(
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    /// An executable that exits successfully without arguments or input.
    fn test_executable() -> PathBuf {
        #[cfg(unix)]
        let executable = dunce::canonicalize("/usr/bin/true").unwrap();
        #[cfg(windows)]
        let executable = system32("hostname.exe");
        executable
    }

    #[cfg(windows)]
    fn system32(name: &str) -> PathBuf {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot is set on Windows");
        dunce::canonicalize(Path::new(&root).join("System32").join(name)).unwrap()
    }

    /// The platform shell and the argv that makes it run `script`.
    fn shell(script: &[&str]) -> (PathBuf, Vec<String>) {
        #[cfg(unix)]
        let (shell, mut args) = (
            dunce::canonicalize("/bin/sh").unwrap(),
            vec!["-c".to_owned()],
        );
        #[cfg(windows)]
        let (shell, mut args) = (system32("cmd.exe"), vec!["/d".to_owned(), "/c".to_owned()]);
        #[cfg(unix)]
        args.push(script.join(" "));
        #[cfg(windows)]
        args.extend(script.iter().map(|part| (*part).to_owned()));
        (shell, args)
    }

    fn open_broker(directory: &tempfile::TempDir, grants: &[&Path]) -> Arc<Broker> {
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: grants
                .iter()
                .map(|grant| grant.display().to_string())
                .collect(),
            output_limit_bytes: 65_536,
            max_operations: 8,
            max_concurrent: 4,
            exec_policy: ExecPolicy::Granted,
        };
        Broker::open(config, &devagotchi_ipc::private_test_state(directory.path())).unwrap()
    }

    fn start(
        broker: &Broker,
        key: &str,
        executable: &Path,
        args: Vec<String>,
        deadline_ms: u64,
    ) -> String {
        broker
            .start(StartInput {
                idempotency_key: key.into(),
                executable: executable.display().to_string(),
                args,
                command: None,
                cwd: None,
                deadline_ms,
            })
            .unwrap()
            .operation
            .operation_id
    }

    async fn settle(broker: &Broker, id: &str) -> OperationView {
        settle_within(broker, id, 20).await
    }

    /// Windows PowerShell's first start on a fresh CI image can take tens of seconds.
    async fn settle_within(broker: &Broker, id: &str, seconds: u64) -> OperationView {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            let view = broker.get(id).unwrap();
            if view.state.terminal() {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "operation did not settle: {view:?}"
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    fn output_text(broker: &Broker, id: &str) -> String {
        let page = broker
            .read(ReadInput {
                operation_id: id.into(),
                cursor: 0,
                max_bytes: MAX_OUTPUT_PAGE_BYTES,
            })
            .unwrap();
        page.frames
            .iter()
            .map(|frame| frame.text.as_str())
            .collect()
    }

    #[tokio::test]
    async fn exit_status_and_output_are_recorded() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let (shell, args) = shell(&["echo devagotchi;", "exit 3"]);
        #[cfg(windows)]
        let (shell, args) = shell(&["echo", "devagotchi&", "exit", "3"]);
        let broker = open_broker(&directory, &[&shell]);
        let id = start(&broker, "exit-status", &shell, args, 20_000);
        let view = settle(&broker, &id).await;
        assert!(matches!(view.state, OperationState::Failed), "{view:?}");
        assert_eq!(view.exit_code, Some(3));
        assert!(view.output_complete);
        assert_eq!(view.ownership_scope, SCOPE);
        assert!(output_text(&broker, &id).contains("devagotchi"));
        broker.shutdown().await;
    }

    fn account_broker(directory: &tempfile::TempDir) -> Arc<Broker> {
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: vec![],
            exec_policy: ExecPolicy::Account,
            output_limit_bytes: 65_536,
            max_operations: 8,
            max_concurrent: 4,
        };
        Broker::open(config, &devagotchi_ipc::private_test_state(directory.path())).unwrap()
    }

    fn command_line(key: &str, line: &str, cwd: Option<String>) -> StartInput {
        StartInput {
            idempotency_key: key.into(),
            executable: String::new(),
            args: vec![],
            command: Some(line.into()),
            cwd,
            deadline_ms: 120_000,
        }
    }

    fn limited_config(directory: &tempfile::TempDir, grants: &[&Path], max: usize) -> BrokerConfig {
        BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: grants.iter().map(|g| g.display().to_string()).collect(),
            output_limit_bytes: 8192,
            max_operations: max,
            max_concurrent: 1,
            exec_policy: ExecPolicy::Granted,
        }
    }

    fn input(key: &str, executable: &Path, args: Vec<String>) -> StartInput {
        StartInput {
            idempotency_key: key.into(),
            executable: executable.display().to_string(),
            args,
            command: None,
            cwd: None,
            deadline_ms: 5_000,
        }
    }

    #[tokio::test]
    async fn the_account_policy_runs_a_command_line_in_the_machines_shell() {
        let directory = tempfile::tempdir().unwrap();
        let broker = account_broker(&directory);
        #[cfg(unix)]
        let line = "echo $((6*7)); test -n \"$HOME\" && echo home-set; exit 3";
        #[cfg(windows)]
        let line = "Write-Output (6*7); if ($env:PATHEXT) { Write-Output 'pathext-set' }; exit 3";
        // Outside the workspace: the account's own permissions are the boundary.
        let elsewhere = tempfile::tempdir().unwrap();
        let id = broker
            .start(command_line(
                "line",
                line,
                Some(elsewhere.path().display().to_string()),
            ))
            .unwrap()
            .operation
            .operation_id;
        let view = settle_within(&broker, &id, 120).await;
        assert_eq!(view.exit_code, Some(3), "{view:?}");
        let output = output_text(&broker, &id);
        assert!(output.contains("42"), "{output}");
        // The account's environment reaches the command (a cleared one would not).
        #[cfg(unix)]
        assert!(output.contains("home-set"), "{output}");
        #[cfg(windows)]
        assert!(output.contains("pathext-set"), "{output}");
        // A program nobody granted runs too.
        let program = start(&broker, "program", &test_executable(), vec![], 20_000);
        let view = settle(&broker, &program).await;
        assert!(matches!(view.state, OperationState::Succeeded), "{view:?}");
        broker.shutdown().await;
    }

    #[tokio::test]
    async fn finished_operations_are_retired_so_admission_never_runs_out() {
        let directory = tempfile::tempdir().unwrap();
        let executable = test_executable();
        let config = limited_config(&directory, &[&executable], 2);
        let state = devagotchi_ipc::private_test_state(directory.path());
        let broker = Broker::open(config.clone(), &state).unwrap();
        // More operations than the history holds: every one is admitted.
        let mut ids = Vec::new();
        for n in 0..5 {
            let id = start(&broker, &format!("key-{n}"), &executable, vec![], 5_000);
            settle(&broker, &id).await;
            ids.push(id);
        }
        // The two newest remain; the older ones are retired and no longer found.
        assert_eq!(broker.get(&ids[0]).unwrap_err().code, "not_found");
        assert!(broker.get(&ids[3]).is_ok() && broker.get(&ids[4]).is_ok());
        // A retired key is refused, never run again; a held key still replays.
        let retired = broker
            .start(input("key-0", &executable, vec![]))
            .unwrap_err();
        assert_eq!(retired.code, "idempotency_key_retired");
        let replay = broker.start(input("key-4", &executable, vec![])).unwrap();
        assert!(replay.replayed);
        broker.shutdown().await;
        // Both survive a restart.
        let broker = Broker::open(config, &state).unwrap();
        let retired = broker
            .start(input("key-1", &executable, vec![]))
            .unwrap_err();
        assert_eq!(retired.code, "idempotency_key_retired");
        assert_eq!(broker.get(&ids[1]).unwrap_err().code, "not_found");
        assert!(broker.get(&ids[4]).is_ok());
        broker.shutdown().await;
    }

    #[tokio::test]
    async fn only_the_account_policy_takes_a_command_line_and_never_with_a_program() {
        let directory = tempfile::tempdir().unwrap();
        let granted = open_broker(&directory, &[&test_executable()]);
        let refused = granted.start(command_line("line", "echo hi", None)).unwrap_err();
        assert_eq!(refused.code, "command_not_allowed");
        granted.shutdown().await;
        let other = tempfile::tempdir().unwrap();
        let account = account_broker(&other);
        let mut both = command_line("both", "echo hi", None);
        both.executable = test_executable().display().to_string();
        assert_eq!(account.start(both).unwrap_err().code, "invalid_request");
        let blank = account.start(command_line("blank", "  ", None)).unwrap_err();
        assert_eq!(blank.code, "invalid_request");
        account.shutdown().await;
    }

    #[tokio::test]
    async fn admission_waits_only_while_every_held_operation_runs() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let (long, args) = shell(&["/bin/sleep 30"]);
        #[cfg(windows)]
        let (long, args) = (
            system32("PING.EXE"),
            vec!["-n".to_owned(), "30".to_owned(), "127.0.0.1".to_owned()],
        );
        let quick = test_executable();
        let broker = Broker::open(
            limited_config(&directory, &[&long, &quick], 1),
            &devagotchi_ipc::private_test_state(directory.path()),
        )
        .unwrap();
        let running = start(&broker, "long", &long, args, 60_000);
        let refused = broker.start(input("quick", &quick, vec![])).unwrap_err();
        assert_eq!(refused.code, "operation_limit_reached");
        broker.cancel(&running).unwrap();
        settle(&broker, &running).await;
        let admitted = broker.start(input("quick", &quick, vec![])).unwrap();
        settle(&broker, &admitted.operation.operation_id).await;
        broker.shutdown().await;
    }

    #[tokio::test]
    async fn owned_descendants_keep_the_operation_running_until_cancelled() {
        let directory = tempfile::tempdir().unwrap();
        // The root exits at once and leaves one descendant running.
        #[cfg(unix)]
        let (shell, args) = shell(&["/bin/sleep 30 &", "exit 0"]);
        #[cfg(windows)]
        let ping = system32("PING.EXE");
        #[cfg(windows)]
        let (shell, args) = shell(&[
            "start",
            "/b",
            "",
            ping.to_str().unwrap(),
            "-n",
            "30",
            "127.0.0.1",
        ]);
        let broker = open_broker(&directory, &[&shell]);
        let id = start(&broker, "descendant", &shell, args, 60_000);
        sleep(Duration::from_millis(1500)).await;
        let view = broker.get(&id).unwrap();
        assert!(
            matches!(view.state, OperationState::Running),
            "a descendant outlived its root: {view:?}"
        );
        broker.cancel(&id).unwrap();
        let view = settle(&broker, &id).await;
        assert!(matches!(view.state, OperationState::Cancelled), "{view:?}");
        assert!(view.cancellation_requested);
        assert!(view.output_complete, "{view:?}");
        assert!(
            view.detail
                .unwrap()
                .contains(platform::TERMINATION_VERIFIED_DETAIL)
        );
        broker.shutdown().await;
    }

    #[tokio::test]
    async fn the_deadline_ends_the_owned_tree() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let (shell, args) = shell(&["/bin/sleep 30"]);
        #[cfg(windows)]
        let (shell, args) = (
            system32("PING.EXE"),
            vec!["-n".to_owned(), "30".to_owned(), "127.0.0.1".to_owned()],
        );
        let broker = open_broker(&directory, &[&shell]);
        let id = start(&broker, "deadline", &shell, args, 500);
        let view = settle(&broker, &id).await;
        assert!(matches!(view.state, OperationState::TimedOut), "{view:?}");
        assert!(
            view.detail
                .unwrap()
                .contains(platform::TERMINATION_VERIFIED_DETAIL)
        );
        broker.shutdown().await;
    }

    #[cfg(windows)]
    #[test]
    fn batch_files_are_not_executable_grants() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("grant.cmd");
        std::fs::write(&script, b"@echo off\r\n").unwrap();
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: vec![script.display().to_string()],
            output_limit_bytes: 8192,
            max_operations: 1,
            max_concurrent: 1,
            exec_policy: ExecPolicy::Granted,
        };
        let error = Broker::open(config, &devagotchi_ipc::private_test_state(directory.path()))
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains("cmd.exe"));
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
            exec_policy: ExecPolicy::Granted,
        };
        assert!(Broker::open(config, directory.path()).is_err());
    }

    #[tokio::test]
    async fn critical_journal_failure_is_visible_and_blocks_new_execution() {
        let directory = tempfile::tempdir().unwrap();
        let executable = test_executable();
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: vec![executable.display().to_string()],
            output_limit_bytes: 8192,
            max_operations: 4,
            max_concurrent: 2,
            exec_policy: ExecPolicy::Granted,
        };
        let state = devagotchi_ipc::private_test_state(directory.path());
        let broker = Broker::open(config, &state).unwrap();
        let injection = rusqlite::Connection::open(state.join("operations.sqlite3")).unwrap();
        injection.execute_batch("CREATE TRIGGER injected_failure BEFORE UPDATE ON operations BEGIN SELECT RAISE(FAIL, 'test persistence failure'); END;").unwrap();
        let input = StartInput {
            idempotency_key: "before-fault".into(),
            executable: executable.display().to_string(),
            args: vec![],
            command: None,
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
        let executable = test_executable();
        let config = BrokerConfig {
            workspace: directory.path().display().to_string(),
            allowed_executables: vec![executable.display().to_string()],
            output_limit_bytes: 8192,
            max_operations: 4,
            max_concurrent: 2,
            exec_policy: ExecPolicy::Granted,
        };
        let input = StartInput {
            idempotency_key: "survives-restart".into(),
            executable: executable.display().to_string(),
            args: vec![],
            command: None,
            cwd: None,
            deadline_ms: 1000,
        };
        let id = uuid::Uuid::new_v4().to_string();
        let state = devagotchi_ipc::private_test_state(directory.path());
        {
            let broker = Broker::open(config.clone(), &state).unwrap();
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
        let broker = Broker::open(config, &state).unwrap();
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
