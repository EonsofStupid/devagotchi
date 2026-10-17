/**
 * Generated from canonical Rust DTOs in src/contracts.rs. DO NOT EDIT.
 * Generator: node scripts/generate-contracts.cjs
 * Drift check: node scripts/generate-contracts.cjs --check
 * JSON Schema: schemas/broker-contracts.schema.json
 * Broker validation enforces domain bounds beyond these structural types.
 */

export type OperationState =
  | "accepted"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "timed_out"
  | "interrupted"
  | "unknown";

/**
 * Export-only schema root. These fields reference canonical Rust DTOs and are
 * not a broker request or a second set of independently authored contracts.
 */
export interface BrokerContracts {
  bridge_error: BridgeError;
  get_input: GetInput;
  list_input: ListInput;
  operation_view: OperationView;
  operations_page: OperationsPage;
  output_page: OutputPage;
  read_input: ReadInput;
  start_input: StartInput;
  start_output: StartOutput;
}
export interface BridgeError {
  code: string;
  message: string;
}
export interface GetInput {
  operation_id: string;
}
export interface ListInput {
  limit?: number;
  offset?: number;
}
export interface OperationView {
  args: string[];
  cancellation_requested: boolean;
  created_at_ms: number;
  cwd: string;
  deadline_ms: number;
  detail?: string | null;
  executable: string;
  exit_code?: number | null;
  operation_id: string;
  output_complete: boolean;
  /**
   * Root process and its inherited process group only; this is not OS containment.
   */
  ownership_scope: string;
  state: OperationState;
  updated_at_ms: number;
}
export interface OperationsPage {
  next_offset?: number | null;
  operations: OperationSummary[];
}
export interface OperationSummary {
  created_at_ms: number;
  /**
   * Bounded display preview only. Execute only the exact argv from the canonical request.
   */
  display_command: string;
  operation_id: string;
  state: OperationState;
  updated_at_ms: number;
}
export interface OutputPage {
  earliest_retained_sequence: number;
  frames: OutputFrame[];
  /**
   * True when requested output was evicted from this operation's bounded retention.
   */
  gap: boolean;
  has_more: boolean;
  latest_sequence: number;
  next_cursor: number;
  operation: OperationView;
  operation_id: string;
}
export interface OutputFrame {
  /**
   * Raw process bytes, losslessly encoded. Text below is a UTF-8 convenience view.
   */
  bytes_base64: string;
  sequence: number;
  stream: string;
  /**
   * Incrementally decoded per stream; split UTF-8 sequences are retained between chunks.
   */
  text: string;
}
export interface ReadInput {
  /**
   * Last sequence consumed, starting at zero. Reading never advances another reader.
   */
  cursor?: number;
  /**
   * Maximum raw bytes to return, 1024..=16384. Frames are at most 1024 bytes.
   */
  max_bytes?: number;
  operation_id: string;
}
export interface StartInput {
  /**
   * Exact argument vector. No shell parsing, interpolation, or added command text.
   */
  args?: string[];
  /**
   * Absolute working directory within the configured workspace; defaults to its root.
   */
  cwd?: string | null;
  /**
   * Execution deadline, not a client wait timeout. 1..=300000 milliseconds.
   */
  deadline_ms: number;
  /**
   * Absolute path of an executable granted by the broker operator.
   */
  executable: string;
  /**
   * A caller-created retry key. The same key and inputs return the same operation.
   */
  idempotency_key: string;
}
export interface StartOutput {
  operation: OperationView;
  replayed: boolean;
}
