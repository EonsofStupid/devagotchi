import contractSnapshot from "../public/reference/broker-contracts.schema.json";

export const REFERENCE_VERSION = "2026.10.17-preview.1";
export const MCP_VERSION = "2026-07-28";
export const SITE_ORIGIN = "https://devagotchi.justsayit.chatgpt.site";
export const PROJECT_REPOSITORY = "https://github.com/eonsofstupid/devagotchi";

export const platforms = [
  { name: "Linux", status: "Source preview", detail: "Broker, stdio MCP adapter, and native inspector. Tested on a Linux development host.", available: true },
  { name: "macOS", status: "Planned", detail: "Native process supervision, permissions, packaging, and validation remain to be built.", available: false },
  { name: "Windows", status: "Planned", detail: "Job Objects, named pipes, ConPTY, native permissions, and installers remain to be built.", available: false },
] as const;

export const localTools = [
  { name: "operation_start", description: "Start an explicitly granted executable with an exact argument vector, deadline, and retry key.", inputSchema: contractSnapshot.$defs.StartInput },
  { name: "operation_get", description: "Read the recorded operation state and exact invocation.", inputSchema: contractSnapshot.$defs.GetInput },
  { name: "operation_read", description: "Read retained output using your own cursor; gaps are explicit.", inputSchema: contractSnapshot.$defs.ReadInput },
  { name: "operation_cancel", description: "Request cancellation of an admitted operation.", inputSchema: contractSnapshot.$defs.GetInput },
] as const;

export const examples = {
  broker: JSON.stringify({ workspace: "/absolute/path/to/workspace", allowed_executables: ["/usr/bin/printf"], output_limit_bytes: 262144, max_operations: 64, max_concurrent: 4 }, null, 2),
  build: "cargo build --locked --bins",
  startBroker: "./target/debug/devagotchi broker run \
  --state-dir /absolute/private/state/devagotchi \\\n  --config /absolute/path/broker.json",
  localMcp: JSON.stringify({ mcpServers: { devagotchi: { command: "/absolute/path/to/devagotchi/target/debug/devagotchi", args: ["mcp", "--socket", "/absolute/private/state/devagotchi/broker.sock"] } } }, null, 2),
  start: JSON.stringify({ idempotency_key: "first-hello-001", executable: "/usr/bin/printf", args: ["Hello from Devagotchi\\n"], cwd: "/absolute/path/to/workspace", deadline_ms: 5000 }, null, 2),
  read: JSON.stringify({ operation_id: "<operation ID returned by operation_start>", cursor: 0, max_bytes: 16384 }, null, 2),
};

export function capabilities(origin = SITE_ORIGIN) {
  return {
    schema_version: "1.0",
    reference_version: REFERENCE_VERSION,
    product: "Devagotchi",
    stage: "developer-preview",
    reference: {
      purpose: "Public product documentation and capability discovery. No workstation execution.",
      api: `${origin}/api/v1/capabilities`,
      documentation: `${origin}/api/v1/reference`,
      openapi: `${origin}/openapi.json`,
      mcp: { url: `${origin}/mcp`, protocol_versions: [MCP_VERSION], transport: "streamable-http", access: "read-only reference; hosted MCP access policy applies" },
    },
    workspace: {
      available_as: "linux-source-preview",
      transport: "stdio",
      protocol_versions_verified_in_raw_tests: ["2025-11-25", MCP_VERSION],
      named_client_certification: [],
      tools: localTools.map(({ name }) => name),
      remote_execution_available: false,
      graph_execution_available: false,
      mesh_available: false,
    },
    platforms,
    source: { repository: PROJECT_REPOSITORY, repository_access_verified: false, preview: `${origin}/downloads/devagotchi-linux-source-preview.zip`, license: "No project license has been selected for the preview." },
  };
}

