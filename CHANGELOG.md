# Changelog

Releases are the operator's act: a tag `vX.Y.Z` on `main` runs `.github/workflows/release.yml`
(cargo-dist: archives for x86_64/aarch64 Linux and Windows, installers, GitHub attestations).
The version in `Cargo.toml` is bumped by the operator in the same commit as this file's next
heading. Agents draft the fragments below; they never tag.

## Unreleased (would be 0.1.0 — the first release; no tag exists yet)

### One line joins a machine (increment 29b)
- With `downloads` in the hub config (a directory of `VERSION`, `SHA256SUMS` and the build the
  hub runs), "Add a machine" shows one line per OS: `curl -fsSL <hub>/join/<code> | sudo sh` on
  Linux and `irm <hub>/join/<code>.ps1 | iex` on Windows. The script `/join/<code>` serves
  downloads that build from the hub's `/dist`, checks every file against the SHA-256 written
  into it, installs, makes the machine's key, enrolls (the code on stdin or in the
  environment, never an argument), sets `exec_policy: account` and starts the broker and the
  link at boot. Fetching it does not spend the code; enrolling does; a dead code's script only
  says so. The hub hashes the listed files at start and leaves `/join` off, reported in
  `/healthz`, when one does not match.
- `scripts/lay-down-dist.sh COMMIT DIR` lays down that directory from GitHub's build of a main
  commit: the four archives through `fetch-main-build.sh` (checksum and attestation), the
  commit's installers, `VERSION` and `SHA256SUMS`.
- `install-devagotchi.sh --exec-policy granted|account` and `install-devagotchi.ps1
  -ExecPolicy`; the Windows installer also takes the code from `DEVAGOTCHI_ENROLL_CODE`.
- CI: the Linux host-install job joins a bare account with the one line from a hub serving the
  runner's own build and runs a real operation through it.

### Add a machine from the browser (increment 29a)
- The Machines page (`/account`) has **Add a machine**: a signed-in person names a machine
  and gets a one-time code for it, shown once, valid 15 minutes, with the installer line for
  Linux and Windows. The machine that enrolls with it belongs to that account: only it sees
  and reaches the machine (`machines`, `whoami`, every routed tool) without anyone editing
  the hub's config, and only it can **Revoke** it. Names the hub config uses, names another
  account holds (or holds a live code for), and more than 16 machines per account are
  refused. The owner's phones are told when the machine joins, with its key's fingerprint.
- The link admits a machine the config does not name only with an account's code or as an
  account's enrolled machine; an operator's code still joins only a configured broker.

### `devagotchi connect`: Claude Code, Codex and Claude Desktop in one command (increment 28a)
- Each client found on the machine (or named) gets an MCP server `devagotchi` running this
  binary: `mcp --hub` (the hosted hub as the signed-in account; the default once
  `devagotchi login` ran) or `mcp` with `--local`. Claude Code through its own
  `claude mcp add --scope user`; Codex as `[mcp_servers.devagotchi]` in config.toml
  (`CODEX_HOME` honoured, `tool_timeout_sec = 120` because a wait is held up to 50 s);
  Claude Desktop as `mcpServers.devagotchi`. Every file is backed up and written whole;
  running it again replaces Devagotchi's entry and nothing else.
- Then the server is started once and asked for its tools: the answer is whether it works.
- The hosted hub's default URL is `https://app.devagotchi.com` (was devagotchi.com, which
  now redirects there; the relay never follows redirects, so `mcp --hub` and
  `board bind --hub` without a URL failed).

### The hub keeps working while Nomen is down (increment 27a)
- 26 Oct 14:45 UTC: Gold's edge stopped answering, so Nomen did too, and every call to the
  hub on Texas waited 10 s on it and then failed; Claude's connector timed out. Now a token
  Nomen confirmed keeps working through such an outage, until the token's own expiry and
  never more than 12 hours after Nomen last confirmed it. A token Nomen never confirmed
  gets 503 with Retry-After at once instead of after a 10 s wait.
- After one failure the hub stops making every caller wait on Nomen: one request at a time
  tries again, after 2 s doubling to 30 s. Nomen gets 3 s to connect and 5 s to answer
  (was 5 and 10).
- The outage is logged once when it starts and once when it ends, with how long it lasted
  and how many requests were honoured meanwhile; per request, at most one line every
  30 s. `/healthz` gains `issuer`: reachable, down since, last answer, last error.

### Board waits wake on the change itself (increment 26b)
- Every board write that records an event advances the board's change signal after it
  commits. `board_wait` and `approval_wait` sleep on it instead of re-reading the store
  every 500 ms, so a note, a claim or a person's decision on the phone reaches the waiting
  agent the moment it lands. A write that records nothing (an empty reap) wakes nobody.
- Lapsed leases are expired when they lapse: the reaper sleeps until the first lease on
  any open board ends, or until a board changes, instead of sweeping every 60 s.

### The inspector waits instead of polling (increment 26e)
- New broker request `operations_changed {after_revision?, timeout_ms}`: answered when an
  operation is admitted or retired or changes state (output alone does not count), or at
  the timeout with the same revision; a revision the broker does not have (it restarted)
  answers at once.
- The desktop inspector re-lists only when that answer changes, instead of every 3 s,
  and follows the selected command's output with `operation_wait` (output past its
  cursor, or settlement) instead of reading every second; draining pages no longer
  pauses 100 ms between them. After an error it retries in 3 s.
- The desktop crate builds against the current contracts again: its response-kind match
  lacked the file responses added in 24d and 24e.

### Process exits are events on Windows (increment 26d)
- The root's exit is the wait on its process handle (registered with the system thread
  pool by the runtime), not a 20 ms look. Each Job Object gets its own I/O completion
  port, associated before the root is assigned; a listener thread advances a signal on
  every member-exit and active-process-zero notice, and the broker queries the job only
  then. Windows documents job notices as not guaranteed to arrive, so while members
  outlive the root the job is also queried once a second (never while the root runs).
- Termination waits for the job's notice instead of looking every 20 ms.

### Process exits are events on Linux (increment 26c)
- The broker no longer wakes every 20 ms while a command runs. On Linux it holds a pidfd
  for the unreaped root (readable when it exits, Linux 5.3+) and sleeps until the root
  ends, a stop is requested or the deadline passes. Members that outlive the root are
  waited for the same way: /proc is read once, every member found gets a pidfd (checked
  to still be a member after it is opened; 64 at a time), and /proc is read again only
  after they have all exited.
- Cancellation sends SIGTERM and moves to SIGKILL as soon as the group has left, instead
  of always waiting 200 ms; settlement after SIGKILL is the same event wait, bounded by
  one second as before.
- A kernel or sandbox without pidfd_open keeps the previous 20 ms look. Windows keeps it
  until increment 26d.

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

### Our servers run GitHub's builds (increment 25a)
- The release workflow also runs on every push to `main`: it builds, attests and keeps the
  archives for each target as workflow artifacts, without publishing a release (a version
  tag still does that). `scripts/fetch-main-build.sh COMMIT` downloads a commit's build,
  checks every archive's SHA-256 and verifies its GitHub attestation, and hands the
  directory to `install-devagotchi.sh --version main-<short> --artifact-dir`.

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
