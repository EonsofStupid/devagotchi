# Changelog

Releases are the operator's act: a tag `vX.Y.Z` on `main` runs `.github/workflows/release.yml`
(cargo-dist: archives for x86_64/aarch64 Linux and Windows, installers, GitHub attestations).
The version in `Cargo.toml` is bumped by the operator in the same commit as this file's next
heading. Agents draft the fragments below; they never tag.

## Unreleased (would be 0.1.0 — the first release; no tag exists yet)

### The agents' board (increments 7–8)
- `devagotchi-hub board …`: a per-project ticket board in `<project>/.devagotchi/board.sqlite3`
  — hash ids `<prefix>-xxxx` with hierarchical children, workflow lanes (ready → in_progress →
  review → reviewing → done; the reviewer is never the author), claims with leases, fences,
  heartbeats, expiry and a reaper, append-only notes, a sha256 event chain, `export`/`verify`.
- The command line for agents: `init --agents-md`, `prime`, `claim`, `beat`, `note`, `done`,
  `handoff`, `release`, `force` (people only), `set`, `dep`, `held`, `hooks`; JSON off a pipe,
  exit codes by failure class, a `next` hint on every error, a claim cache so fences are never
  copied by hand.
- The board over MCP: seventeen `board_*` tools on the hub's `/mcp`, `boards` in the hub
  config, grants with `boards` and `human`, `whoami` reporting both, the fence remembered per
  subject, every call on the audit chain, lapsed leases reaped every minute.
- The hook pack: `board hooks` writes `.devagotchi/hooks/{session-start,beat,stop}.sh` and
  prints the Claude Code and Codex hook configuration — prime at session start, heartbeat
  after tool calls, no stopping while holding a ticket.
- Contract and decisions: `docs/agent-board.md` (B01–B09; B02 per-project store and B06 ACP
  for all five seats ruled in chat, ADRs pending through the component).

### Deployment (increments 7, 8, 10)
- ARM64: release builds, installers and CI on aarch64 Linux and Arm64 Windows.
- Release hardening: fat-LTO, single-codegen-unit, stripped `dist` profile; every GitHub
  Action pinned to a commit SHA; Dependabot for crates and actions.
- `install-devagotchi.sh --role hub`: the hub on a server as a user service on loopback,
  `hub.json` written once, project boards initialised, bearer tokens minted and shown once,
  Caddy in front with automatic TLS (`--caddy`). CI runs it on a real systemd host behind Caddy.

### Absorbing the Go hub (increment 9)
- `docs/devagotchi/README.md`: the ledger of what `shippinAI/agent-hub` had, what is ported,
  partial or pending, plan row D19, and the one-command import of its design documents.
