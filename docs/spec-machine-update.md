# Spec: machines update themselves, and on request

Status: APPROVED by Jesse 2026-11-05. One agent executes U1–U4 in order.
Ruled 2026-11-05 (Jesse): **both** — machines update themselves quietly by default, and an
AI can update one now with a tool. Scope is only what is written here. Paths are relative to
the repo root; function names are the anchors, line numbers are hints from `main` 2026-11-05.

## Why (what happened on dabo, 2026-11-05)
Updating a joined Windows PC took a hand-written script started outside the broker (a job
the broker owns cannot outlive the broker it stops), and the first try failed and rolled back
(fixed in #140). A machine has no idea what build it runs (`devagotchi --version` prints only
`0.1.0`) and no way to learn there is a newer one. Every user would hit this.

## The behaviour a person gets
1. The hub serves a new build (its own commit, as today, `crates/hub/src/downloads.rs`).
2. Within an hour, each online machine that is on another build and has **no running jobs**
   updates itself: download from the hub, sha256 checked against the hub's list, installer
   in upgrade mode, back online on the new build. If the new build does not come up, the
   previous one comes back by itself. Nothing for the person to do or see unless it fails.
3. `machines` shows each machine's `build` and, while one is running, `update: "updating"`;
   after a failure `update: "failed: <reason>, back on <build>"`.
4. An AI can call `machine_update` with a machine name: same steps, now, and it waits for
   running jobs unless `force: true`. The tool answers when the machine is back (or failed).
5. Off switch: hub config `machines.auto_update` (default `true`); `machine_update` works
   either way.

## Work items (each one PR, merged only on full green, in this order)

### U1 — every build knows its name (contracts + binary, no behaviour change)
- `crates/devagotchi/build.rs` (new): `DEVAGOTCHI_BUILD` from the env at compile time
  (CI sets it to `main-<sha7>` / the release version, the same string the hub's downloads
  `VERSION` file holds), else `dev`. `devagotchi --version` prints `devagotchi 0.1.0 (<build>)`.
- Set `DEVAGOTCHI_BUILD` in every workflow that builds release/download artifacts
  (`.github/workflows/release.yml`, `image.yml`, wherever `VERSION` is written today — the
  agent finds the one writer and uses the same value).
- Test: `crates/devagotchi/tests/one_binary.rs` asserts the `--version` shape.

### U2 — the link says its build; the hub can ask it to update (contracts)
`crates/contracts/src/lib.rs` (only place contracts change; regenerate with
`node scripts/generate-contracts.cjs`):
- `LinkFrame::Hello` gains `#[serde(default)] build: Option<String>`. Safe: hubs update first
  (they hand out the builds), and an old device simply omits it.
- New `HubFrame::Update { version: String, force: bool }`. The hub sends it **only** to a
  device whose hello carried `build` (an old device would refuse an unknown variant).
- New `LinkFrame::UpdateStatus { state: String, detail: Option<String> }` with states
  `waiting_for_jobs | downloading | installing | done | failed`.
- Tests: round-trip serde for the three; an old-shape hello (no `build`) still parses.

### U3 — the link updates its machine (crates/link)
- `crates/link/src/session.rs`: send `build` in the hello; on `HubFrame::Update`, start one
  update (a second one while running is answered with the current status).
- `crates/link/src/update.rs` (new, ≤ 250 lines):
  1. **Wait for idle**: ask the local broker `OperationList` (the same broker connection the
     link already uses for calls) until no operation is `accepted`/`running`; re-check every
     30 s; give up after 6 h with `failed: busy` unless `force`.
  2. **Download** `/dist/install-devagotchi.{ps1,sh}` and the archive for this target from the
     hub URL in the link config; sha256 must match the hub's `SHA256SUMS` (fetched from
     `/dist/SHA256SUMS`; add that route to `downloads.rs` if it is not served). Mismatch →
     `failed`, nothing changed.
  3. **Hand off** to the platform updater so the update outlives the link and the broker:
     - Windows: start `powershell.exe -File <installer> -Version <v> -ArtifactDir <dir>
       -Root <root> -Autostart` as a process **not** in any job of ours (the link is not in the
       broker's job; spawn it detached with `CREATE_BREAKAWAY_FROM_JOB` attempted and
       `DETACHED_PROCESS`), stdout/stderr to `<root>\logs\update-<v>.log` files (never a pipe
       back to the link — a pipe kept the 2026-11-05 updater alive forever).
     - Linux: services run as an unprivileged account and `/opt/devagotchi` is root's, so the
       link writes `<state>/update-request.json` (version + dir) and a root
       `devagotchi-update.path` unit (installed by `deploy/linux/install-devagotchi.sh`)
       starts `devagotchi-update.service`, which runs the installer in upgrade mode from the
       verified dir. The link never gets root.
  4. **Rollback** is the installer's: it already keeps `versions/<v>` and `current`; the
     updater script (Windows) / the service (Linux) re-runs the installer with the previous
     version when the new broker or link does not report ready within 60 s.
  5. The new link reconnects and its hello carries the new build; the hub compares and marks
     the update `done`, or `failed` with the log's last line if the build did not change.
- Only code in `crates/link` and the platform files; `unsafe` only where CLAUDE.md allows
  (the Windows spawn flags belong in a small platform module the agent places per the rules).

### U4 — the hub: auto-update and `machine_update` (crates/hub)
- `crates/hub/src/link.rs`: keep each live session's `build` from its hello.
- Auto-update: once an hour, and when a device connects, for each live device with a `build`
  different from `downloads.version`, not updated in the last hour, and `machines.auto_update`
  on, send `Update { version, force: false }`. Limit to 20 machines at once per hub.
- New MCP tool `machine_update { machine, force?: bool, wait_seconds? }` in the machines tool
  router (`crates/hub/src/hub.rs` next to `machine_add`): sends `Update`, then waits up to
  `wait_seconds` (default 50) for `done`/`failed`; returns state and build. Description:
  "Update one of the user's machines to the hub's current build now. Waits for its running
  jobs to finish unless force is true; the machine is offline for about a minute."
- `machines` output gains `build` and `update` (state + detail) per machine.
- Config: `machines.auto_update: bool` (default true) in `crates/hub/src/config/`.
- Audit: each update request and its result go in the audit chain like other machine events.

### U5 — tests (in the PRs above, listed here so nothing is skipped)
- Unit: version compare and the hourly selection (U4); idle wait with a fake broker (U3).
- **Real OS, Windows CI** (`deploy/windows/ci-install-check.ps1`, same job as #140's test):
  install build A from local artifacts, start link + broker as the Run entries do, serve
  build B from a local hub (`tests/e2e.py` already starts one), send `Update`, assert the
  machine comes back reporting build B; then serve a broken B (broker exits at start) and
  assert it comes back on A with `failed`.
- **Real OS, Linux CI** (`deploy/linux/ci-install-check.sh`): the same two cases through the
  `.path` unit.
- e2e (`tests/e2e.py`): `machine_update` on a Linux broker from build A to B.

## Acceptance
1. CI green with the tests above on Linux and all three Windows runners.
2. Live: Jesse's PC `dabo` updates itself after the next merge with no one touching it;
   `machines` shows the new build. Then `machine_update dabo` with no new build answers
   "already on <build>".

## Not in this spec
Mac; update channels (beta/stable); updating the hub itself (it already follows its image);
signed builds beyond the hub's sha256 list.
