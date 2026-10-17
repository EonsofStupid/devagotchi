# Devagotchi

**Your AI, at home in your workspace.**

Devagotchi is a universal workspace bridge, beginning with locally installed MCP clients and a native app for seeing commands, output, and workspace activity. The product direction represents every admitted task as a graph and grows into a trusted internal mesh, with public API/MCP surfaces for adoption. Windows, macOS, and Linux are required targets. Source is published at [EonsofStupid/devagotchi](https://github.com/EonsofStupid/devagotchi). The rebuilt [Devagotchi Site](https://devagotchi.justsayit.chatgpt.site) is published with a read-only HTTP/MCP reference service and Linux source download; [devagotchi.net](https://devagotchi.net) has been added and is pending DNS validation.

This working copy starts the replacement for DesktopCommanderMCP with a separate process broker, a thin official-SDK MCP adapter, and a Tauri desktop inspector. **The current execution provider is implemented and tested on Linux. Windows and macOS providers are still required before a cross-platform release.**

## Project status and source preview

| Surface | Current status | What its evidence establishes |
|---|---|---|
| Desktop broker, local stdio MCP, and inspector | Linux foundation source and historical verification are included here | The recorded process/protocol/native UI scenarios; no Windows/macOS or named-host certification. |
| Public website and reference HTTP API/MCP | Published as Site version 3; HTTP reference/download verified, MCP enabled with OAuth and installed plugin | Read-only docs, local schemas, capability information, and a labelled graph example; no workstation execution. |
| Custom domain | devagotchi.net added, DNS pending | No verified custom-domain activation is claimed here. |
| Task graphs | Candidate contract and scoped implementation plan | No graph execution engine is implemented in this foundation. |
| Trusted mesh and runtime HTTP API/MCP | Committed direction; proposed contracts and acceptance gates | No public or peer-to-peer workstation runtime endpoint is implemented. |

This directory is the desktop foundation and planning source preview, not a signed product installer. Website/reference-service verification belongs to the existing Site's separate publication receipt and does not extend the original Linux evidence. A public tool schema is documentation; it does not grant access to the documented operation.

**Planning revision, October 17, 2026 UTC:** the [architecture and growth plan](docs/product-and-platform-plan.md) retains the existing owners, G0–G5 native/scale gates, and immediate P0 reference publication. The [task-graph and mesh contract](docs/task-graph-and-mesh-contract.md) defines proposed task/step/attempt/output/event records, local-to-mesh packets, and failure/authorization semantics. The [performance specification](docs/performance-and-experience-spec.md) defines proposed budgets and reproducible comparisons; none is an achieved benchmark. The [execution guide](docs/implementation-guide.md) separates public publication from core baseline work. [Desktop Commander user evidence](docs/desktop-commander-user-evidence.md) preserves primary reports, corrections, and fix evidence without claiming every reported problem remains present.

## Current desktop behavior

- The independently running broker owns admitted jobs. Closing the inspector or MCP connection does not terminate them.
- Every job has a stable operation ID, exact executable and argument vector, working directory, deadline, and durable state.
- Matching retries return the recorded job. Reusing a key with changed inputs is a conflict.
- Each output reader supplies its own cursor. UTF-8 decoding is incremental; raw bytes remain available losslessly. Retention limits produce explicit gaps.
- Cancellation and deadlines supervise the root and its inherited Linux process group. A parent exiting does not abandon children that remain in that group.
- Restarted brokers preserve unfinished work as `unknown`. They do not replay it or signal a remembered PID.
- The desktop inspector reads the same operations, displays exact command details and live retained output, and can request cancellation.
- The native host validates broker responses against Rust types. Desktop types are generated from the same canonical Rust contracts.

This desktop implementation does not yet provide file/search/edit tools, PTY input, workspace onboarding, a tray/service installer, Windows Job Objects/ConPTY, macOS process supervision, signed updates, task-graph execution, trusted mesh, or runtime HTTP. Those capabilities follow the scoped sequence in the [master plan](docs/product-and-platform-plan.md). The website/reference publication is a separate immediate workstream and does not wait for full native release.

## Components

| Component | Source | Responsibility |
|---|---|---|
| Broker | `src/execution.rs`, `src/journal.rs`, `src/bin/bridge-broker.rs` | Admission, execution, output retention, cancellation, recovery |
| Local IPC | `src/ipc.rs` | Private directory/socket, peer UID, instance lock, bounded framed requests |
| MCP adapter | `src/bin/bridge-mcp.rs` | Official `rmcp` 3.5.1 stdio transport and four typed tools |
| Desktop app | `apps/desktop` | Tauri 2 native host and React/TypeScript inspector |
| Website and public reference service | [`apps/website`](apps/website/README.md) | Product documentation, source download, and read-only HTTP/MCP reference routes |
| Contract authority | `src/contracts.rs` | Rust DTOs, strict decoding, generated JSON Schema and TypeScript |
| Verification | `tests`, module tests, `verification` | Real process and protocol checks plus recorded evidence |

The desktop process and MCP adapter both connect to the broker. Neither owns a job's lifetime or silently restarts a job after a lost connection.

As the product grows, graph scheduling extends this broker and journal. A step attempt references its real operation; transport redelivery does not create another attempt. Mesh targets keep their local grants and settlement authority. The website presents public reference material without acquiring either role.

## Build the Linux core

Install the Rust toolchain specified in `rust-toolchain.toml`. Then, from this repository root:

```bash
cargo build --locked --bins
cargo test --locked --lib
```

Create a broker configuration using `config/broker.example.json` as a template. Replace the absolute paths with existing paths on your machine. Select only executables you intend to authorize. The state directory's parent must already exist; the broker creates the state directory with mode `0700` or verifies an existing private directory.

```bash
./target/debug/bridge-broker \
  --state-dir /absolute/private/state/devagotchi \
  --config /absolute/path/broker.json
```

The endpoint is `/absolute/private/state/devagotchi/broker.sock`. Run the broker independently of the MCP client. `SIGTERM` or Ctrl-C requests bounded shutdown and cancellation of active owned jobs.

The example config's paths are placeholders, deliberately not broad default grants. The broker loads one workspace and one operator configuration at startup. Configuration errors prevent admission. MCP tools cannot modify grants.

## Connect a local MCP client

For clients using an `mcpServers` configuration object, the entry has this shape:

```json
{
  "mcpServers": {
    "devagotchi": {
      "command": "/absolute/path/to/devagotchi/target/debug/bridge-mcp",
      "args": ["--socket", "/absolute/private/state/devagotchi/broker.sock"]
    }
  }
}
```

The exact configuration location and format depend on the selected host. A named desktop client's integration has not yet been certified. The adapter's raw stdio tests cover both `initialize` with protocol `2025-11-25` and discovery/per-request metadata with `2026-07-28`.

Those are historical local-adapter results. The separate Site reference MCP targets an explicitly modern `2026-07-28` profile and requires its own published-route verification; it does not expose the following runtime tools for execution.

| Tool | Effect |
|---|---|
| `operation_start` | Admit an explicitly granted executable with exact arguments and a retry key |
| `operation_get` | Read the job's observed state and exact request |
| `operation_read` | Read retained output after a caller-owned cursor |
| `operation_cancel` | Explicitly request cancellation of an admitted job |

Stopping an MCP request only stops waiting for that response. Once admission is committed, reuse the same retry key and request to reconcile a lost reply. A different retry key can start new work.

## Desktop app

The desktop source is in `apps/desktop`. Install the native dependencies in the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/), then:

