# Verification evidence

**Planning revision, October 17, 2026 UTC:** the results below describe the earlier foundation runs, not new runs performed for the growth plan. Product source and raw reports are unchanged. The current source-manifest.json inventories this documentation revision; the original tested-foundation manifest is preserved at ../../audit/devagotchi-source-manifest-before-growth-plan.json. See ../../audit/planning-revision.json for the before/after document hashes and source-preservation check. No comparative performance or Windows/macOS evidence was added.

This directory records completed Linux core checks for the Devagotchi foundation. The three core harness JSON reports are unmodified copies of the corresponding files from `test-results/`. The native report is copied from the completed WebDriver run; build-checks.json records the separate observed build and module results. Their counts use different harness units: module tests, end-to-end checks, protocol assertions, and grouped boundary tests. They should not be combined into a single coverage score.

## Website and adoption publication

The separate [website publication receipt](website-publication.json) records the October 17 public Site, HTTP reference/download checks, MCP OAuth challenge and plugin provisioning, source identity, and DNS status. Its 12 reference-handler checks concern website code, not this Rust broker. Browser visual QA and an authenticated named-client MCP call remain unverified. The Linux results below are unchanged historical evidence.

## Devspace provisioning

[devspace-provision.json](devspace-provision.json) records the October 20, 2026 UTC end-to-end run of `deploy/devspace/provision.sh` in an Ubuntu 24.04 sandbox: the four failures it surfaced and their fixes, the `devspace-check` output (every toolchain under `/devspace`, nothing stray in `$HOME`), and a broker started with the generated config and probed through `bridge-mcp` (`tests/devspace_broker_probe.py`: `git --version` succeeded under the broker, an ungranted executable was refused). The sandbox has no PID 1 systemd, so `enable-linger` and the user unit are listed as not verified; they are checked on the real tenant.

## Recorded results

| Check | Recorded result | Evidence and scope |
|---|---|---|
| Core library/module tests | **7 passed; 0 failed, ignored, measured, or filtered** | Fresh integration verification with `cargo +stable test --locked --offline --lib`, Rust 1.99.1. Test profile finished in 0.80 s; tests in 0.23 s. The observed console output is preserved in [core-module-tests.txt](core-module-tests.txt). |
| Real broker through MCP stdio | **8 checks passed** | [mcp-smoke.json](mcp-smoke.json). Real Linux broker, adapter, fixture executables, and a scripted MCP client; 3.004 s. |
| MCP protocol and tool catalog | **11 checks passed** | [mcp-protocol.json](mcp-protocol.json). Raw stdio sessions; no broker execution. Recorded at `2026-10-16T23:26:52.749581+00:00`. |
| IPC and execution boundaries | **15 grouped tests passed; 0 failures, errors, or skips** | [security-boundary.json](security-boundary.json). Real broker with disposable Linux fixtures. Recorded from `2026-10-16T23:44:15.375175+00:00` to `2026-10-16T23:44:25.651760+00:00`. |
| Generated-contract drift check | **Passed** | Fresh integration verification with `node scripts/generate-contracts.cjs --check`. This is a generation-consistency check, not an additional process or UI test. The observed result is also recorded in [build-checks.json](build-checks.json). |

The module and generation checks were confirmed fresh. Smoke, boundary and native UI checks were rerun against the final rebuilt broker after its platform startup guards were compiled. The protocol report remains valid for the unchanged adapter hash. The harness outputs were copied without changing their evidence fields.

## Tested binary identity

| Report | Executable recorded by the report | Recorded SHA-256 |
|---|---|---|
| `security-boundary.json` | `target/debug/bridge-broker` | `4064d6f2d20b665f6b5dc421416a899df1ebe66b523cf88a95f915d185e20b14` |
| `mcp-protocol.json` | `target/debug/bridge-mcp` | `bbf03617f283acb2a5ac0c2c968e262c713075c899c3aa8fc0ce311b3b77838f` |

These report-bound hashes also matched the two current build-directory binaries when this summary was assembled. The boundary report additionally records `binary_changed_during_run: false`. Absolute executable paths inside the copied reports describe the original test environment; source and harness references below are relative to this repository.

