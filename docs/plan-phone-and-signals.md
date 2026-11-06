# Plan: the phone app, and tickets from what machines and AIs already emit

Status: APPROVED by Jesse 2026-11-06. Each phase becomes its own code-anchored spec
(like `docs/spec-first-connect.md`) before any agent works on it. Nothing here is built yet
unless it says **exists**.

Ruled 2026-11-06 (Jesse):
- Phone first means a **control app** installed from app.devagotchi.com (no app store), not
  the phone as a machine.
- Tickets open automatically from **AI agent hooks, failed jobs, CI failures and errors in
  logs/services**: one canonical, consistent signal format, aligned with platform practice,
  where anyone can **add new sources and edit existing ones** through configuration.
- Every ticket is tied to its **machine/system, project + repo, environment, and agent +
  session**.

## What exists today (read from `main`, 2026-11-06)

| Piece | Where | State |
|---|---|---|
| Installable web app manifest, service worker, web push, push subscribe | `crates/hub/src/web/notify.rs`, `web/assets/push.js`, `crates/hub/src/push.rs` | **exists**: used for approvals |
| Approvals inbox on the web | `web/approvals.rs` (`/approvals`) | **exists** |
| Machines on the web | `web/machines.rs` (add, revoke only), `/account` | partial: no list page with status/build/update |
| The board: tickets, claim/lease, handoff, deps, notes, event chain, one board per project | `crates/hub/src/board/` (`mod.rs` `Board::create/update/list/events`), `docs/agent-board.md` | **exists** |
| Board in MCP | `board/mcp.rs` (`board_*` tools) | **exists** |
| Hook pack for Claude Code / Codex (session start → prime, tool use → beat, stop → guard) | `board/cli.rs` `HOOK_SCRIPTS` | **exists** |
| Ticket fields | `board/mod.rs` `Ticket`: kind, title, labels, parent, assignee… | no machine / repo / environment / session fields |
| Machine build + update state | `machines`, `machine_update` (#141–#144) | **exists** |
| CI webhooks, log watchers, signal rules | — | missing |

## The shape (one idea everything plugs into)

```
source ──► Signal (one canonical envelope) ──► Rules (configurable) ──► Ticket (anchored)
 hooks        { source, kind, severity,           match → open / update /        machine, project+repo,
 jobs           fingerprint, anchors{machine,      comment / close, dedupe        environment, agent+session
 CI             project, repo, env, agent,         by fingerprint, reopen         ──► phone push / board / AI
 logs           session}, summary, detail,         on recurrence, auto-close
 (new: any)     links[], at }                      on recovery
```

- **Signal** is the one canonical format (a contract in `crates/contracts`, schema generated
  like every other). Its fields follow OpenTelemetry's resource/log attribute names where one
  exists (`service.name`, `host.name`, `deployment.environment`, `vcs.repository.url`,
  `error.type`), so anything that already speaks OTel maps in without translation.
- **A source** is anything that can produce a Signal: built-in adapters plus a plain
  `POST /signals/<project>` (token-scoped) and `devagotchi signal emit …` for everything else.
  Adding a source = adding a config row, not code, unless it needs a new adapter.
- **Rules** live per project in the board's settings (`Board::put_settings`, already in the
  event chain, so every edit is recorded and undoable). Defaults ship in
  `crates/hub/site/default/content/signal-rules.json`; a project overrides or adds. Editable
  from MCP (`signal_rules_list/put/delete`), the CLI, and the phone app.
- **Tickets** gain first-class anchors (machine, project, repo, environment, agent, session),
  filterable everywhere, and a `fingerprint` so one recurring failure is one ticket with a
  count, not a hundred.

## Phases (each its own spec, then one agent, one PR per item, merged on full green)

### P1 — The phone app (control)
Server-rendered pages in the hub's existing style (`web/pages.rs`), phone-first, one fixed
frame with pages scrolling in (Jesse's Nov 2 UI ruling), installable from the existing manifest:
1. `/app` home: machines (status, build, update, last seen) · approvals waiting · tickets that
   need you · offers state. Install prompt for iPhone (Share → Add to Home Screen) and Android.
2. `/app/machines/<name>`: status, build, update now (`machine_update`), recent jobs.
3. `/app/boards/<project>` and `/app/tickets/<id>`: list with filters by the four anchors;
   detail with notes; actions: comment, assign, close, hand to an AI.
4. Push for: approvals (exists), new auto-tickets above a severity the person sets, a machine
   going offline or failing an update.
Acceptance: Jesse installs it on his phone from app.devagotchi.com and approves, reads and
closes a ticket from it.

### P2 — Anchored tickets
1. Board store migration: anchor columns + `fingerprint` + `occurrences` + `last_seen_at`.
2. `board_create/update/list` and the CLI take and filter by anchors; `Ticket` contract grows.
3. When an AI creates a ticket through the hub, anchors fill themselves from what the hub
   knows (machine of the call, project bound to the board, the client's agent and session).

### P3 — Signals and rules
1. `Signal` contract; `POST /signals/<project>`; `devagotchi signal emit`; MCP `signal_emit`.
2. Rule engine in `crates/hub/src/signals/` (new module beside `board/`): match on source,
   kind, severity, anchors, text; actions open / update (bump count, add note) / reopen /
   close; dedupe by fingerprint; rate limit per source.
3. Rules as board settings with defaults from the site directory; MCP + CLI to list, add,
   edit, delete; every change in the event chain.

### P4 — Built-in sources, in this order
1. **AI agent hooks**: extend the existing hook pack (and add Cursor and Gemini rows from
   `docs/agent-board.md` §7): `Stop` without completion, `PostToolUse` failures repeated N
   times, `SessionEnd` mid-ticket → `devagotchi signal emit`. Session ids ride along, so the
   ticket can resume the same conversation.
2. **Failed jobs**: when an operation the hub routed settles failed, timed out or was killed,
   the hub emits a Signal (machine + project from the call). Later: brokers report settles of
   locally started jobs over the link.
3. **CI failures**: per project, link a repo; GitHub webhook `POST /hooks/github/<project>`
   with the repo's secret (stored as a secret, never shown); `workflow_run` failed → Signal,
   success on the same workflow → auto-close.
4. **Logs and services**: broker-side watchers configured per machine (systemd unit failed,
   Windows service stopped, a file tail matching a rule), reported over the link. Last,
   because it is the only one that adds work on the machines.

### P5 — Make it consistent everywhere
`llms.txt` and the MCP instructions describe signals and anchors; the landing and docs list
sources; every source has the same test shape (fixture in → expected ticket out).

## Decisions still open (asked one at a time, when their phase is specced)
- P1: does closing a ticket from the phone need a confirmation tap? (proposal: no; undo for
  10 s.)
- P3: who may edit rules on a shared board — the board's owner only, or any member?
- P4.3: GitHub App (one install, many repos) or a webhook per repo (no app to register)?
- P4.4: which log sources first on Windows (Event Log, services) vs Linux (journald, units)?

## Order
P1 → P2 → P3 → P4.1 → P4.2 → P4.3 → P4.4 → P5. P1 can run beside P2 (different files).