export const graphExamples = {
  local: {
    title: "Prepare a workspace change",
    description: "One machine. Independent checks can run together; review waits for their results.",
    status: "design-proposal",
    nodes: [
      { id: "inspect", title: "Inspect workspace", device: "This workstation", kind: "Process", detail: "Resolve the workspace and record the exact requested operation. The local broker checks its configured grants.", command: "git status --short", x: 8, y: 43 },
      { id: "test", title: "Run tests", device: "This workstation", kind: "Process", detail: "Wait for inspection. Start a separately identified attempt and keep output attached to that attempt.", command: "cargo test --locked", x: 39, y: 19 },
      { id: "build", title: "Build package", device: "This workstation", kind: "Process", detail: "Run alongside tests within the broker's capacity. A new attempt never erases the previous result.", command: "cargo build --locked", x: 39, y: 67 },
      { id: "review", title: "Review results", device: "This workstation", kind: "Review", detail: "Become ready only after both checks succeed and output is settled. The graph records why the node ran or stayed blocked.", command: "Requires test + build success", x: 70, y: 43 },
    ],
    edges: [["inspect", "test"], ["inspect", "build"], ["test", "review"], ["build", "review"]],
  },
  mesh: {
    title: "Validate across workstations",
    description: "A future mesh routes explicit steps to enrolled machines. Each machine keeps authority over execution.",
    status: "design-proposal",
    nodes: [
      { id: "plan", title: "Resolve targets", device: "Coordinator", kind: "Plan", detail: "Pin the graph revision, enrolled device identities, capabilities, and intended principals before dispatch.", command: "Explicit target IDs + approved grants", x: 8, y: 43 },
      { id: "linux", title: "Linux checks", device: "linux-builder", kind: "Process", detail: "The target broker validates authority and owns the attempt. A transport disconnect does not mean the process stopped.", command: "Target: linux-builder", x: 39, y: 8 },
      { id: "windows", title: "Windows checks", device: "windows-workstation", kind: "Process", detail: "A Windows provider must implement native containment and cancellation. Cross-platform execution is a planned capability.", command: "Target: windows-workstation", x: 39, y: 43 },
      { id: "mac", title: "macOS checks", device: "mac-workstation", kind: "Process", detail: "The macOS broker retains its own process and permission semantics. Device reachability never silently changes the target.", command: "Target: mac-workstation", x: 39, y: 78 },
      { id: "collect", title: "Collect evidence", device: "Coordinator", kind: "Join", detail: "Reconcile receipts from every required target. A partition produces an unknown outcome until evidence resolves it.", command: "Wait for all required receipts", x: 70, y: 43 },
    ],
    edges: [["plan", "linux"], ["plan", "windows"], ["plan", "mac"], ["linux", "collect"], ["windows", "collect"], ["mac", "collect"]],
  },
} as const;

