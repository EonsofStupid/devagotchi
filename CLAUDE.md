# Devagotchi: how to work in this repository

Devagotchi is a Rust replacement for DesktopCommanderMCP: an independently running per-user
broker that owns jobs, a thin stdio MCP adapter, and a Tauri inspector. The plan is canon;
this file is how to work here. Read first:

- `docs/product-and-platform-plan.md`: decisions D01–D23 (D20–D23: one `devagotchi` binary, the three distribution tiers, the sign-in rule, board sync — §13), gates G0–G5, the boundary test for any new crate, process or service.
- `docs/performance-and-experience-spec.md` (workloads W01–W15, budgets B01–B14) and `docs/task-graph-and-mesh-contract.md`.
- `docs/agent-board.md`: the agents' ticket board in `crates/hub/src/board/` — per-project store, claim/lease protocol, how a CLI agent starts (decisions B01–B09).
- `docs/devagotchi/README.md`: the product design absorbed from shippinAI/agent-hub (index at `docs/devagotchi-spec.md`) and the ledger of what is ported, partial or pending.
- `deploy/devspace/README.md`: the canonical development environment and how to reach it remotely.

## Layout

Cargo workspace: `crates/contracts` (wire types, schema exporter), `crates/ipc` (framing, typed
client, Unix-socket and named-pipe endpoints), `crates/core` (broker, journal, execution),
`crates/broker` and `crates/mcp` (libraries with their own binaries, still named `bridge-broker`
and `bridge-mcp`), `crates/hub` (`devagotchi-hub`, the bastion: authenticated HTTP MCP routed to
brokers by name; the board), `crates/link` (`devagotchi-link`, a machine's outbound link to the
hub; no inbound port), `crates/devagotchi` (**the one binary**, D20: `devagotchi start | mcp |
board | broker | hub | link` over those same libraries — new behaviour goes in the library, never
in the dispatcher),
`deploy/` (`devspace/`, `shippin/`, `windows/` installers), `apps/desktop` (Tauri 2 + React inspector, not a default member), `apps/website`, `tests/`
(Python harnesses), `verification/` (recorded receipts), `schemas/` (generated).

## Before every push

```bash
cargo build --locked --bins
cargo nextest run --locked                         # every test binary, hub included
python3 tests/e2e.py --bin-dir target/debug        # broker, stdio adapter, hub and link end to end
cargo deny --all-features check                    # advisories, licenses, sources, no-network bans
python3 tests/security_boundary.py --broker "$PWD/target/debug/bridge-broker"
python3 tests/mcp_protocol.py --adapter "$PWD/target/debug/bridge-mcp" --socket /tmp/unused.sock
python3 tests/mcp_smoke.py --broker "$PWD/target/debug/bridge-broker" --adapter "$PWD/target/debug/bridge-mcp"
node scripts/generate-contracts.cjs --check      # after `npm ci` in apps/desktop
```

CI runs the build and nextest on ubuntu-24.04, ubuntu-24.04-arm, windows-2025, windows-2022,
windows-11-arm and macos-latest, the end-to-end run (and the Windows installer under PowerShell 7
and 5.1) on both Linux and all three Windows runners, the Linux server installer on x86_64 and
aarch64, `cargo deny` on every PR and weekly, and the release build for Linux and Windows on
x86_64 and ARM64. A PR merges only when all of it is green. `docs/security-baseline.md` is the checklist every PR clears.

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
- No network calls from `core` or `mcp`, ever; `deny.toml` bans HTTP client crates. Only
  `hub` (identity provider, `/link`) and `link` (its one outbound WebSocket) touch the network.
- On Jesse's Windows machines write to `D:`, never `C:`.
- Commits end with the `Co-Authored-By` and `Claude-Session` trailers the session provides.
