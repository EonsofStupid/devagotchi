# Spec P2: tickets tied to machine, project + repo, environment, agent + session

Status: DRAFT for Jesse's approval. Plan: `docs/plan-phone-and-signals.md` (P2). Ruled
2026-11-06: every ticket is tied to its machine/system, project + repo, environment, and
agent + session. Paths relative to `crates/hub/`; names are anchors, lines are hints.

## What changes for a person and an AI
- Every ticket shows where it belongs: `machine`, `repo`, `environment`, `agent`, `session`
  (the project is the board it lives on). All optional; filled automatically when known.
- `board_list` / the CLI / the phone filter by any of them ("open tickets on dabo in prod").
- A ticket can carry a `fingerprint` (P3 uses it to fold repeats into one ticket) with
  `occurrences` and `last_seen_at`.
- A project names its repo and environments once, in board settings; tickets pick from them.

## Work items (one PR each, merged on full green, in order)

### P2a — the board store, schema 2
- `board/mod.rs`: `SCHEMA_VERSION` 1 → 2 (~279). Today `open` refuses any other version
  (~465, `board_version`); add a step there: a version-1 board is migrated in one
  transaction (`ALTER TABLE tickets ADD COLUMN` ×8 below, index on `machine`, `environment`,
  `fingerprint`), `meta.schema_version` set to 2, and an `event` row `board.migrated` so the
  chain records it. Anything else unknown is still refused.
- New nullable columns on `tickets`: `machine`, `repo`, `environment`, `agent`, `session`,
  `fingerprint` (TEXT), `occurrences` (INTEGER NOT NULL DEFAULT 1), `last_seen_at` (INTEGER).
- `Ticket`, `NewTicket`, `Patch`, `Filter` (~128–240) gain the same fields (filters: equality
  on each anchor, `fingerprint`).
- Board settings (`Settings`, ~242): `repo: Option<String>` (URL),
  `environments: Vec<String>` (default `["dev","staging","prod"]`). An `environment` outside
  the list is refused with the list in `next`.
- Tests (`board/tests.rs`): a version-1 file migrates and keeps every ticket, note and event,
  and its chain still verifies; create/update/list round-trip each anchor; filters; an unknown
  environment is refused.

### P2b — MCP, CLI and contracts
- `board/mcp.rs` `CreateIn` (~574), the update and list inputs: the five anchors + `fingerprint`;
  descriptions say each is optional and filled when the hub knows it.
- `board/cli.rs`: `--machine --repo --env --agent --session --fingerprint` on create/update/list.
- If the board types are exported in `crates/contracts` (check `schemas/`), change them there
  and regenerate (`node scripts/generate-contracts.cjs`); never hand-edit `schemas/`.
- Tests: the MCP tools' output schemas validate with anchors present and absent
  (`output_schema.rs` validator); CLI JSON output includes them.

### P2c — anchors fill themselves
- In `board_create` through the hub (`board/mcp.rs` `board_create`, ~1132): when the caller
  left them empty —
  - `agent` = the token's seat (`Subject.agent`, `auth.rs` ~43);
  - `session` = the MCP session id of this connection, when the transport has one;
  - `repo` = the board's `Settings.repo`;
  - `machine` = the machine of the caller's most recent routed call in this MCP session
    (kept in the session state the hub already holds), only when there was exactly one.
- The hook pack (`board/cli.rs` `HOOK_SCRIPTS`) exports `DEVAGOTCHI_SESSION` from the agent
  CLI's session id when the CLI passes one (Claude Code's hook input carries `session_id`;
  Codex likewise) so CLI-made tickets carry it too.
- Tests: a ticket made through MCP after a `run` on one machine carries that machine, the
  seat and the board's repo; made after runs on two machines, `machine` stays empty.

### P2d — show them
- `prime` / `board_get` text output and the phone ticket page (P1c) show anchors as one
  line: `dabo · prod · github.com/x/y · claude (session …)`; the phone board page (P1c) gets
  anchor filter chips.

## Acceptance
CI green; Jesse's existing boards open after the upgrade with every ticket intact; a ticket an
AI creates after running on dabo shows `dabo`, the AI's seat and the repo without the AI
passing them.

## Not in this spec
Signals, rules and auto-created tickets (P3/P4); cross-board search.
