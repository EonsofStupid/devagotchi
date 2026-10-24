# Devagotchi agent board: tickets any subscription can claim, work, annotate and hand off

Revision: **2026-10-24 UTC**. Status: **contract proposed; store implemented (increment 7); MCP, CLI and UI pending**.
Owner: `crates/hub/src/board/`. This document owns the board contract; the
[architecture master](product-and-platform-plan.md) owns product decisions and sequencing; the
[task-graph contract](task-graph-and-mesh-contract.md) owns execution graphs, which are a different thing.

## 1. What a ticket is, and is not

A **ticket** is a unit of coordination and memory between people and agents: "port the claim
protocol", "rotate the key on the SMB share", "review Codex's installer PR". It is claimed under a
lease, worked, annotated, handed off, reviewed by someone else, and closed. It travels with the
project. Its history is a hash-chained event log any reader can verify.

A **broker operation** (and, when the task-graph packets land, a task graph) is a unit of
execution: one admitted command with its own identity, output and settlement. A ticket may link to
the operations that worked it; it never admits, schedules or cancels one. The hub never owns jobs
(ruling 2026-10-21). The two meet only through an attempt link: "ticket X was worked by operation Y
on machine Z", recorded after the fact by the agent or the broker.

The Go `agent-hub` (shippinAI/agent-hub, Postgres) proved this shape on one box in October 2026:
leases with fences, a ready queue over dependencies, reviewer ≠ author as data, notes and handoffs,
an append-only chain. The ruling moves it into this hub, in Rust, on SQLite, per project. This
increment ports the protocol; what it adds comes from what works elsewhere (§7).

## 2. Boundary test (master §4)

| Question | Answer |
|---|---|
| Responsibility it owns | Tickets, dependencies, claims/leases, notes, handoffs, the board's event chain, per project. |
| Existing owner inspected | `crates/hub` (bastion: subjects, audit chain, MCP surface, web); `crates/core/journal` (operations). The Go `agent-hub` store (`server/internal/store`, `docs/spec.md`). |
| Why extension is adequate | The hub already authenticates every actor, already keeps a hash-chained audit, already is the one MCP front door, already opens SQLite. A board is tables, tools and routes inside it; no new process, service or daemon. |
| What it replaces | The Go hub's tasks/notes/deps/leases/events, ported not run beside; the Go hub freezes behind a superseded banner once parity tests pass (increment 11). |
| Authoritative state | `<project>/.devagotchi/board.sqlite3`, one writer, WAL. One board per project; the hub opens boards on demand for projects it is told about. `board.jsonl` (increment 8) is an export for viewers and git diffs, never the truth. |
| Dependencies and failure | `rusqlite` (already a workspace dependency). A missing or locked project directory makes that board unavailable; no other project's board is affected. The chain is verified on health checks; a broken chain is reported, never repaired silently. |
| Acceptance | `crates/hub/src/board/tests.rs`: lanes, fencing after expiry, reaper → attention, readiness through `blocks`, assignee priority, WIP, handoff, force-release, version conflicts, transitions, filters, append-only triggers, chain tampering, workflow replacement, discovered work. |

Rejected on the way: a board per machine (tickets outlive machines), a board in the hub's state
directory keyed by project (does not travel with the repo, cannot be read offline by a local
agent), Dolt or another versioned database (no embedded Rust implementation; git holds the export).

## 3. Records

```text
Ticket {
  id: "<prefix>-<hash>" | "<parent-id>.<n>",   -- hash ids never collide across machines; children are hierarchical
  kind: "task" | "epic" | "message",
  title, description, state, priority (lower = sooner), labels[],
  parent: Id | null, assignee: Principal | null,
  holder: Principal | null, lease_id, lease_ttl_ms, lease_expires_at, heartbeat_at,
  fence: integer (global, strictly increasing, never reused),
  version: integer (optimistic concurrency), expiry_count, last_worker,
  progress: JSON | null, created_by, created_at, updated_at, closed_at,
  blocked_by[]: computed (open `blocks` dependencies), lease_expired: computed
}
Dep   { ticket, depends_on, kind: blocks | parent | related | discovered-from | duplicates | supersedes }
Note  { seq, ticket, author, kind, body, to_principal, fence, created_at }          -- append-only
Event { seq, at, actor, action, ticket, data, fence, prev_hash, hash }              -- append-only, hash-chained
Workflow { states[{key,label,kind}], lanes[{queue,active,complete_to,release_to,distinct_from_previous}], transitions[], initial_state, attention_state }
Settings { lease_ttl_ms, lease_min_ttl_ms, lease_max_ttl_ms, max_lease_expiries, wip_limit_default, default_priority }
```