**Attribution limit:** `mcp-smoke.json` does not record a run timestamp or executable hashes. Its `sha256` field in `split_utf8_lossless_bytes_and_independent_readers` hashes fixture output bytes, not a binary. Do not use that field to identify an executable or infer a stronger binary binding for the smoke run. The Cargo module result likewise has no archived test-executable hash.

## Harnesses and what they establish

| Harness / source | Principal implementation exercised | Distinction |
|---|---|---|
| [Core execution module tests](../src/execution.rs) and [journal module tests](../src/journal.rs) | UTF-8 decoder, configuration limits, journal persistence/fault behavior, recovery | Seven Rust library tests, including controlled state and persistence-failure injection. |
| [tests/mcp_smoke.py](../tests/mcp_smoke.py) | [MCP adapter](../src/bin/bridge-mcp.rs), [IPC](../src/ipc.rs), [execution](../src/execution.rs), [journal](../src/journal.rs) | Scripted stdio MCP client talking to the real broker and owned fixture processes. |
| [tests/mcp_protocol.py](../tests/mcp_protocol.py) | [MCP adapter](../src/bin/bridge-mcp.rs) and its official `rmcp` SDK transport/catalog | Protocol and catalog checks only; an unused socket path suffices. This is not full protocol-conformance or named-client certification. |
| [tests/security_boundary.py](../tests/security_boundary.py) | [Broker startup](../src/bin/bridge-broker.rs), [IPC](../src/ipc.rs), [contracts](../src/contracts.rs), [execution](../src/execution.rs), [journal](../src/journal.rs) | Black-box requests and fixture side-effect checks against the real broker. A grouped test can contain several rejection cases. |
| [scripts/generate-contracts.cjs](../scripts/generate-contracts.cjs) | [Rust exporter](../src/bin/export-contracts.rs), [canonical DTOs](../src/contracts.rs), [JSON Schema](../schemas/broker-contracts.schema.json), [generated TypeScript](../apps/desktop/src/generated/contracts.ts) | Detects drift between fresh generated output and checked-in contract artifacts; domain-bound enforcement remains in Rust. |

The seven module tests are:

- `utf8_is_identical_across_every_split_boundary`
- `utf8_invalid_and_truncated_sequences_are_explicit`
- `unknown_config_fields_and_unsafe_limits_fail_closed`
- `critical_journal_failure_is_visible_and_blocks_new_execution`
- `recovery_never_replays_unsettled_admissions`
- `journal_reopens_exact_bytes_and_idempotency_evidence`
- `journal_rejects_hardlinks_and_sidecar_collisions_without_mutation`

The smoke checks cover initialization and the four-tool catalog; split UTF-8 and lossless retained bytes; independent output readers; matching retries and conflicting keys; client disconnection without job cancellation; explicit root/child cancellation; execution deadlines; retention gaps; and broker crash recovery without re-execution. Multiple assertions are grouped within its eight named checks.

The protocol checks exercise both the `2025-11-25` initialize path and the `2026-07-28` discovery/per-request-metadata path supported by the installed SDK, including catalog schemas, invalid version/metadata and unknown-tool rejection, continued handling after rejection, and clean stdio shutdown.

The boundary checks cover private state/socket permissions, peer UID checks, preservation of active or foreign sockets, safe stale-socket replacement, lock collisions and single-broker ownership, symlink and journal hardlink collisions, malformed/oversized/truncated frames, strict request fields, grant denials without fixture side effects, one request per connection, recovery from incomplete-peer saturation, and cancellation of a same-group child after its root exits.

## Linux environment and limits

The boundary preflight recorded both local Unix-socket access and distinct-UID fixture support. All 15 grouped tests ran without skips. An environment that blocks socket creation must report a blocked setup rather than turn startup failures into passing denial tests; a run without distinct-UID support must retain its skip information. The tests use disposable directories and their own processes, with extra fixture privileges only where the UID scenarios require them.

These results establish the specific Linux behaviors exercised by the harnesses. The core checks alone do not establish native UI behavior; the separate native section below records that evidence. None of these results establishes Windows or macOS execution support, an installed MCP application's end-to-end integration, signing, packaging, or release readiness. Recovery checks use controlled broker loss and injected records/failures; they are not physical-power-loss qualification.

