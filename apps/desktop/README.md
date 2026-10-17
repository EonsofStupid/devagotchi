# Devagotchi desktop

A native Tauri 2 inspector for the independently running Devagotchi broker, built with React and TypeScript. The window reads real command history, exact executable/argument/directory details, retained output, and observed working directories. It can explicitly request cancellation of an active command.

**Current native execution provider: Linux.** This checkout has compiled the Tauri host on Linux. Windows and macOS remain product requirements; their broker execution providers, native distribution, signing, and platform certification are pending. A frontend build or a Tauri cross-platform API is not evidence that those providers work. See the [project README](../../README.md) and its verification results for the full project status.

## Run the desktop app

Prerequisites:

- Use the repository's pinned Rust toolchain from `rust-toolchain.toml` (1.99.1 in this foundation). The Tauri manifest declares a minimum of Rust 1.90.
- Node **20.19+ on the 20.x line, or 22.12+** and npm, matching the pinned Vite engine requirement. This checkout was built with Node 24.
- The native prerequisites for your build host, described in the [official Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). Linux needs the GTK 3 and WebKitGTK 4.1 development libraries, `pkg-config`, and a native build toolchain.
- An independently configured and running broker. Follow the root README to configure executable/workspace grants and start it.

From `apps/desktop`:

```bash
npm ci
npm run build
cargo build --locked --manifest-path src-tauri/Cargo.toml --features custom-protocol
```

`custom-protocol` embeds the built assets for a standalone native run. With the default Cargo target directory, launch:

```bash
./src-tauri/target/debug/devagotchi --socket /absolute/path/to/broker-state/broker.sock
```

If you set `CARGO_TARGET_DIR`, use that directory's `debug/devagotchi` binary instead. The socket must refer to your existing broker; an arbitrary new socket path does not create one.

The command line accepts only these forms:

```text
devagotchi
devagotchi --socket ABS_PATH
devagotchi --help
```

With no argument, `DEVAGOTCHI_SOCKET` supplies the path. If neither is set, the app opens its connection setup view. A command-line path takes precedence over the environment; relative paths, extra options, and repeated options fail. `--help` works without a broker or a desktop display.

For a development window with Vite:

```bash
DEVAGOTCHI_SOCKET=/absolute/path/to/broker-state/broker.sock npm run tauri -- dev
```

Vite binds only to `127.0.0.1:1420`. Running `npm run dev` in a normal browser deliberately shows that the native host is missing. It does not fabricate a broker connection or switch into a demo.

## What the views mean

**Activity** reads a bounded page of 32 command summaries. Search and the active-state filter apply to that loaded page. Newer/older controls page through retained history. The selected command shows the exact executable, JSON argument array, working directory, execution deadline, state, and output. A command-list preview is display text and is never used as an executable command.

**Workspaces** groups the working directories observed in commands on the current Activity page. It does not claim to enumerate granted access, and it cannot change grants. The broker remains authoritative for access decisions.

**Connection** shows the configured local endpoint, its configuration source, the native platform, the last successful sync, and the actual error returned on failure. The endpoint cannot be changed from the webview.

Closing this window or losing its connection does not cancel broker-owned commands. The active command's **Cancel command** button sends an explicit cancellation request. Its pending state follows that operation ID even if you select a different command. The displayed state and control-scope detail describe the broker's observed outcome; a request alone is not a successful termination claim.

The elapsed field is time **since acceptance**. The execution deadline is the requested duration passed to the broker; the UI does not infer a precise process-start timestamp that the contract does not supply.

## Output and connection behavior

- The command list polls every three seconds. The selected output polls once per second while active, catches up in bounded batches, and stops when the broker confirms a terminal state with complete output. Terminal states with incomplete output use a slower three-second observation interval.
- Each selected command has its own sequence cursor. Obsolete asynchronous responses are ignored when selection, page, or refresh generation changes. Responses must identify the requested command before the UI accepts them.
- The frontend retains at most **1,200 displayed frames and 524,288 UTF-16 code units** (at most 1 MiB of UTF-16 text). It does not retain base64 output bytes in display state. A notice appears when earlier displayed text is trimmed or the broker reports a retention gap.
- Output is rendered as React text. HTML, terminal escape sequences, links, and control instructions are never executed. C0 control characters other than newline/tab become visible symbols.
- An unknown/interrupted outcome can have incomplete output. The UI labels that condition explicitly instead of implying that more output is guaranteed to arrive.
- Failed requests preserve the last received command view where available and expose the error. Reconnection never submits a new operation or retries an execution effect.