export const documentation = {
  quickstart: {
    title: "Linux source preview quickstart",
    stage: "implemented-foundation",
    prerequisites: ["Linux with a native compiler/linker toolchain", "Rust 1.99.1, pinned by rust-toolchain.toml", "An existing workspace and a private state-directory parent", "A locally installed MCP host that can launch a stdio server"],
    steps: [
      { title: "Build the source snapshot", command: examples.build },
      { title: "Create broker.json with paths that exist on your machine", config: examples.broker },
      { title: "Start the broker independently", command: examples.startBroker },
      { title: "Configure your local MCP host", config: examples.localMcp },
      { title: "Call operation_start", arguments: JSON.parse(examples.start) },
      { title: "Read output using the returned operation ID", arguments: JSON.parse(examples.read) },
    ],
    notes: ["Verify /usr/bin/printf exists before granting it, or replace it with the actual absolute path.", "mcpServers is a common configuration shape, not a certified integration with a named host.", "The broker starts with a cleared environment and null stdin. A workspace grant is not an OS sandbox.", "Changing an executable grant requires changing the broker configuration and restarting the broker.", "max_operations caps all retained records, including completed operations; the default 64 is a finite preview limit. max_concurrent caps active work.", "Retry identity currently includes broker configuration; changed limits or grants can make an old key conflict.", "No installer or signed cross-platform release is available."],
  },
  "local-tools": {
    title: "Installed broker tools",
    stage: "implemented-foundation",
    scope: "These tools are served by `devagotchi mcp` on the user's Linux workstation. The public reference MCP does not execute them.",
    schema_source: "Generated snapshot from the canonical Rust contracts; runtime validation adds the numeric limits described in field documentation.",
    tools: localTools,
    semantics: ["A retry with the same key and exact inputs reconciles the original operation. Changed inputs conflict.", "Cancelling a transport request stops waiting; operation_cancel requests cancellation of admitted work.", "Every output reader supplies its own cursor. Retention may leave an explicit gap.", "Unfinished operations after broker restart become unknown and are not replayed automatically."],
  },
  "task-graphs": {
    title: "Every task is a graph",
    stage: "design-proposal",
    principles: ["A single command is a one-node graph; a larger task adds explicit dependencies.", "A graph definition is immutable per revision. A run instantiates that definition; a node can have several separately recorded attempts.", "Each process attempt uses the existing operation lifecycle and journal.", "Dependency readiness requires a known result and settled execution. Unknown never becomes success.", "The first scheduler is a bounded DAG. Loops require explicit bounded expansion rather than an accidental dependency cycle.", "Task dependency graphs and the cyclic topology of connected devices are different models."],
    examples: graphExamples,
  },
  mesh: {
    title: "Grow from one workstation to a trusted mesh",
    stage: "design-proposal",
    principles: ["Extend the same broker, policy checks, contracts, and journal. A transport adapter does not become another execution owner.", "Enroll devices with cryptographic identity; hostnames and model-supplied conversation IDs are not authorization.", "Every dispatch names its device, graph run, node, attempt, and authorized principal.", "A device validates current local grants when it admits and dispatches work.", "Leases and fencing protect scheduler ownership. They do not make arbitrary shell commands exactly once.", "Reconcile unknown outcomes after a partition before considering a retry. Never silently fall back to another machine.", "Start with explicit trusted peers over an existing secured network. Evaluate relays and broader discovery only after the execution and identity model is verified."],
  },
  platforms: { title: "Platform readiness", stage: "developer-preview", platforms },
} as const;

export type DocumentationTopic = keyof typeof documentation;
export const documentationTopics = Object.keys(documentation) as DocumentationTopic[];

export const referenceTools = [
  { name: "devagotchi_get_capabilities", description: "Read current Devagotchi platform readiness and connection endpoints. Reference only; cannot access or execute on a workstation.", inputSchema: { type: "object", properties: {}, additionalProperties: false }, annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false } },
  { name: "devagotchi_get_reference", description: "Read verified local-tool schemas, Linux onboarding, or clearly marked graph and mesh design proposals.", inputSchema: { type: "object", properties: { topic: { type: "string", enum: documentationTopics } }, required: ["topic"], additionalProperties: false }, annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false } },
] as const;

export function openApi(origin = SITE_ORIGIN) {
  return {
    openapi: "3.1.0",
    info: { title: "Devagotchi Public Reference API", version: REFERENCE_VERSION, description: "Read-only product reference and connection discovery. Does not start tasks, connect to devices, or read workstation data." },
    servers: [{ url: origin }],
    paths: {
      "/api/v1/capabilities": { get: { operationId: "getDevagotchiCapabilities", summary: "Read platform readiness and connection endpoints", responses: { "200": { description: "Current reference capabilities", content: { "application/json": { schema: { type: "object" } } } } } } },
      "/api/v1/reference": { get: { operationId: "getDevagotchiReference", summary: "Read a reference topic, or list available topics", parameters: [{ name: "topic", in: "query", required: false, schema: { type: "string", enum: documentationTopics } }], responses: { "200": { description: "Reference topic or topic index", content: { "application/json": { schema: { type: "object" } } } }, "400": { description: "Unknown or repeated query parameter" } } } },
    },
    externalDocs: { description: "Connection guide and MCP contract", url: `${origin}/connect` },
  };
}

export function referenceJson(data: unknown, status = 200) {
  return Response.json(data, { status, headers: { "Cache-Control": status === 200 ? "public, max-age=60" : "no-store", "Access-Control-Allow-Origin": "*", "X-Content-Type-Options": "nosniff" } });
}
