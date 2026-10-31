//! Canonical wire types. Serde rejects unknown fields; broker validation enforces bounds.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const API_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_PAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartInput {
    /// A caller-created retry key. The same key and inputs return the same operation.
    pub idempotency_key: String,
    /// Absolute path of the program to run, with `args`. On a broker that runs only
    /// operator-granted programs it must be granted. Leave empty when `command` is given.
    #[serde(default)]
    pub executable: String,
    /// Exact argument vector. No shell parsing, interpolation, or added command text.
    #[serde(default)]
    pub args: Vec<String>,
    /// A command line for the machine's own shell (bash or sh on Linux, Windows PowerShell
    /// on Windows), accepted by a broker that runs commands as its account. Give either
    /// this or `executable` with `args`, not both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Absolute working directory; defaults to the workspace. A broker that runs only
    /// granted programs keeps it within the workspace.
    pub cwd: Option<String>,
    /// Execution deadline, not a client wait timeout. 1..=300000 milliseconds.
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetInput {
    pub operation_id: String,
}

/// Longest a broker holds one wait open. MCP clients give up on a call after about a
/// minute, so a wait ends well before that and the caller waits again.
pub const MAX_WAIT_MS: u64 = 50_000;

/// Wait until the broker's operations change as a list shows them: one admitted, retired,
/// or moved to another state (output alone is not a change here). For a view that
/// re-lists only when something changed, instead of on a timer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangesInput {
    /// The revision your list reflects (from the last answer). Absent: answer at once with
    /// the current revision. A broker that restarted answers at once, too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_revision: Option<u64>,
    /// Longest to wait, 1..=50000 milliseconds; at the limit the same revision comes back.
    pub timeout_ms: u64,
}