Only `blocks` affects readiness. `parent` gives epics their children and hierarchical ids
(`deva-a3f8`, `deva-a3f8.1`, `deva-a3f8.1.1`). `discovered-from` records work an agent found while
doing something else, so nothing is lost and nothing is invented. Workflow and settings are data
rows in the board, validated before they land; state *kinds* (`queue`, `active`, `review`,
`blocked`, `done`, `attention`) are what the code reasons about, names are the operator's.

## 4. Claim and lease protocol (ported; the Go hub's `docs/spec.md` §3 holds)

1. `claim(id, ttl?)` / `claim_next(filter)`: one transaction. A candidate is a lane queue state
   (or a lapsed lease), has no open `blocks`, is unassigned or assigned to the caller. Checks the
   lane's `distinct_from_previous` (reviewer ≠ author), then WIP (`actor.wip_limit ??
   settings.wip_limit_default`; 0 = unlimited). Sets holder, a fresh lease id, `lease_expires_at =
   now + ttl` (clamped to the settings' bounds), `fence = next`, `state = lane.active`, `version + 1`.
   Re-claiming what you hold returns the current lease, nothing changes.
2. `heartbeat(id, fence, progress?)`: holder and fence must match, lease must be live; extends by
   the lease's ttl. Recommended interval: `heartbeat_every_ms` = ttl/3.
3. Every mutation of a held ticket carries the current fence. Wrong fence → `stale_fence`; not the
   holder → `not_holder`; lapsed → `lease_expired`.
4. Expiry (reaper, or lazily inside a claim): back to the lane's `release_to`, `expiry_count + 1`,
   event `lease.expired`; at `max_lease_expiries` the ticket goes to the workflow's attention
   state. The fence is burned, so a worker that wakes up late is rejected, not merged.
5. `release(id, fence, reason)` → `release_to`. `complete(id, fence, summary)` → `complete_to`,
   `last_worker = me`, summary as a note. `handoff(id, fence, note, to?)` = handoff note +
   release + assignee, one transaction.
6. `force_release(id, reason)`: a human principal (code rule) with a reason ≥ 3 characters; the
   event keeps the previous holder and fence.
7. `update(id, version, patch)`: optimistic concurrency; a state change must be a declared
   transition, never into an active state, never on a held ticket (`use_lease_ops`).
8. Every mutation appends its event in the same transaction; the chain is
   `sha256(prev ‖ canonical{seq, at, actor, action, ticket, data, fence})`.

Error codes are stable across CLI, MCP and REST: `not_found`, `invalid`, `held_by_other`,
`not_claimable`, `blocked`, `distinct_required`, `wip_limit`, `stale_fence`, `not_holder`,
`lease_expired`, `version_conflict`, `transition_not_allowed`, `use_lease_ops`, `humans_only`,
`dependency_cycle`, `parent_closed`, `board_mismatch`, `board_version`.

## 5. Where the board lives and how an agent starts it (increment 8)

```text
<project>/
  .devagotchi/
    board.sqlite3        the truth (git-ignored; one writer at a time: the CLI as the OS user, or the hub)
    board.jsonl          `board export`: one JSON line per ticket with its deps and notes (commit it if you want diffs)
    config.json          project name and id prefix; later the hub endpoint and which seats may claim here
    claims/<actor>.json  the CLI's claim cache ({id → fence, version, lease expiry}) so agents never copy fences
    .gitignore           ignores the database, its WAL and the claim caches
  AGENTS.md              gains one fenced block (markers `devagotchi-board:start/end`) pointing at `board prime`
