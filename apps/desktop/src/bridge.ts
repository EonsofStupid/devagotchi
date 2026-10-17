import { invoke, isTauri } from "@tauri-apps/api/core";
import type {
  BridgeError,
  GetInput,
  ListInput,
  OperationView,
  OperationsPage,
  OutputPage,
  ReadInput,
} from "./generated/contracts";

/** Desktop configuration only. Broker wire types are generated from Rust. */
export interface ConnectionInfo {
  socket_path: string | null;
  source: "command_line" | "environment" | "unconfigured";
  platform: string;
  app_version: string;
  transport: string;
}

export interface BridgeClient {
  connectionInfo(): Promise<ConnectionInfo>;
  listOperations(input: ListInput): Promise<OperationsPage>;
  getOperation(input: GetInput): Promise<OperationView>;
  readOutput(input: ReadInput): Promise<OutputPage>;
  cancelOperation(input: GetInput): Promise<OperationView>;
}

async function call<T>(command: string, input?: unknown): Promise<T> {
  if (!isTauri()) {
    throw {
      code: "desktop_required",
      message: "Open the Devagotchi desktop application to connect to your local broker. This browser page has no native connection.",
    } satisfies BridgeError;
  }
  return invoke<T>(command, input === undefined ? {} : { input });
}

export const nativeBridge: BridgeClient = {
  connectionInfo: () => call("connection_info"),
  listOperations: (input) => call("list_operations", input),
  getOperation: (input) => call("get_operation", input),
  readOutput: (input) => call("read_output", input),
  cancelOperation: (input) => call("cancel_operation", input),
};

export function describeError(error: unknown): BridgeError {
  if (typeof error === "object" && error !== null && "message" in error) {
    const value = error as { code?: unknown; message: unknown };
    return {
      code: typeof value.code === "string" ? value.code : "connection_error",
      message: typeof value.message === "string" ? value.message : "The broker request failed.",
    };
  }
  return {
    code: "connection_error",
    message: typeof error === "string" ? error : "The broker request failed.",
  };
}
