# Spec P1: the phone control app

Status: DRAFT for Jesse's approval. Plan: `docs/plan-phone-and-signals.md` (P1). Ruled
2026-11-06: control app installed from app.devagotchi.com, no app store. Paths relative to
`crates/hub/`; function names are anchors, line numbers are hints (`main` 2026-11-06).

## What the person gets
1. On the phone, app.devagotchi.com → sign in (the first-connect sign-in) → a one-line banner
   "Add to your home screen" with the right gesture for iPhone (Share → Add to Home Screen) or
   Android (the install prompt). It opens full-screen like an app afterwards.
2. **Home** (`/app`): three cards, newest first — **Needs you** (approvals waiting + tickets
   assigned to the person + machines that failed an update), **Machines** (name, online/offline
   dot, build, "updating" while it is), **Boards** (each project with its open count).
3. **Machine** (`/app/machines/<name>`): status, build, last seen, **Update now**
   (`machine_update`, shows the result), recent jobs (last 20 operations the hub routed: when,
   command's first 80 chars, state, exit code).
4. **Board** (`/app/boards/<project>`): open tickets, filters as chips (state, assignee,
   label; P2 adds the four anchors), search box.
5. **Ticket** (`/app/tickets/<project>/<id>`): title, state, priority, labels, holder,
   description, notes, events; actions **Comment**, **Assign to me**, **Close** (closes at
   once, "Undo" for 10 s), **Hand to AI** (moves it to the board's ready queue, unassigned).
6. Push: approvals (exists) and, new, "<machine> is offline" (after 10 min) and "<machine>
   update failed". Tapping opens the matching page. Push for new tickets comes with P3.

## Work items (one PR each, merged on full green; P1a first, the rest in order)

### P1a — app shell and home
- `web/app/` (new directory, one file per page, each ≤ 300 lines): `mod.rs` (routes, the shared
  frame), `home.rs`.
- The frame: `pages::app` (exists, `web/pages.rs` ~136) gains a bottom tab bar (Home, Machines,
  Boards, Approvals) and keeps one fixed frame with the page scrolling inside (Nov 2 ruling).
  Server-rendered HTML like every other page, no client framework; at most one small script
  `web/assets/app.js` (install banner, pull-to-refresh, undo timer). CSS in `web/hub.css`.
- Data: machines from the same code `machines` uses (`hub.rs` `machines`, ~638, via
  `tenant_machines::own_machines`); approvals from `web/approvals.rs` `list`'s source; boards
  from `board::mcp` `board_for`/the account's bound boards (`whoami` lists them).
- Manifest (`site.rs` `manifest`, ~575): `start_url` `/app` (was `/approvals`), add
  `"id": "/app"`, `"orientation": "portrait"`, maskable icon entry. Service worker
  (`notify::service_worker`) caches the frame and CSS so the app opens offline with "You're
  offline" instead of a browser error.
- `routes.rs`: `/app`, `/app/machines`, `/app/boards`; signed-out → the sign-in page with
  `return_to`.
- Words in `site/default/content/messages.json` under a new `app` key; nothing hardcoded.

### P1b — machine page
- `web/app/machine.rs`: page + `POST /app/machines/<name>/update` (calls the same function
  the `machine_update` tool calls; same-origin check as `approvals::same_origin`).
- Recent jobs: the hub keeps the last 20 routed operations per machine in memory (ring
  buffer beside the link sessions in `link.rs`); nothing new is stored.

### P1c — board and ticket pages
- `web/app/board.rs`, `web/app/ticket.rs`: read with `Board::list`/`Board::get`; actions via
  `Board::add_note`, `Board::update` (assignee, state). Every action is the same audited board
  call the MCP tools make, with the person as actor (`Actor::human`).
- Close + Undo: the close happens at once; Undo within 10 s reopens it (one `update`).

### P1d — push for machines
- In `link.rs`, when a linked machine has been offline 10 min, or an update reports
  `failed`, push to the account's phones through `crate::push` (as approvals do), once per
  occurrence.

## Tests (in the PRs above)
- Rust (`web/tests.rs` pattern): each page renders signed in, redirects signed out, shows
  only the account's own machines and boards (tenant test: account B sees none of A's).
- Actions: update, comment, assign, close, undo, each changes state and is in the board's
  event chain / the audit.
- Browser (`tests/app_browser.py`, Playwright, iPhone 13 and Pixel 7 emulation, added to the
  CI job that runs `tests/workflow_browser.py`): sign in → home shows a machine and a board →
  open a ticket → comment → close → undo; no horizontal scroll at 390 px; manifest has
  `start_url /app` and `display standalone`; offline reload shows the offline page.

## Acceptance
CI green; then Jesse, on his phone: installs from app.devagotchi.com, sees dabo with its build,
approves a test approval, comments on and closes a ticket, and gets a push when dabo goes
offline.

## Not in this spec
Anchors and filters by them (P2), auto-tickets and their push (P3/P4), the phone as a machine.