```

`devagotchi board init [--project NAME] [--prefix P] [--agents-md]` (increment 8a, built; `devagotchi board` since 13b) creates
the directory and, with `--agents-md`, appends the block once. `board prime` is what an agent
runs, or has run for it, at session start: under fifty lines — the tickets it holds (fence, lease
left), what is ready for it, tickets that need a person, notes addressed to it, and the one-line
protocol. `board onboard` prints the block for pasting elsewhere; `board agent` prints the whole
workflow. Identity on this surface is the OS user (the board file is theirs, as the broker's trust
scope is): `--as` or `DEVAGOTCHI_ACTOR` names an agent, which may not force-release; the bare OS
user is the person. The same directory, named in the hub's `boards` config, is what the hub's
`board_*` tools open (§6). Still to come in 8b: registering the project with the hub from here,
the MCP snippets for each CLI, and `--hooks` for the hook pack (§7).

## 6. Surfaces (increment 8)

Two faces, one store. The command line (§5) is for agents on the host that holds the project;
the hub's MCP tools are for agents anywhere. Both open `<project>/.devagotchi/board.sqlite3`
(WAL, five-second busy timeout), so an agent on the hub's host and one across the world see one
board, one chain.

**MCP tools on the hub's `/mcp` (8b, built).** The hub config names the boards it serves and who
may work on them, the way it names brokers:

```json
"boards": [ { "project": "devagotchi", "dir": "/srv/projects/devagotchi" } ],
"grants": [
  { "subject": "300000000000000001", "boards": ["devagotchi"], "human": true },
  { "subject": "claude-on-gus",      "boards": ["devagotchi"] }
]
```

`project` must be the name the board was initialised with (ids embed its prefix; a mismatch is
refused at first use), `dir` is absolute, and an uninitialised directory is `no_board`, never
silently created. A hub may serve boards and no broker, or nothing yet (whoami and `/healthz`
still answer, so an installer can bring it up before its first board or machine). The actor is the authenticated subject;
it is a person only when its grant says `human` or it is the owner (`whoami` reports `human` and
`boards`). Agents never are, whatever they claim. No grant, no board: `not_granted`; a project the
hub does not serve: `unknown_board`.

Seventeen tools, each taking `project`: `board_prime` (start here), `board_ready`, `board_list`,
`board_get`, `board_create`, `board_claim` (by id, or the best ready ticket when `id` is omitted;
`claimed: false` when nothing is ready), `board_heartbeat`, `board_update`, `board_note`,
`board_handoff`, `board_release`, `board_complete`, `board_dep`, `board_force_release` (people
only), `board_events`, `board_wait` (long-poll on the event feed, at most 25 s), `board_verify`.
Outputs are the store's records with JSON Schemas; a refusal is `{code, message, ticket?, next}`
with the store's stable code (§4) and what to call next, as the CLI's `next` does. The fence may
be omitted on a held ticket: the hub keeps the fence it last handed that subject for that ticket
(claim or heartbeat), as the CLI's claim cache does, and forgets it on complete, handoff or
release; the store still checks whatever is passed, so a lost lease is refused either way. Every
call lands on the hub's audit chain (`tool`, the project under `broker`, the ticket under
`operation_id`, the outcome), and a lapsed lease is reaped every minute as well as lazily on claim.
`tests/board_probe.py` works a board as three subscriptions (a person, two agents) and then reads
it back with the command line; `tests/e2e.py` runs it.

**The hook pack (8b, built).** `devagotchi board hooks [--for claude-code|codex|all]` writes
three POSIX scripts under `.devagotchi/hooks/` and prints the hook configuration to add:
`session-start.sh` runs `board prime --text` (its stdout is the session's first context),
`beat.sh` runs `board beat` (every held lease, at most once a minute so the chain does not fill
with heartbeats), `stop.sh` refuses a stop while the actor holds a ticket (exit 2, which Claude
Code and Codex treat as "do not stop, here is why") once, and allows the second attempt so an
agent that cannot finish does not loop — the lease then lapses and the reaper requeues the
ticket. The actor name is the scripts' argument, so one pack serves every CLI; Claude Code
(`.claude/settings.json`) and Codex (`.codex/hooks.json`, feature `codex_hooks`) get printed
snippets; the others wire the same scripts to their own session-start / after-tool / stop
events. `board held` and `board beat` with no id exist for the pack. The skill that teaches all
of this to a seat lives in agent-skills (`hubs/intelligence/devagotchi-board/`), with the MCP
connection snippet per CLI.

Not built yet from the 8b list: `request_id` idempotent retries on writes (the store's version
check and `already_held` cover the common retries) and project registration from the CLI
(`board init --hub`). They stay in packet 8b.

## 7. What the CLI agents can do now, and what the board does for them

Re-probed 2026-10-24 against vendor documentation (chatmaton `docs/51` of 2026-08-15 is stale on two
rows):

| Seat | Multi-turn without a new process | Resume across processes | Hooks | ACP registry |
|---|---|---|---|---|
| Claude Code | yes (stream-json) | `--resume` | 30+ events incl. `SessionStart`, `PreToolUse`, `PostToolUse`, `Stop`, `TeammateIdle`, `TaskCreated/Completed` | Claude Agent (wrapper) |
| Codex | yes (`codex app-server`) | `codex resume` | `SessionStart`, `PreToolUse`, `PostToolUse`, `Stop` (`decision: block` forces continuation) behind `codex_hooks = true` | `codex-acp` |
| Grok | yes (`grok agent stdio`, ACP v1) | `session/load` | — | Grok Build |
| Gemini / Antigravity `agy` | **yes**: `--input-format stream-json` (requires `--output-format stream-json`) | `--continue`, `--conversation <id>` | `.agents/hooks.json`, `transcript.jsonl` | Google Antigravity; Gemini CLI |
| Cursor `agent` | **yes**: hidden `agent acp`; otherwise `create-chat` + `--resume <id>` | `--resume`, `--continue` | `sessionStart`, `stop`, `postToolUse`, `afterFileEdit`, `afterShellExecution` (two response hooks do not fire headless, acknowledged March 2026) | Cursor |

Consequences the board builds on:

- **One seat driver.** All five seats, and 37 more, are in the ACP registry. ACP gives an
  orchestrator `session/new` (with the board's MCP endpoint handed over in the handshake),
  `session/prompt`, `session/update`, `session/request_permission`, `session/cancel`. chatmaton
  already speaks it for grok; the devagotchi broker runs the ACP process as a job. "One-shot
  seat" stops being a category; what remains per vendor is a config row and the policy question
  below.
- **Liveness is the broker's job, not the agent's.** When a seat works a ticket as a broker
  operation, the hub heartbeats the lease while the operation is alive and releases or hands off
  when it settles. An agent that cannot remember to heartbeat still cannot lose a lease by
  forgetting. Symphony does the same with its workspace reconciliation; this is the inverted form.
- **Hook pack, same four hooks on every CLI that has them:** `SessionStart` → `board prime` as
  injected context; `PostToolUse` → `board beat` (debounced to a third of the ttl); `Stop` → the
  GUPP guard: a held ticket without a handoff or completion blocks the stop with the reason
  "hand off or complete <id>"; `TeammateIdle` (Claude teams) → `board claim_next`. Gas Town's
  "if there is work on your hook, run it" as four shell lines.
- **One skill, spec-pure.** `.agents/skills/board/SKILL.md` under the Agent Skills standard
  (Claude Code, Codex, Gemini CLI, Cursor, Kiro, 32+ tools as of May 2026), validated by the
  `agent-skills` repo's CI; per-surface extras live beside it, never in the frontmatter.
- **Agent-friendly CLI contract.** JSON output whenever stdout is not a terminal; `--quiet`;
  distinct exit codes per failure class (0 ok, 2 conflict, 3 refused, 4 not found, 5 unavailable);
  every error carries `next` (what to run); creates are idempotent by `--request-id`; `board
  agent` prints the workflow offline with progressive disclosure. What Speakeasy and Nx measured
  on their own CLIs in 2026: fewer turns, and skills that fire without helping cost more than no
  skill.
- **Vendor sessions are foreign keys.** A ticket records the vendor conversation that worked it
  (`agy --conversation`, `agent --resume`, `codex resume`, `claude --resume`), so the next turn on
  that ticket resumes the same conversation; the board holds the id, never the token
  (chatmaton CM-0.4, CM-0.5).

Policy, stated plainly: Antigravity's own forum carries five unanswered threads (September 7 to
October 19, 2026) asking whether a local orchestrator may launch `agy` under a consumer account; the
one reply that says "fully supported, same entitlements" is not identified as Google staff. Cursor,
Codex and Claude document their headless modes. Treat `agy` orchestration as technically proven and
policy-unconfirmed until Google answers; the seat's config row carries that flag.

## 8. Decision register (a RULED row becomes canon only as `docs/adr/NNNN-*.md` saved through Jesse's ADR component)

| ID | Status | Decision |
|---|---|---|
| B01 | LOCKED | The agents' board lives in this hub, in Rust, per project, on SQLite; the hub never owns jobs (ruling 2026-10-21). |
| B02 | RULED 2026-10-24 (chat; ADR pending through the component) | Store placement: `<project>/.devagotchi/board.sqlite3`, hub-opened on demand, `board.jsonl` export. Rejected: one database in the hub's state directory keyed by project. Plan D23 (same day) adds: the local file stays the board of record and syncs to the account's hosted hub when signed in; the hosted hub keeps one board per account-project (packet 15 in plan §13). |
| B03 | PROPOSED | Ids: prefix + hash for roots, hierarchical for children. Alternative: counters (the Go hub), rejected for multi-machine creation. |
| B04 | PROPOSED | Dependency kinds `blocks | parent | related | discovered-from | duplicates | supersedes`; only `blocks` gates readiness. |
| B05 | PROPOSED | Liveness by broker: an operation working a ticket keeps its lease alive; settlement releases or hands off. |
| B06 | RULED 2026-10-24 (chat; ADR pending through the component) | All five seats are driven over ACP, one driver with a config row per vendor; `agy` runs only behind its policy-unconfirmed flag until Google answers. |
| B07 | PROPOSED | Hook pack (prime / beat / GUPP stop guard / idle → claim_next) installed by `board init --hooks`. |
| B08 | DEFERRED | Per-state WIP caps, per-principal budgets, message tickets with threads, path reservations with a pre-commit guard (increment 9). |
| B09 | DEFERRED | Board pages in the hub's web and the one-tap Approve from the agent-board plan (increment 10). |
| B10 | RULED 2026-10-24 (chat; ADR pending through the component) | Approvals reach the user by phone push and email first, on both iPhone and Android; Slack and Discord follow as one-click connects. Which channels, in what order and for which kinds of request are per-user settings whose defaults need no setup. Every approval step (requested, seen, approved or denied, running, verified, done or rolled back) is an event the requesting agent continues from; nothing polls. |

## 9. Packets

| Increment | Scope | Exit |
|---|---|---|
| 7 (this one) | `crates/hub/src/board/`: schema, ids, workflow, store, leases, reaper, chain; this document | `cargo nextest run -p devagotchi-hub` green; §2 acceptance list |
| 8a (built) | `devagotchi board` CLI (`devagotchi-hub board` until 13b): `init` (+ AGENTS.md block), `prime`, `onboard`, `agent`, `ready`, `ls`, `show`, `new`, `claim`, `beat`, `note`, `done`, `handoff`, `release`, `force`, `set`, `dep`, `reap`, `verify`, `export`; JSON off a pipe, `--quiet`, exit codes by class, `next` on every error, claim cache | five CLI tests: init idempotence, the loop from a subdirectory with a cached fence, exit codes and hints, `prime` ≤ 50 lines, export + verify |
| 8b (in) | MCP tools on `/mcp` (one board per `project`; **built**, §6: `boards` config, grants with `boards`/`human`, 17 tools, fence memory, audit, reaper, `tests/board_probe.py`); the hook pack (`board hooks`, **built**); the skill with the MCP snippet per CLI (**built**, agent-skills); still open: project registration with the hub, `request_id` | `tests/e2e.py` works a board over MCP as a person and two agents (**green**); the hook scripts exercised against the CLI (stop refused once while holding, beat throttled) in the CLI tests and by hand; on Claude Code and Codex themselves: open |
| 9 | Broker liveness (B05), attempts link, message tickets, path reservations, per-state WIP, budgets | A lease survives an agent that never heartbeats while its operation runs; two agents cannot edit one reserved path |
| 10 | Web: `/board`, `/board/<id>`, Approve; approval events; phone push (installable web app, iPhone and Android) and email (B10) | Phone-width pages served by the existing axum app; a request an agent raises reaches a phone and an inbox, one tap approves, and the agent continues on the event |
| 11 | Freeze the Go `agent-hub` | Parity tests pass here; banner there |
