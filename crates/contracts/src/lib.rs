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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestBody {
    OperationStart(StartInput),
    OperationGet(GetInput),
    OperationRead(ReadInput),
    OperationCancel(GetInput),
    OperationList(ListInput),
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