```bash
cd apps/desktop
npm ci
npm run build
DEVAGOTCHI_SOCKET=/absolute/private/state/devagotchi/broker.sock npm run tauri dev
```

The UI does not create grants or start a hidden broker. Its unconfigured, offline, empty, failed, and connected states are explicit. See the desktop README for the exact native build and QA commands. The separate browser fixture is for UI tests only and is not included in the production app.

## Generate contracts

After installing the desktop development dependencies:

```bash
node scripts/generate-contracts.cjs
node scripts/generate-contracts.cjs --check
```

The Rust exporter produces one JSON Schema bundle. `json-schema-to-typescript` generates the UI DTOs. The check compares fresh Rust output and generated TypeScript without changing either checked-in artifact. Rust remains responsible for domain bounds and executable grants; JSON Schema generation is not a substitute for that validation or an implemented task-graph engine.

## Run the acceptance checks

These tests create disposable workspaces and launch only their own fixture processes. Linux local Unix sockets must be available; the boundary suite performs a preflight so an environment that blocks sockets cannot produce misleading passing startup-denial tests.

```bash
python3 -B tests/mcp_smoke.py --output test-results/mcp-smoke.json
python3 -B tests/security_boundary.py --broker target/debug/bridge-broker
python3 -B tests/mcp_protocol.py \
  --adapter "$PWD/target/debug/bridge-mcp" \
  --socket /tmp/devagotchi-protocol-unused.sock \
  --output test-results/mcp-protocol.json
```