## Native boundary

The host exposes exactly five desktop commands:

| Tauri command | Broker request | Expected response |
| --- | --- | --- |
| `connection_info` | No broker request; local launch configuration | Desktop connection information |
| `list_operations` | `OperationList(ListInput)` | `Operations(OperationsPage)` |
| `get_operation` | `OperationGet(GetInput)` | `Operation(OperationView)` |
| `read_output` | `OperationRead(ReadInput)` | `Output(OutputPage)` |
| `cancel_operation` | `OperationCancel(GetInput)` | `Operation(OperationView)` |

Unexpected response variants fail explicitly. Canonical DTOs and domain validation remain in the broker. The host uses the root `desktop_bridge::ipc::request` implementation, including its local peer and socket checks. It never starts a broker, opens a shell, changes grants, or creates an operation.

`build.rs` enrolls all five app commands in Tauri's `AppManifest` ACL. `capabilities/inspector.json` grants those commands only to the local `main` window. No plugin command permission, remote URL grant, shell permission, or filesystem permission is included. The host denies external navigation and new windows. The production Content Security Policy permits bundled assets and local Tauri IPC, with no remote font or asset service. See [Tauri capabilities](https://v2.tauri.app/security/capabilities/) for why application commands must be enrolled in the manifest as well as named in a capability.

The only copied value exposed by the UI is a user-selected command ID. Copy failure is displayed; it is not reported as success.

## Generated contracts

`src/generated/contracts.ts` comes from canonical Rust DTOs in `src/contracts.rs` at the project root. Do not edit it by hand. From the project root, after installing the locked desktop dependencies:

```bash
node scripts/generate-contracts.cjs
node scripts/generate-contracts.cjs --check
```

The generator runs the Rust `export-contracts` binary, emits a shared JSON Schema graph, and compiles TypeScript from that graph. The drift check regenerates in memory and fails when the checked-in schema or types differ. It does not replace broker-side validation.

## Verification and fixture separation

The production frontend check is:

```bash
npm run build
```

The native host tests cover strict launch configuration, allowed navigation origins, and a real framed Unix IPC exchange preserving the list offset/limit and response mapping:

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml --features custom-protocol --lib
```

The project also includes `tests/native_ui_smoke.py` at the root. It drives the **actual compiled Tauri app**, a disposable real broker, and real process output using Tauri WebDriver, WebKitWebDriver, Xvfb, and D-Bus. Its JSON result records the binary and scenarios. See that script's `--help` and the root verification reports. This validates the tested Linux path; it does not certify a named MCP client or another operating system.

There is a separate optional browser harness under `tests/ui/`. It injects synthetic broker responses only through its explicit `fixture.html` entry, always with a **TEST FIXTURE** banner. Neither its client nor its entry is imported by `src/main.tsx` or embedded by the product build.

```bash
npm run qa:dev
# Open http://127.0.0.1:1421/tests/ui/fixture.html?scenario=activity
```

Fixture scenarios include `activity`, `empty`, `offline`, `error`, `unknown`, `cancel`, and `overflow`. In an environment with Playwright and its matching Chromium browser installed, `npm run qa:test` checks the product's missing-native state, navigation, literal text rendering, incomplete-output wording, cancellation correlation, and bounded retention. It writes a separate JSON result and screenshots under `qa-output/`.

**The optional Chromium fixture suite was not completed in this build environment:** no Chromium executable was installed, and the approved browser download returned an invalid archive. Its unsuccessful setup result must not be counted as passing UI evidence. The actual native WebKit run is tracked separately.

## Packaging status

The Tauri app icon is original vector artwork in `public/devagotchi.svg`; platform icon files were generated by the pinned Tauri CLI. Regenerate with:

```bash
npm run tauri -- icon public/devagotchi.svg
```

Native bundling is disabled in `tauri.conf.json` for this development slice. Installers, signing, notarization, automatic updates, Windows/macOS execution providers, platform installers, and named-client setup remain work beyond this local inspector. No capability in the current UI silently enables those features.