Executable grants and a checked working directory do not provide an OS sandbox. Granted programs still act with the broker user's authority. Supervision covers the root and descendants that remain in its inherited Linux process group; processes that leave that group are excluded. Passing journal and output-retention checks does not establish a physical disk quota, a complete denial/audit chain, or application-specific verification of all command effects.

## Reproduce the core checks

From the repository root, with the pinned Rust toolchain, desktop generation dependencies, Linux `/proc`, and local Unix sockets available:

```bash
cargo build --locked --bins
cargo test --locked --lib
node scripts/generate-contracts.cjs --check
mkdir -p test-results
python3 -B tests/mcp_smoke.py --output test-results/mcp-smoke.json
python3 -B tests/security_boundary.py --broker target/debug/bridge-broker --report test-results/security-boundary.json
python3 -B tests/mcp_protocol.py --adapter "$PWD/target/debug/bridge-mcp" --socket "$PWD/test-results/protocol-unused.sock" --output test-results/mcp-protocol.json
```

The protocol harness requires absolute `--adapter` and `--socket` values. Its socket argument above is an unused endpoint, not a broker to start. Regeneration or new runs should write fresh reports to `test-results/`; do not silently overwrite this evidence snapshot while retaining its old binary attribution.

## Native desktop and UI verification

**The actual Linux app built and passed seven native WebKit/broker checks.** [native-ui.json](native-ui.json) records the completed run, executable hashes, loaded asset names, WebDriver capabilities and system test packages. [build-checks.json](build-checks.json) records the independent native build and three native-host test passes.

The tested executable's SHA-256 is `29187b414926bc3c385fe1433c4107e72f59b100db54d4539ce18bf5cff9d201`. It embedded and loaded the current production assets from `tauri://localhost`; no Vite server or frontend test hooks supplied the behavior.

The seven-check run completed in 11.311 seconds and used the real MCP adapter to admit a completed historical command and a live fixture before opening the app. It verified real operation IDs and exact executable/arguments/directory; Unicode, stderr and markup-shaped text rendered as plain text; clicking **Cancel command** reached the broker and stopped a TERM-ignoring fixture; and broker loss produced the actual IPC error while retaining recorded history. A focused selection regression used a real pointer sequence A → B → A → B → Cancel: B stopped while A remained running, uncancelled, and continued producing its heartbeat; A was then cancelled during the ordinary cancellation check. This verifies the exercised interaction, not every possible timing schedule. Teardown completed cleanly, with no remaining app, driver, virtual-display or fixture processes observed in a scoped check afterward.

The native host's three Rust tests passed: explicit CLI configuration, navigation allowlisting, and framed list paging/response-kind handling. The allowed native command path was exercised end to end. Tauri's negative capability boundary was reviewed in source; these results are not a comprehensive runtime ACL penetration test.

The run used Ubuntu 24.04.3, Rust 1.99.1, Python 3.12.14, tauri-driver 2.1.0, and WebKitGTK/WebKitWebDriver 2.52.3 inside Xvfb. No WebKit sandbox-disable workaround was used. Actual running, cancelled and disconnected screenshots were inspected internally for visual layout and truthful state. Screenshots and fixture contents are excluded from the deliverable.

Reproduce after building the broker and installing the desktop dependencies and native test tools:

```bash
cd apps/desktop
npm ci
npm run build
cd ../..
cargo build --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --features custom-protocol
cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --features custom-protocol --lib
python3 -B tests/native_ui_smoke.py --tauri-driver /absolute/path/to/tauri-driver --artifacts-dir /absolute/private/qa-output
```

The optional Playwright browser-fixture suite was not completed in this environment: its browser download returned invalid archive content. Its explicit synthetic fixture entry is separate from production and is not used as a fallback. This limitation does not substitute for or invalidate the independently completed real native WebKit checks.

This is a development native build in a virtual Linux display, not an installer or a named MCP host certification. Windows/macOS UI and provider behavior, physical desktop integration, accessibility coverage, signing and update behavior still need their own evidence.