/// The broker's current operations revision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitInput {
    pub operation_id: String,
    /// Return when output beyond this sequence exists (the cursor you have read up to), as
    /// well as when the operation settles. Without it, return only when it settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_after: Option<u64>,
    /// Longest to wait, 1..=50000 milliseconds. The answer comes as soon as the operation
    /// changes as asked, without polling; at the limit it comes back unchanged.
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInput {
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_list_limit")]
    pub limit: u32,
}
fn default_list_limit() -> u32 { 32 }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationSummary {
    pub operation_id: String,
    pub state: OperationState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    /// Bounded display preview only. Execute only the exact argv from the canonical request.
    pub display_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationsPage {
    pub operations: Vec<OperationSummary>,
    pub next_offset: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadInput {
    pub operation_id: String,
    /// Last sequence consumed, starting at zero. Reading never advances another reader.
    #[serde(default)]
    pub cursor: u64,
    /// Maximum raw bytes to return, 1024..=16384. Frames are at most 1024 bytes.
    #[serde(default = "default_page_bytes")]
    pub max_bytes: usize,
}

fn default_page_bytes() -> usize { MAX_OUTPUT_PAGE_BYTES }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Accepted,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Interrupted,
    Unknown,
}

impl OperationState {
    pub fn terminal(&self) -> bool {
        !matches!(self, Self::Accepted | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationView {
    pub operation_id: String,
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub deadline_ms: u64,
    pub state: OperationState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub exit_code: Option<i32>,
    pub detail: Option<String>,
    /// Root process and its inherited process group only; this is not OS containment.
    pub ownership_scope: String,
    pub cancellation_requested: bool,
    pub output_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartOutput {
    pub operation: OperationView,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputFrame {
    pub sequence: u64,
    pub stream: String,
    /// Raw process bytes, losslessly encoded. Text below is a UTF-8 convenience view.
    pub bytes_base64: String,
    /// Incrementally decoded per stream; split UTF-8 sequences are retained between chunks.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputPage {
    pub operation_id: String,
    pub frames: Vec<OutputFrame>,
    pub next_cursor: u64,
    /// True when requested output was evicted from this operation's bounded retention.
    pub gap: bool,
    pub earliest_retained_sequence: u64,
    pub latest_sequence: u64,
    pub has_more: bool,
    pub operation: OperationView,
}

/// Largest page of file bytes one read returns, so the answer fits one wire frame.
pub const MAX_FILE_READ_BYTES: usize = 24 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileReadInput {
    /// Absolute path of the file.
    pub path: String,
    /// Byte offset to start at; negative counts back from the end (a tail). Default 0.
    #[serde(default)]
    pub offset: i64,
    /// Bytes to read, 1..=24576 (default 24576). Read on from `next_offset` for more.
    #[serde(default = "default_file_read_bytes")]
    pub max_bytes: usize,
}

fn default_file_read_bytes() -> usize { MAX_FILE_READ_BYTES }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileContent {
    /// The file's canonical path.
    pub path: String,
    /// The file's size in bytes when it was read.
    pub size: u64,
    /// Where these bytes start.
    pub offset: u64,
    /// The bytes as text, when they are UTF-8 text. A character split at the end of the
    /// page is left for the next read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The raw bytes, base64, when they are not text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_base64: Option<String>,
    /// The offset to read on from.
    pub next_offset: u64,
    /// Whether this page reached the end of the file.
    pub eof: bool,
    pub modified_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileListInput {
    /// Absolute path of the directory.
    pub path: String,
    /// How many levels below it to include, 0..=3 (default 0: its own entries).
    #[serde(default)]
    pub depth: u32,
    /// Most entries to return, 1..=1000 (default 200).
    #[serde(default = "default_list_entries")]
    pub max_entries: u32,
}

fn default_list_entries() -> u32 { 200 }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Path relative to the listed directory, `/`-separated.
    pub path: String,
    /// `file`, `dir`, `symlink` or `other`. Symbolic links are listed, never followed.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<u64>,
    /// Why a directory's own entries are missing (it could not be read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileListing {
    /// The directory's canonical path.
    pub path: String,
    /// Directories first at each level, then by name.
    pub entries: Vec<FileEntry>,
    /// Whether `max_entries` cut the listing short.
    pub truncated: bool,
}

/// Largest content one write or edit carries, so the request fits one wire frame.
pub const MAX_FILE_WRITE_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Replace the file's whole content (atomically: a new file renamed into place), or
    /// create it. The default.
    #[default]
    Overwrite,
    /// Create the file; refuse when it exists.
    Create,
    /// Add to the end of the file, creating it when missing.
    Append,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileWriteInput {
    /// Absolute path of the file.
    pub path: String,
    /// The content as text, written as UTF-8 exactly as given. Give this or `bytes_base64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The content as base64 bytes. At most 32 KiB of content per call; append for more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_base64: Option<String>,
    #[serde(default)]
    pub mode: WriteMode,
    /// Overwrite only if the file's current SHA-256 is this (from an earlier write or
    /// edit), so a change made in between is not lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    /// Create missing parent directories.
    #[serde(default)]
    pub create_dirs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileEditInput {
    /// Absolute path of a UTF-8 text file.
    pub path: String,
    /// Exact text to find, including whitespace and line endings.
    pub old_text: String,
    /// Text to put in its place.
    pub new_text: String,
    /// How many times `old_text` must occur (default 1); every occurrence is replaced, and
    /// a different count changes nothing.
    #[serde(default = "default_replacements")]
    pub expected_replacements: u32,
}

fn default_replacements() -> u32 { 1 }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileWritten {
    /// The file's canonical path.
    pub path: String,
    /// Its size after the change.
    pub size: u64,
    /// SHA-256 of its whole content after the change: pass it as `expected_sha256` next time.
    pub sha256: String,
    /// Whether the file was created.
    pub created: bool,
    /// For an edit: how many occurrences were replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacements: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileInfoInput {
    /// Absolute path of a file, directory or symbolic link.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DirectoryInput {
    /// Absolute path of the directory to make; missing parents are made too. An existing
    /// directory is not an error.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileMoveInput {
    /// Absolute path of the file, directory or link to move or rename (a link moves as
    /// itself, never what it points to).
    pub source: String,
    /// Absolute path it moves to, on the same filesystem. Its parent directory must exist.
    pub destination: String,
    /// Replace a file already at the destination (never a directory). Default false: an
    /// existing destination refuses the move and changes nothing.
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileInfo {
    /// The path, canonical up to its own name (a link is reported as itself).
    pub path: String,
    /// `file`, `dir`, `symlink` or `other`.
    pub kind: String,
    /// Size in bytes (for a link, the link's own).
    pub size: u64,
    pub modified_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<u64>,
    /// Whether its permissions mark it read-only.
    pub readonly: bool,
    /// Unix permission bits in octal (`0644`); absent on Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Where a symbolic link points, as stored in the link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    /// For a text file of at most 8 MiB: how many lines it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
    /// After create_directory: whether this call made it (false: it existed already).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made: Option<bool>,
    /// After move_file: whether a file at the destination was replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
}

impl BridgeError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}
impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for BridgeError {}

pub type BridgeResult<T> = Result<T, BridgeError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestBody {
    OperationStart(StartInput),
    OperationGet(GetInput),
    OperationRead(ReadInput),
    OperationCancel(GetInput),
    OperationList(ListInput),
    OperationWait(WaitInput),
    OperationsChanged(ChangesInput),
    FileRead(FileReadInput),
    FileList(FileListInput),
    FileWrite(FileWriteInput),
    FileEdit(FileEditInput),
    FileInfo(FileInfoInput),
    FileMkdir(DirectoryInput),
    FileMove(FileMoveInput),
}

impl RequestBody {
    /// How long the broker may hold this request before answering, beyond ordinary work:
    /// transports (local IPC, the machine link) wait this much longer for the response.
    pub fn held_for(&self) -> std::time::Duration {
        match self {
            Self::OperationWait(WaitInput { timeout_ms, .. })
            | Self::OperationsChanged(ChangesInput { timeout_ms, .. }) => {
                std::time::Duration::from_millis((*timeout_ms).min(MAX_WAIT_MS))
            }
            _ => std::time::Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRequest {
    pub api_version: u32,
    pub request: RequestBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseBody {
    Start(StartOutput),
    Operation(OperationView),
    Output(OutputPage),
    Operations(OperationsPage),
    File(FileContent),
    Listing(FileListing),
    Written(FileWritten),
    Revision(Revision),
    Info(FileInfo),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireResponse {
    Ok(ResponseBody),
    Error(BridgeError),
}

/// The machine link (plan N20): a broker on another machine is reached through one
/// outbound WebSocket that its `devagotchi-link` holds open to the hub. These frames
/// are the whole protocol. A `call` carries exactly one broker request, so the
/// broker's own validation and executable grants still decide what runs.
pub const LINK_PROTOCOL_VERSION: u32 = 1;
/// Largest WebSocket message either side accepts.
pub const MAX_LINK_MESSAGE_BYTES: usize = 1024 * 1024;

/// Hub to device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HubFrame {
    /// The first frame on every connection. The device answers with a signed `hello`.
    Challenge { nonce: String, hub: String },
    Welcome { device: String },
    Refused { code: String, message: String },
    Call { id: u64, request: WireRequest },
}

/// Device to hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkFrame {
    /// Proves the device key over the challenge. `enroll_code` is present only on the
    /// first connection, when the hub binds this key to the device name.
    Hello {
        version: u32,
        device: String,
        /// Ed25519 public key, 32 bytes, hex.
        public_key: String,
        /// Ed25519 signature over `link_signing_message`, 64 bytes, hex.
        signature: String,
        #[serde(default)]
        enroll_code: Option<String>,
    },
    Result { id: u64, response: WireResponse },
    Heartbeat,
}

/// The exact bytes a device signs: protocol, the hub's public URL, the device name and
/// the hub's nonce, so a signature cannot be replayed to another hub, as another
/// device, or on another connection.
pub fn link_signing_message(hub: &str, device: &str, nonce: &str) -> Vec<u8> {
    format!("devagotchi-link/{LINK_PROTOCOL_VERSION}\n{hub}\n{device}\n{nonce}").into_bytes()
}
