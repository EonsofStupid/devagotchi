/** Synthetic responses used only by fixture.html. No product import refers here. */
import type { BridgeClient } from "../../src/bridge";
import type { OperationView, OutputFrame } from "../../src/generated/contracts";

export function createFixtureClient(scenario: string): BridgeClient {
  const accepted = Date.now() - 17000;
  const operations: OperationView[] = [
    { operation_id: "fixture-51e2386d-running", executable: "/usr/bin/python3", args: ["-u", "tests/fixture_worker.py", "--workspace", "devagotchi"], cwd: "/workspace/devagotchi", deadline_ms: 180000, state: "running", created_at_ms: accepted, updated_at_ms: accepted + 2, exit_code: null, detail: null, ownership_scope: "Fixture process group only; this is synthetic test data.", cancellation_requested: false, output_complete: false },
    { operation_id: "fixture-23c871ca-schema", executable: "/usr/bin/node", args: ["scripts/generate-contracts.cjs", "--check"], cwd: "/workspace/devagotchi", deadline_ms: 60000, state: scenario === "cancel" ? "running" : "succeeded", created_at_ms: accepted - 80000, updated_at_ms: accepted - 74000, exit_code: scenario === "cancel" ? null : 0, detail: null, ownership_scope: "Fixture process group only.", cancellation_requested: false, output_complete: scenario !== "cancel" },
    { operation_id: "fixture-734c8ef1-build", executable: "/usr/bin/cargo", args: ["check", "--offline"], cwd: "/workspace/devagotchi/apps/desktop/src-tauri", deadline_ms: 300000, state: "succeeded", created_at_ms: accepted - 145000, updated_at_ms: accepted - 105000, exit_code: 0, detail: null, ownership_scope: "Fixture process group only.", cancellation_requested: false, output_complete: true },
    { operation_id: "fixture-17ae246b-format", executable: "/usr/bin/cargo", args: ["fmt", "--check"], cwd: "/workspace/devagotchi", deadline_ms: 30000, state: "failed", created_at_ms: accepted - 210000, updated_at_ms: accepted - 207000, exit_code: 1, detail: "Fixture command exited with status 1.", ownership_scope: "Fixture process group only.", cancellation_requested: false, output_complete: true },
    { operation_id: "fixture-48f2ea69-history", executable: "/usr/bin/git", args: ["status", "--short"], cwd: "/workspace/devagotchi", deadline_ms: 30000, state: "succeeded", created_at_ms: accepted - 290000, updated_at_ms: accepted - 289200, exit_code: 0, detail: null, ownership_scope: "Fixture process group only.", cancellation_requested: false, output_complete: true },
  ];
  if (scenario === "unknown") {
    operations.splice(1);
    Object.assign(operations[0], { state: "unknown", updated_at_ms: accepted + 4000, detail: "Fixture: broker restarted before the outcome could be confirmed.", output_complete: false });
  }
  if (scenario === "empty") operations.splice(0);
  const output: OutputFrame[] = [
    "[fixture] Preparing a local command\n",
    "[fixture] Workspace /workspace/devagotchi\n",
    "[fixture] Executable and argument vector accepted\n\n",
    "Connecting to the retained output stream…\n",
    "  ✓ Independent command ownership\n  ✓ Per-reader output cursor\n  ✓ Explicit cancellation\n\n",
    "Process text is never treated as page markup:\n",
    "<img src=x onerror=\"window.fixtureExecuted=true\">\n",
    "\u001b[32mControl codes stay visible.\u001b[0m\n",
  ].map((text, index) => ({ sequence: index + 1, stream: index === 7 ? "stderr" : "stdout", text, bytes_base64: btoa(unescape(encodeURIComponent(text))) }));
  if (scenario === "overflow") {
    output.splice(0);
    for (let index = 0; index < 2000; index++) {
      const text = "fixture " + String(index).padStart(4, "0") + " · " + "x".repeat(780) + "\n";
      output.push({ sequence: index + 1, stream: "stdout", text, bytes_base64: "" });
    }
    operations[0].state = "succeeded";
    operations[0].output_complete = true;
    operations[0].exit_code = 0;
  }
  function get(id: string) {
    const operation = operations.find((item) => item.operation_id === id);
    if (!operation) throw { code: "operation_not_found", message: "No such fixture command." };
    return operation;
  }
  return {
    async connectionInfo() { return { socket_path: "/tmp/devagotchi-fixture/broker.sock", source: "command_line", platform: "linux", app_version: "0.1.0", transport: "local_ipc" }; },
    async listOperations({ offset = 0, limit = 32 }) {
      if (scenario === "offline") throw { code: "ipc_io", message: "Fixture: no broker is listening at the configured path." };
      if (scenario === "error") throw { code: "socket_permissions", message: "Fixture: the broker socket is not private to the local operator." };
      return { operations: operations.slice(offset, offset + limit).map((item) => ({ operation_id: item.operation_id, state: item.state, created_at_ms: item.created_at_ms, updated_at_ms: item.updated_at_ms, display_command: item.executable + " " + item.args.join(" ") })), next_offset: null };
    },
    async getOperation({ operation_id }) { return { ...get(operation_id) }; },
    async readOutput({ operation_id, cursor = 0 }) {
      const operation = { ...get(operation_id) };
      const frames = scenario === "unknown" ? [] : output.filter((frame) => frame.sequence > cursor).slice(0, 16);
      const next = frames.at(-1)?.sequence ?? cursor;
      return { operation_id, operation, frames, next_cursor: next, gap: false, earliest_retained_sequence: output.length ? 1 : 0, latest_sequence: scenario === "unknown" ? 0 : output.length, has_more: scenario !== "unknown" && next < output.length };
    },
    async cancelOperation({ operation_id }) {
      if (scenario === "cancel") await new Promise((resolve) => setTimeout(resolve, 1500));
      const operation = get(operation_id);
      Object.assign(operation, { state: "cancelled", cancellation_requested: true, output_complete: true, updated_at_ms: Date.now(), detail: "Fixture cancellation confirmed." });
      return { ...operation };
    },
  };
}
