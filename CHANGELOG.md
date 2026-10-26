# Changelog

Releases are the operator's act: a tag `vX.Y.Z` on `main` runs `.github/workflows/release.yml`
(cargo-dist: archives for x86_64/aarch64 Linux and Windows, installers, GitHub attestations).
The version in `Cargo.toml` is bumped by the operator in the same commit as this file's next
heading. Agents draft the fragments below; they never tag.

## Unreleased (would be 0.1.0 — the first release; no tag exists yet)

### Board waits wake on the change itself (increment 26b)
- Every board write that records an event advances the board's change signal after it
  commits. `board_wait` and `approval_wait` sleep on it instead of re-reading the store
  every 500 ms, so a note, a claim or a person's decision on the phone reaches the waiting
  agent the moment it lands. A write that records nothing (an empty reap) wakes nobody.
- Lapsed leases are expired when they lapse: the reaper sleeps until the first lease on
  any open board ends, or until a board changes, instead of sweeping every 60 s.

### Waiting, not polling: `operation_wait` (increment 26a)
- The broker answers `operation_wait` the moment an operation settles (or, with
  `output_after`, has output past that sequence), or after `timeout_ms` (at most 50 s)
  unchanged. It sleeps on one change signal that every state change, output frame and
  cancellation advances; nothing is re-read on a timer. At most 16 waits are held at once
  (`wait_busy` beyond), so ordinary requests always get a connection.
- Local IPC and the machine link allow a held request its wait on top of their usual
  timeouts; the link keeps waits in their own pool of 8 so they never crowd out calls.
- The hub's `run` makes one held request instead of asking every 50–500 ms, and both the
  hub and the stdio adapter offer `operation_wait`, so an AI waits in one call instead of
  calling `operation_get` again and again. The broker's own tests and the protocol and
  end-to-end harnesses settle operations through it.

### Changing files: `write_file`, `edit_file` (increment 24e)
- `write_file` overwrites (the default: written beside the file and renamed into place, so
  a reader never sees half of it; permissions carry over), creates (refused when it exists)
  or appends, up to 32 KiB per call, from text or base64, optionally creating parent
  directories (under `granted`, only below an ancestor inside the workspace). With
  `expected_sha256` it refuses (`changed_since_read`) when the file changed since.
- `edit_file` replaces exact text in a UTF-8 file (up to 8 MiB) only when it occurs the
  expected number of times (default once); otherwise nothing changes and the answer says
  how many times it occurs. Both return the file's new size and SHA-256.

### Files in one call: `read_file`, `list_directory` (increment 24d)
- The broker answers `file_read` and `file_list` directly (no operation, nothing
  journaled), under the same reach as execution: inside the workspace under `granted`,
  wherever its account can read under `account`. A read returns up to 24 KiB from an
  offset (negative: from the end), as text when it is UTF-8 text (never splitting a
  character) and as base64 otherwise, with `next_offset` and `eof`. A listing goes up to 3
  levels and 1000 entries, directories first, symbolic links listed and never followed.
- The hub's `read_file` and `list_directory` tools route them by machine name, audited
  per call with the path.

### `run`: a command and its output in one call (increment 24c)
- The hub's `run` tool takes a machine, a command line and optional `cwd`, `wait_seconds`
  (1–50, default 30) and `timeout_seconds` (1–300, default 300). It starts the command as a
  broker operation, waits, and returns its state, exit code and output (stdout and stderr in
  order, the last 32 KiB). A command still running when the wait ends keeps running and
  comes back with its `operation_id` and how to follow it. One audit entry per `run`. It
  needs a machine whose broker runs under the `account` policy; a `granted` one refuses it
  with `command_not_allowed`. The hub's instructions now send an AI to `run` first.

### Commands as the account (increment 24b, ruled 25 Oct)
- A broker's `exec_policy` (in `broker.json`) is `granted`, the default and the old
  behaviour, or `account`: the broker runs anything its OS account can, in any directory,
  with that account's environment, and takes a `command` line for the machine's shell
  (`$SHELL` when it is bash, zsh, sh, dash or ksh, else bash, else `/bin/sh`; Windows
  PowerShell with `-EncodedCommand`). `operation_start` carries `command` or `executable` with
  `args`, never both; a `granted` broker refuses `command` with `command_not_allowed`.
  The account's permissions are the boundary: a broker that takes commands runs as an
  unprivileged account. Approvals for risky steps and the installers' default follow.

### Bounded history, not a lifetime quota (increment 24a)
- A broker kept every operation it ever admitted and refused all new work once it held
  `max_operations` (64 by default), so a machine stopped taking commands after its 64th.
  Now the operation that finished longest ago is retired to make room; admission is refused
  only while every held operation is still running. A retired operation's ID answers
  `not_found`, and its idempotency key is remembered (the newest 4096) and refused with
  `idempotency_key_retired`, so a late retry never runs the work twice.

