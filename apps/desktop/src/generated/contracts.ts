/**
 * Generated from canonical Rust DTOs in src/contracts.rs. DO NOT EDIT.
 * Generator: node scripts/generate-contracts.cjs
 * Drift check: node scripts/generate-contracts.cjs --check
 * JSON Schema: schemas/broker-contracts.schema.json
 * Broker validation enforces domain bounds beyond these structural types.
 */

/**
 * The kind of a directory entry itself. `symlink` covers symbolic links, junctions, and other
 * name-surrogate reparse points; they are never followed.
 */
export type FileKind = "file" | "directory" | "symlink" | "other";
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
  file_list_input: FileListInput;
  file_list_page: FileListPage;
  file_read_input: FileReadInput;
  file_read_page: FileReadPage;
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
export interface FileListInput {
  /**
   * The previous page's `next_cursor`; omit it for the first page.
   */
  cursor?: string | null;
  /**
   * Maximum entries to return, 1..=512.
   */
  limit?: number;
  /**
   * Absolute path of a directory within the configured workspace.
   */
  path: string;
}
export interface FileListPage {
  /**
   * Entries in ascending native-name order after the request cursor.
   */
  entries: FileEntry[];
  /**
   * Pass as `cursor` to read the next page; absent on the last page.
   */
  next_cursor?: string | null;
  /**
   * Canonical absolute path of the listed directory.
   */
  path: string;
}
/**
 * One directory entry, describing the entry itself and never a link target.
 */
export interface FileEntry {
  kind: FileKind;
  /**
   * Last modification time in Unix milliseconds, when the filesystem reports one.
   */
  modified_ms?: number | null;
  /**
   * The entry's name. When `name_exact` is false the native name is not valid Unicode; invalid
   * sequences are shown as U+FFFD and this version cannot address the entry by path.
   */
  name: string;
  name_exact: boolean;
  /**
   * Present for regular files.
   */
  size_bytes?: number | null;
}
export interface FileReadInput {
  /**
   * Maximum bytes to return, 1024..=65536.
   */
  max_bytes?: number;
  /**
   * Byte offset of the first byte to return.
   */
  offset?: number;
  /**
   * Absolute path of a regular file within the configured workspace.
   */
  path: string;
}
export interface FileReadPage {
  /**
   * The exact bytes read.
   */
  bytes_base64: string;
  /**
   * True when the read reached the end of the file.
   */
  eof: boolean;
  /**
   * Offset to request next.
   */
  next_offset: number;
  offset: number;
  /**
   * Canonical absolute path of the file that was read.
   */
  path: string;
  revision: FileRevision;
  /**
   * The same bytes as text when they are valid UTF-8. A character split by the page end is
   * left for the next page; any other invalid sequence leaves this absent.
   */
  text?: string | null;
}
/**
 * The version of a file a read observed.
 */
export interface FileRevision {
  /**
   * Identity of the file object: device and inode on Unix, volume serial and file ID on
   * Windows. It survives a rename and changes when the file is replaced by another object.
   */
  identity: string;
  /**
   * Last modification time in Unix milliseconds, when the filesystem reports one.
   */
  modified_ms?: number | null;
  size_bytes: number;
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
