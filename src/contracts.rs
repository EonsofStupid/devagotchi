//! Canonical wire types. Serde rejects unknown fields; broker validation enforces bounds.

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

pub const API_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_PAGE_BYTES: usize = 16 * 1024;
pub const MAX_FILE_PAGE_BYTES: usize = 64 * 1024;
pub const MAX_FILE_LIST_ENTRIES: u32 = 512;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartInput {
    /// A caller-created retry key. The same key and inputs return the same operation.
    pub idempotency_key: String,
    /// Absolute path of an executable granted by the broker operator.
    pub executable: String,
    /// Exact argument vector. No shell parsing, interpolation, or added command text.
    #[serde(default)]
    pub args: Vec<String>,
    /// Absolute working directory within the configured workspace; defaults to its root.
    pub cwd: Option<String>,
    /// Execution deadline, not a client wait timeout. 1..=300000 milliseconds.
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetInput {
    pub operation_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileListInput {
    /// Absolute path of a directory within the configured workspace.
    pub path: String,
    /// The previous page's `next_cursor`; omit it for the first page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Maximum entries to return, 1..=512.
    #[serde(default = "default_file_list_limit")]
    pub limit: u32,
}
fn default_file_list_limit() -> u32 { 128 }

/// The kind of a directory entry itself. `symlink` covers symbolic links, junctions, and other
/// name-surrogate reparse points; they are never followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// One directory entry, describing the entry itself and never a link target.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// The entry's name. When `name_exact` is false the native name is not valid Unicode; invalid
    /// sequences are shown as U+FFFD and this version cannot address the entry by path.
    pub name: String,
    pub name_exact: bool,
    pub kind: FileKind,
    /// Present for regular files.
    pub size_bytes: Option<u64>,
    /// Last modification time in Unix milliseconds, when the filesystem reports one.
    pub modified_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileListPage {
    /// Canonical absolute path of the listed directory.
    pub path: String,
    /// Entries in ascending native-name order after the request cursor.
    pub entries: Vec<FileEntry>,
    /// Pass as `cursor` to read the next page; absent on the last page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileReadInput {
    /// Absolute path of a regular file within the configured workspace.
    pub path: String,
    /// Byte offset of the first byte to return.
    #[serde(default)]
    pub offset: u64,
    /// Maximum bytes to return, 1024..=65536.
    #[serde(default = "default_file_page_bytes")]
    pub max_bytes: usize,
}
fn default_file_page_bytes() -> usize { MAX_FILE_PAGE_BYTES }

/// The version of a file a read observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileRevision {
    /// Identity of the file object: device and inode on Unix, volume serial and file ID on
    /// Windows. It survives a rename and changes when the file is replaced by another object.
    pub identity: String,
    pub size_bytes: u64,
    /// Last modification time in Unix milliseconds, when the filesystem reports one.
    pub modified_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileReadPage {
    /// Canonical absolute path of the file that was read.
    pub path: String,
    pub revision: FileRevision,
    pub offset: u64,
    /// The exact bytes read.
    pub bytes_base64: String,
    /// The same bytes as text when they are valid UTF-8. A character split by the page end is
    /// left for the next page; any other invalid sequence leaves this absent.
    pub text: Option<String>,
    /// Offset to request next.
    pub next_offset: u64,
    /// True when the read reached the end of the file.
    pub eof: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestBody {
    OperationStart(StartInput),
    OperationGet(GetInput),
    OperationRead(ReadInput),
    OperationCancel(GetInput),
    OperationList(ListInput),
    FileList(FileListInput),
    FileRead(FileReadInput),
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
    Files(FileListPage),
    FileContent(FileReadPage),
}

impl ResponseBody {
    /// The wire `kind` tag, for diagnostics about an unexpected response.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Start(_) => "start",
            Self::Operation(_) => "operation",
            Self::Output(_) => "output",
            Self::Operations(_) => "operations",
            Self::Files(_) => "files",
            Self::FileContent(_) => "file_content",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireResponse {
    Ok(ResponseBody),
    Error(BridgeError),
}
