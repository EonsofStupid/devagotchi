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
  file_content: FileContent;
  file_edit_input: FileEditInput;
  file_list_input: FileListInput;
  file_listing: FileListing;
  file_read_input: FileReadInput;
  file_write_input: FileWriteInput;
  file_written: FileWritten;
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
export interface FileContent {
  /**
   * The raw bytes, base64, when they are not text.
   */
  bytes_base64?: string | null;
  /**
   * Whether this page reached the end of the file.
   */
  eof: boolean;
  modified_ms?: number | null;
  /**
   * The offset to read on from.
   */
  next_offset: number;
  /**
   * Where these bytes start.
   */
  offset: number;
  /**
   * The file's canonical path.
   */
  path: string;
  /**
   * The file's size in bytes when it was read.
   */
  size: number;
  /**
   * The bytes as text, when they are UTF-8 text. A character split at the end of the
   * page is left for the next read.
   */
  text?: string | null;
}
export interface FileEditInput {
  /**
   * How many times `old_text` must occur (default 1); every occurrence is replaced, and
   * a different count changes nothing.
   */
  expected_replacements?: number;
  /**
   * Text to put in its place.
   */
  new_text: string;
  /**
   * Exact text to find, including whitespace and line endings.
   */
  old_text: string;
  /**
   * Absolute path of a UTF-8 text file.
   */
  path: string;
}
export interface FileListInput {
  /**
   * How many levels below it to include, 0..=3 (default 0: its own entries).
   */
  depth?: number;
  /**
   * Most entries to return, 1..=1000 (default 200).
   */
  max_entries?: number;
  /**
   * Absolute path of the directory.
   */
  path: string;
}
export interface FileListing {
  /**
   * Directories first at each level, then by name.
   */
  entries: FileEntry[];
  /**
   * The directory's canonical path.
   */
  path: string;
  /**
   * Whether `max_entries` cut the listing short.
   */
  truncated: boolean;
}
export interface FileEntry {
  /**
   * Why a directory's own entries are missing (it could not be read).
   */
  error?: string | null;
  /**
   * `file`, `dir`, `symlink` or `other`. Symbolic links are listed, never followed.
   */
  kind: string;
  modified_ms?: number | null;
  /**
   * Path relative to the listed directory, `/`-separated.
   */
  path: string;
  size?: number | null;
}
export interface FileReadInput {
  /**
   * Bytes to read, 1..=24576 (default 24576). Read on from `next_offset` for more.
   */
  max_bytes?: number;
  /**
   * Byte offset to start at; negative counts back from the end (a tail). Default 0.
   */
  offset?: number;
  /**
   * Absolute path of the file.
   */
  path: string;
}
export interface FileWriteInput {
  /**
   * The content as base64 bytes. At most 32 KiB of content per call; append for more.
   */
  bytes_base64?: string | null;
  /**
   * Create missing parent directories.
   */
  create_dirs?: boolean;
  /**
   * Overwrite only if the file's current SHA-256 is this (from an earlier write or
   * edit), so a change made in between is not lost.
   */
  expected_sha256?: string | null;
  mode?: "overwrite" | "create" | "append";
  /**
   * Absolute path of the file.
   */
  path: string;
  /**
   * The content as text, written as UTF-8 exactly as given. Give this or `bytes_base64`.
   */
  text?: string | null;
}
export interface FileWritten {
  /**
   * Whether the file was created.
   */
  created: boolean;
  /**
   * The file's canonical path.
   */
  path: string;
  /**
   * For an edit: how many occurrences were replaced.
   */
  replacements?: number | null;
  /**
   * SHA-256 of its whole content after the change: pass it as `expected_sha256` next time.
   */
  sha256: string;
  /**
   * Its size after the change.
   */
  size: number;
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
   * A command line for the machine's own shell (bash or sh on Linux, Windows PowerShell
   * on Windows), accepted by a broker that runs commands as its account. Give either
   * this or `executable` with `args`, not both.
   */
  command?: string | null;
  /**
   * Absolute working directory; defaults to the workspace. A broker that runs only
   * granted programs keeps it within the workspace.
   */
  cwd?: string | null;
  /**
   * Execution deadline, not a client wait timeout. 1..=300000 milliseconds.
   */
  deadline_ms: number;
  /**
   * Absolute path of the program to run, with `args`. On a broker that runs only
   * operator-granted programs it must be granted. Leave empty when `command` is given.
   */
  executable?: string;
  /**
   * A caller-created retry key. The same key and inputs return the same operation.
   */
  idempotency_key: string;
}
export interface StartOutput {
  operation: OperationView;
  replayed: boolean;
}