The smoke test covers real process execution, split Unicode, independent readers, retries/conflicts, reconnect, cancellation, deadlines, output overflow, and restart without re-execution. The boundary tests cover malformed frames, grants, distinct UIDs, socket/state collisions, journal hardlinks, connection saturation, single broker ownership, and children that outlive their root.

## Authority and limits

The local trust boundary is the OS user. A same-user process can use the same private endpoint; this version does not authenticate individual MCP applications separately. The socket does not expose a remote listener.

Future authenticated runtime HTTP and trusted peer routing must reuse broker authorization and recorded operation identity. A website login, graph edge, route advertisement, or client-supplied device label cannot widen a workstation grant. An unreachable target or uncertain attempt is not permission to execute the work on another machine.

Executable grants authorize the behavior of that executable as your OS user. Granting Python, a shell, or a similar interpreter grants its ability to run code. A checked working directory is not filesystem or network containment. Stronger restrictions require an actual OS isolation provider.

Output and command arguments can contain sensitive or hostile text. The broker and inspector do not send telemetry, and the journal is stored locally. Connected MCP clients receive tool results and may forward them to their configured model or services. The UI renders output as text. The journal contains exact arguments and retained output, so choose a private local state directory and appropriate retention. Logical output/frame and operation counts are bounded; a strict physical disk quota and a complete denial/audit event chain are later release requirements.

The current engine uses pipes, not a PTY. It clears inherited environment variables and closes stdin. Programs that need environment configuration, interactive input, or a real terminal are outside this initial execution contract and must receive explicit future support.

## Source and release status

DesktopCommanderMCP was inspected as the reference at commit `f508e524469c26f6366e17dbfd3ee5a69d112127`. The implementation here is a new foundation; it is not a claim of feature parity or benchmarked superiority over that project. The separate audit explains concrete behaviors being improved and preserves the reference's MIT license with its source snapshot.

The broker, desktop inspector, planning documents, audit evidence, and website source are published in this repository. The initial complete source import is commit [`72ea5db`](https://github.com/EonsofStupid/devagotchi/commit/72ea5db73e898efa0ebfc8800a531a2df29fe6f7), whose Git tree matches the verified 211-file source snapshot. Website code lives in [`apps/website`](apps/website/README.md) and retains its existing Sites deployment identity. The earlier `404` is preserved in historical audit artifacts; repository access is now verified.

The website is published at https://devagotchi.justsayit.chatgpt.site as version 3, source commit `91bb93c49eb35fa37315973666ce71dbb437a659`. The [publication receipt](verification/website-publication.json) records 12 passing reference-handler checks, successful type checking/build, 13 successful public GET checks, and the deployed MCP OAuth challenge. The MCP plugin is provisioned and installed; its tools did not surface in this chat, so an authenticated named-client call is not claimed. Browser visual QA remained unavailable. `devagotchi.net` is pending DNS validation. Public reference availability does not imply graph/mesh/runtime HTTP implementation, a native installer, or measured superiority.