### One binary (increment 13a)
- `devagotchi`: one organised command tree over the same libraries — `devagotchi start`
  (this user's broker; the first run writes `broker.json` with the workspace and the
  toolchains on PATH), `devagotchi mcp` (the adapter an agent runs; finds that broker through
  `<state>/endpoint`), `devagotchi board …`, `devagotchi broker run …`, `devagotchi hub
  serve|enroll|revoke|devices`, `devagotchi link enroll|run`, `devagotchi version`.
- The clean cut (13b): `devagotchi` is the only binary the release ships and the installers,
  harnesses, CI and docs call (`bridge-broker`, `bridge-mcp`, `devagotchi-link` and
  `devagotchi-hub` are gone; their crates are libraries). The installers' units run
  `devagotchi broker run`, `devagotchi link run` and `devagotchi hub serve`; Claude Desktop
  and Claude Code are registered with `devagotchi mcp`.

### The account (increment 14a, D22)
- `devagotchi login`: sign in with a Shippin account from a terminal with no browser —
  OAuth device authorization (RFC 8628) against Nomen; the code and URL are shown, the
  tokens land in `account.json` under the config directory, owner-only. `devagotchi whoami`
  (`--json`) names the account and refreshes a token that is due; `devagotchi logout`
  revokes both tokens and forgets the account; `devagotchi login --local` records the
  accountless choice. `devagotchi start` says which it is until one is on file. The hub
  accepts the token `login` lands when the CLI's client id is in `auth.clients`.

### Sign in and out from anywhere (increment 22)
- Every hub page has a top bar: signed in, Approvals, Machines and a one-tap Sign out;
  signed out, one Sign in that returns to the same page. `/` explains Devagotchi and offers
  Sign in with Shippin instead of bouncing a stranger to the identity provider.

### Org boards (increment 21a, B13)
- A project's board belongs to the org the identity provider proves the account belongs to
  (Nomen's `urn:zitadel:iam:user:resourceowner:id`): `board_bind` by any member makes or
  finds it, every member and their agents work it, another org never sees it. Seats are
  `<account>/<seat>` on an org's board; the free limit counts the org's projects; `whoami`
  says the org. Accounts with no org keep boards of their own.
- Web sign-in, `devagotchi login` and the RFC 9728 metadata (`scopes_supported`, and `scope`
  in the 401 challenge) ask for exactly `openid profile offline_access
  urn:zitadel:iam:user:resourceowner` and the hub's audience.

### The command line bound to the hub (increment 15b, B11)
- `devagotchi board bind [--hub URL] [--project NAME]`: the hub makes (or finds) the account's
  board and `.devagotchi/config.json` remembers the hub; every `board` command and hook here
  then runs on the hub as the account on file, the agent's name as its seat; `board unbind`
  goes back to the local store. The hub's refusals keep their codes; `not_signed_in`,
  `signed_out` and `hub_unreachable` say what to run. `devagotchi mcp --hub` sends the seat
  (`DEVAGOTCHI_ACTOR`). `board_list` takes `@me`.
- `devagotchi hub board …` is gone; `devagotchi board …` is the one command line.

### Account boards on the hub (increment 15a, D23)
- `account_boards` in the hub config: `board_bind {project}` makes the calling account's own
  board (one per account-project under `dir/<account>/`), listed first in `whoami.boards`; a
  free account binds one project (`plan_limit`; the owner and `unlimited` accounts have no
  limit); a shared board's name cannot be bound (`name_taken`); a hub without the section
  answers `not_offered`.
- Seats: `X-Devagotchi-Seat: <name>` makes one account's agents distinct actors on its own
  board, and `<account>/<seat>` on a shared one; never a person; fences kept per seat.

### The account in the agent's session (increment 14c)
- `devagotchi mcp --hub [URL]`: the stdio MCP server an agent already runs, relaying the
  session to the hosted hub's `/mcp` (default https://devagotchi.com) as the account on
  file — the token stays on this machine and rotates underneath the agent; a refused,
  expired or missing account is answered on the request's id with what to run.

### Dev tokens are for tests (increment 14b, D21)
- The hub accepts `auth.mode dev` only when `public_url` is loopback http; at an address
  people reach it refuses to start and names the alternatives (`nomen`, `oidc`).
- `install-devagotchi.sh --role hub --auth-config FILE`: the `auth` object (and `owner`) from
  FILE, the identity provider's key installed as the hub account's own 0600 file, no tokens
  minted; `--person`/`--agent` are account IDs. A reachable `--public-url` without it is
  refused before anything is written.

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
