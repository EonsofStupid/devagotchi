# The Devagotchi product design, absorbed from shippinAI/agent-hub

The 2026-10-21 ruling put the agents' board in this hub; this directory puts the design beside
the code. The source is `shippinAI/agent-hub` at `ca626db` (its last push, 2026-10-20):
`docs/devagotchi/01`–`39g` and `adr/`, its `docs/devagotchi-spec.md` (the index; keep it at
`../devagotchi-spec.md` so the links inside the files still resolve), `docs/spec.md` (keep as
`go-hub-spec.md`) and `docs/connect.md` (`go-hub-connect.md`).

**The copy itself is one command, run by the operator** (a session may not move one
repository's files into another on its own):

```bash
# both clones side by side
cp -r ../agent-hub/docs/devagotchi/. docs/devagotchi/
cp ../agent-hub/docs/devagotchi-spec.md docs/devagotchi-spec.md
cp ../agent-hub/docs/spec.md docs/devagotchi/go-hub-spec.md
cp ../agent-hub/docs/connect.md docs/devagotchi/go-hub-connect.md
git add docs && git commit -m "docs: import the Devagotchi design from shippinAI/agent-hub@ca626db"
```

Branches from the 2026-10-21 pass (Dockerfiles/compose, approvals inbox, release configs) were
never pushed there; if they carry doc changes, bring those too.

## What governs what

- **This repository is the one home.** `shippinAI/agent-hub` is frozen for reference; nothing
  new lands there.
- **Where these documents and the plan disagree, the plan wins:**
  [product-and-platform-plan.md](../product-and-platform-plan.md) (D01–D19, G0–G5, the boundary
  test), [agent-board.md](../agent-board.md) (B01–B09) and
  [task-graph-and-mesh-contract.md](../task-graph-and-mesh-contract.md). Everything these
  documents cover that the plan does not — subscriptions, providers, guides, costs, the
  approvals PWA, GUI control, the chat page, the public product — they govern, until a plan
  packet or an ADR through Jesse's component says otherwise.
- **Decisions.** `10-decisions.md` D1–D21 and `adr/0001`, `adr/0002` were Jesse's answers in
  October 2026 and stand. Two of them collide with the plan and need a ruling (ledger rows
  marked **conflict**; plan row D19). Resolved rows become `docs/adr/NNNN-*.md` through the
  ADR component.
- **Code paths** inside these files (`server/internal/…`, `bridge/…`, Postgres tables) describe
  the Go implementation they were written against. The ledger says what each became.

## Absorption ledger — Go `agent-hub` → this repository

| Go capability (package · tables) | Here | Status | Lands in |
|---|---|---|---|
| Tasks, deps, notes, leases, fences, events chain (`store` · `tasks task_deps notes events idempotency_keys`) | `crates/hub/src/board/` | **ported**, extended (hash ids, epics, six dep kinds, per-project SQLite) | PR #19, increment 7 |
| Workflows, settings, presets as data (`workflows settings`, `presets`) | board `workflow.rs`, settings rows | ported | increment 7 |
| MCP tools (13: whoami … wait_for_updates, review_ticket) (`mcptools`) | hub `/mcp` tool router | not yet | increment 8 |
| `hub` CLI with claim cache; `GET /api/v1/protocol` AGENTS.md snippet | `devagotchi-hub board …`, `board prime/onboard` | not yet | increment 8 |
| REST `/api/v1` + SSE `/stream` (`httpapi`, `stream`) | hub axum routes | `/api/v1/me`, `/healthz` only | increments 8 (API), 10 (SSE) |
| Web UI: Board, TaskDetail, NewTask, Agents, Tokens, Connect, Policies, Roles, Workflows, Settings, Audit, Actions, Retention, Login (React Aria SPA) | server-rendered pages in `crates/hub/src/web/` + the approvals PWA (39e) | `/account` only | increment 10; **decision**: port the SPA or stay server-rendered + PWA |
| Principals and per-agent bearer tokens (`principals tokens`, `auth`) | subjects from Nomen JWT; static `auth.tokens_file` | partial: no issue/revoke UI, Nomen only | increment 12: identity sources (per-agent tokens, any OIDC, Cloudflare Access, Tailscale) |
| Machines, enrollment codes, presence (`machines`, `bridgehub`) | `devices.rs` (enroll/revoke), `link.rs` (outbound WebSocket) | ported | PRs #7, #12 |
| Machine operations through the bridge (`machineops`, `proc_leases`) | hub routes `operation_*` to brokers by name over the link | ported (process operations) | — |
| Grants, access requests, presets, sandbox policies, capability risks (`access`, `grants access_requests grant_presets sandbox_policies capabilities capability_risks`) | hub config `grants` (static) | partial: data tables and the one-tap request/approve flow missing | increment 14 (bridge plan step 2) |
| Policy rules, deny by default (`policy`, `policyrules`, `policies`) and the gold-policy PDP client (`policygate`) | — | not ported | increment 13 (with factory-board F1, the actuation gateway) |
| Change policy: policy docs, change tickets, evidence, reviews, reviewer agents (`changepolicy`, `policy_docs change_tickets ticket_evidence ticket_reviews`) | board review lane (reviewer ≠ author) | partial | increments 10 (Approve with hash) and 13 |
| Action feed and audit retention (`actionfeed`, `auditkeep`, `action_events audit_categories audit_archives`) | `audit.rs` chain of routed calls | partial: no per-action feed, no retention policy | increment 18 |
| Catalog CRUD with JSON-Schema validation (`catalog`, `schemacheck`, `docker_runtime_presets shell_profiles router_endpoints …`) | — | not ported | increment 15 |
| Guides (`guides`, `guidekit`, `guides guide_versions guide_runs guide_updates`) | — | not ported | increment 16 |
| Subscriptions, role bindings, write-only secrets (`subs`, `secretbox`, `subscriptions role_bindings secrets`) | — | not ported; Vaultix (36) is the secret home | increment 17 |
| Provider schema v1 and the 18-row matrix (`providers`, `providers provider_models`, docs 29/34) | — | not ported; the board's B06 ACP driver is its runtime half | increment 17 |
| Tenants, chat apps, resume acks (`tenants chat_apps resume_acks`) | — | hosted tier; not ported | with Shippin's Bundled Stack row |
| Bridge: 28 Desktop Commander-parity tools (files, search, processes, PDF/DOCX/XLSX, config) in Go | `crates/core` broker: process execution + journal; files/search/terminal are G3/G4 packets | **partial** | G3 (files), G4 (terminal); format tools after |
| Bridge sandbox, grants, doctor, install, GUI control (docs 02/03/11) | Linux cgroup ladder + Windows Job Objects in `core`; installers in `deploy/` | partial; GUI control **conflict** (below) | G2; decision |
| Approvals PWA, push, passkey signing, self-update (39a, 39e, 39g) | — | not ported; the "phone bridge" packet | increment 19 |
| Chat page, rooms, routines (24, 39d) | chatmaton (separate repo) | out of this repo | chatmaton board |

**Conflicts to rule on (plan row D19):**

1. GUI control — Go D7 "yes, from v0.1" versus plan D18 "DEFERRED: screen/input automation".
2. The bridge binary — Go `bridge/cmd/devagotchi` (28 tools, Go) versus this workspace's
   `broker` + `link` + `mcp` (Rust, process operations). The 2026-10-21 bridge plan already names
   the Go bridge as "what it replaces"; its tool list is the parity target, not its code.

## Reading order, once imported

Start at `../devagotchi-spec.md` (the index), then `10-decisions.md`, `27-gap-tracking.md` and
`28-public-gaps-and-phases.md` for the registers; `39*` for the Gold build plan. The plan's phases
(G0–G5) and the board's packets (7–11) are the schedule; `09-phases.md` and
`39-chatmaton-gold-build-plan.md` are the product sequence they serve.
