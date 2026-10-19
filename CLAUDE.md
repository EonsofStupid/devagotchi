# Devagotchi: how to work in this repository

Devagotchi is a Rust replacement for DesktopCommanderMCP: an independently running per-user
broker that owns jobs, a thin stdio MCP adapter, and a Tauri inspector. The plan is canon;
this file is how to work here. Read first:

- `docs/product-and-platform-plan.md`: decisions D01–D18, gates G0–G5, the boundary test for any new crate, process or service.
- `docs/performance-and-experience-spec.md` (workloads W01–W15, budgets B01–B14) and `docs/task-graph-and-mesh-contract.md`.
- `deploy/devspace/README.md`: the canonical development environment and how to reach it remotely.

## Layout

Cargo workspace: `crates/contracts` (wire types, schema exporter), `crates/ipc` (framing, typed
client, Unix-socket and named-pipe endpoints), `crates/core` (broker, journal, execution),
`crates/broker` and `crates/mcp` (the two binaries, still named `bridge-broker` and `bridge-mcp`),
`apps/desktop` (Tauri 2 + React inspector, not a default member), `apps/website`, `tests/`
(Python harnesses), `verification/` (recorded receipts), `schemas/` (generated).

## Before every push

```bash
cargo build --locked --bins
cargo test --locked --lib
python3 tests/security_boundary.py --broker "$PWD/target/debug/bridge-broker"
python3 tests/mcp_protocol.py --adapter "$PWD/target/debug/bridge-mcp" --socket /tmp/unused.sock
python3 tests/mcp_smoke.py --broker "$PWD/target/debug/bridge-broker" --adapter "$PWD/target/debug/bridge-mcp"
node scripts/generate-contracts.cjs --check      # after `npm ci` in apps/desktop
```

CI runs the build and unit tests on ubuntu-24.04, windows-2025, windows-2022 and macos-latest;
a PR merges only when all four are green.

## Rules

- One concern per PR, branch `increment-N/<topic>`, PR body states what was verified and where.
- Extend an existing owner before adding a crate, process or service; the plan's boundary test
  applies. Never a second job system, a broker per workspace, or recovery by remembered PID.
- Native code lives only in `crates/core/src/execution/platform/*` and `crates/ipc/src/{unix,windows}*`;
  `unsafe` is allowed only there. A platform mechanism lands with its real-OS test in the same PR.
- Contracts change only in `crates/contracts/src/lib.rs`; regenerate with `node scripts/generate-contracts.cjs`;
  never hand-edit `schemas/` or `apps/desktop/src/generated/`.
- Decisions: a resolved register row becomes `docs/adr/NNNN-*.md` through Jesse's ADR component.
  An ADR not saved through it is not canon. Do not re-open LOCKED or COMMITTED rows.
- No network calls from `core` or `mcp`, ever; `deny.toml` bans HTTP client crates.
- On Jesse's Windows machines write to `D:`, never `C:`.
- Commits end with the `Co-Authored-By` and `Claude-Session` trailers the session provides.
