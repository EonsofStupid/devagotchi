# Devagotchi

**Your AI, at home in your workspace.**

Devagotchi is a universal workspace bridge, beginning with locally installed MCP clients and a native app for seeing commands, output, and workspace activity. The product direction represents every admitted task as a graph and grows into a trusted internal mesh, with public API/MCP surfaces for adoption. Windows, macOS, and Linux are required targets. Source is published at [EonsofStupid/devagotchi](https://github.com/EonsofStupid/devagotchi). The rebuilt [Devagotchi Site](https://devagotchi.justsayit.chatgpt.site) is published with a read-only HTTP/MCP reference service and Linux source download; [devagotchi.net](https://devagotchi.net) has been added and is pending DNS validation.

This working copy starts the replacement for DesktopCommanderMCP with a separate process broker, a thin official-SDK MCP adapter, and a Tauri desktop inspector. **Execution providers are implemented for Linux and Windows. The macOS provider is still required before a cross-platform release.**

## Install: one line, for you or your AI

Tell your AI "install Devagotchi from github.com/EonsofStupid/devagotchi", or paste the line
for your system into any terminal. It installs the `devagotchi` binary for your user (no
administrator rights), then `setup` gives every AI client it finds (Claude Code, Codex, Claude
Desktop) an MCP server named `devagotchi`. No account, nothing to start by hand: your broker
starts in the background on an AI's first call, and again on the first call after a reboot.

> **Not published yet.** These lines work once the first release (v0.1.0) is out; until then
> the links return 404. Build from source instead ([Linux](#build-the-linux-core),
> [Windows](#build-and-run-on-windows)).

Linux (x86_64, ARM64; glibc 2.35 or later, e.g. Ubuntu 22.04, Debian 12):

```sh
bash -o pipefail -c 'curl --proto =https --tlsv1.2 -LsSf https://github.com/EonsofStupid/devagotchi/releases/latest/download/devagotchi-installer.sh | sh' && ~/.local/bin/devagotchi setup
```

Windows (x64, ARM64), from PowerShell, Command Prompt or Git Bash alike:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -c "[Environment]::SetEnvironmentVariable('PSModulePath', [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')); try { irm https://github.com/EonsofStupid/devagotchi/releases/latest/download/devagotchi-installer.ps1 -ErrorAction Stop | iex } catch { throw }; & ((Get-Variable HOME -ValueOnly) + '\.local\bin\devagotchi.exe') setup"
```

Either line stops before `setup` if the download or the install fails. Restart any AI client
that was open. macOS is not supported yet (its execution provider is pending). On ARM64
Windows, the line run from Git Bash (an x64 program there) installs the x64 build, which runs
under emulation; run it from PowerShell or Command Prompt for the ARM64 one.

**Another MCP client** (Cursor, VS Code, Windsurf…): `setup` prints the server's full path
first (`Devagotchi MCP server: /home/you/.local/bin/devagotchi mcp`); give your client a stdio
server with that absolute path as its command and `mcp` as its argument. A client does not
expand `~` or `%USERPROFILE%`. `setup` ends with an error when it finds none of the three
clients above, after printing that line.

**What it changes**, which an AI doing this for someone should tell them before running it:
- the binary, in `.local/bin` in your home folder (`setup` prints its full path);
- your `PATH`, when `.local/bin` was not on it: on Linux the installer writes
  `~/.config/devagotchi/env.sh` (and `env.fish`) and adds the line
  `. "$HOME/.config/devagotchi/env.sh"` to `~/.profile`, your bash and zsh startup files, and
  `~/.config/fish/conf.d/devagotchi.env.fish`; on Windows it adds that folder to your user
  `Path`;
- the `devagotchi` entry in each AI client's config: Codex's and Claude Desktop's files are
  backed up beside themselves first (`.bak-<time>`), Claude Code's is set with `claude mcp`;
- your broker, a background process that keeps running, with its config and state in
  `~/.config/devagotchi` and `~/.local/state/devagotchi` (Windows `%APPDATA%\devagotchi` and
  `%LOCALAPPDATA%\devagotchi`). It runs what your account can, as Desktop Commander does:
  shell commands, any program, and files anywhere you can reach, starting in your home
  directory. So an AI client with Devagotchi can do whatever your account can, including root
  or administrator rights when you install it as root or from an elevated (Run as
  administrator) window. To narrow it to the developer tools it found on your `PATH`, inside
  your home directory, set `"exec_policy": "granted"` in `broker.json`.

`setup` ends with `Check: Devagotchi answered with N tools` when it works;
`devagotchi setup --help` lists the choices (one client, `--local` or `--hub`).

**To update:** close your AI clients (an open one starts the broker again), stop the broker,
then run the line again. Windows cannot replace the program while anything still runs it.

**To undo**, in this order:
1. Close your AI clients, then stop the broker: `pkill -f '.local/bin/devagotchi start'`
   (Windows: `Get-Process devagotchi | Stop-Process`).
2. Remove the `devagotchi` entries: `claude mcp remove --scope user devagotchi`, the
   `[mcp_servers.devagotchi]` table in `~/.codex/config.toml`, `mcpServers.devagotchi` in
   Claude Desktop's config.
3. Linux: remove the installer's line from your startup files, then its fish file:
   `sed -i '\#devagotchi/env.sh#d' ~/.profile ~/.bashrc ~/.bash_profile ~/.bash_login ~/.zshrc ~/.zshenv 2>/dev/null; rm -f ~/.config/fish/conf.d/devagotchi.env.fish`.
   Windows: remove `.local\bin` in your home folder from your user `Path` (System Properties >
   Environment Variables), unless other tools you use live there too.
4. Delete the binary and the two directories above.

**Checking what you run:** the Linux installer checks the archive's SHA-256 before installing;
the Windows installer does not. Every release's archives carry GitHub build attestations:
download one from the release page and run
`gh attestation verify <archive> --repo EonsofStupid/devagotchi`.

## Project status and source preview

| Surface | Current status | What its evidence establishes |
|---|---|---|
| Desktop broker, local stdio MCP, and inspector | Linux and Windows broker source; historical Linux verification; lifecycle, journal, and IPC tests on Linux and Windows in [CI](.github/workflows/ci.yml) | The recorded process/protocol/native UI scenarios and the CI test results; no macOS, Windows installer, or named-host certification. |
| Public website and reference HTTP API/MCP | Published as Site version 3; HTTP reference/download verified, MCP enabled with OAuth and installed plugin | Read-only docs, local schemas, capability information, and a labelled graph example; no workstation execution. |
| Custom domain | devagotchi.net added, DNS pending | No verified custom-domain activation is claimed here. |
| Task graphs | Candidate contract and scoped implementation plan | No graph execution engine is implemented in this foundation. |
| Trusted mesh and runtime HTTP API/MCP | Committed direction; proposed contracts and acceptance gates | No public or peer-to-peer workstation runtime endpoint is implemented. |

This directory is the desktop foundation and planning source preview, not a signed product installer. Website/reference-service verification belongs to the existing Site's separate publication receipt and does not extend the original Linux evidence. A public tool schema is documentation; it does not grant access to the documented operation.

**Planning revision, October 17, 2026 UTC:** the [architecture and growth plan](docs/product-and-platform-plan.md) retains the existing owners, G0–G5 native/scale gates, and immediate P0 reference publication. The [task-graph and mesh contract](docs/task-graph-and-mesh-contract.md) defines proposed task/step/attempt/output/event records, local-to-mesh packets, and failure/authorization semantics. The [performance specification](docs/performance-and-experience-spec.md) defines proposed budgets and reproducible comparisons; none is an achieved benchmark. The [execution guide](docs/implementation-guide.md) separates public publication from core baseline work. [Desktop Commander user evidence](docs/desktop-commander-user-evidence.md) preserves primary reports, corrections, and fix evidence without claiming every reported problem remains present.

## Current desktop behavior

- The independently running broker owns admitted jobs. Closing the inspector or MCP connection does not terminate them.
- Every job has a stable operation ID, exact executable and argument vector, working directory, deadline, and durable state.
- Matching retries return the recorded job. Reusing a key with changed inputs is a conflict.
- Each output reader supplies its own cursor. UTF-8 decoding is incremental; raw bytes remain available losslessly. Retention limits produce explicit gaps.
- Cancellation and deadlines supervise the root and its inherited Linux process group. A parent exiting does not abandon children that remain in that group.
- On Windows, the root is created suspended and assigned to a broker-owned Job Object before it runs. Cancellation and deadlines terminate the job and verify that no active process remains in it.
- Restarted brokers preserve unfinished work as `unknown`. They do not replay it or signal a remembered PID.
- The desktop inspector reads the same operations, displays exact command details and live retained output, and can request cancellation.
- The native host validates broker responses against Rust types. Desktop types are generated from the same canonical Rust contracts.

This desktop implementation does not yet provide file/search/edit tools, PTY input, workspace onboarding, a tray/service installer, Windows ConPTY, macOS process supervision, signed updates, task-graph execution, trusted mesh, or runtime HTTP. Those capabilities follow the scoped sequence in the [master plan](docs/product-and-platform-plan.md). The website/reference publication is a separate immediate workstream and does not wait for full native release.

## Components

| Component | Source | Responsibility |
|---|---|---|
| Broker | `crates/core/src/broker.rs`, `crates/core/src/journal.rs`, `crates/broker/src/main.rs` | Admission, operation lifecycle, output retention, cancellation, recovery |
| Execution | `crates/core/src/execution.rs`, `crates/core/src/execution/platform/linux.rs`, `crates/core/src/execution/platform/windows.rs` | Process orchestration; spawning, observing, and terminating one root's owned process group or Job Object |
| Local IPC | `crates/ipc/src/lib.rs`, `crates/ipc/src/unix.rs`, `crates/ipc/src/windows.rs`, `crates/ipc/src/windows/security.rs` | Private state directory, Unix socket or named pipe, peer UID or SID, instance lock, bounded framed requests |
| MCP adapter | `crates/mcp/src/main.rs` | Official `rmcp` 3.5.1 stdio transport and four typed tools |
| Desktop app | `apps/desktop` | Tauri 2 native host and React/TypeScript inspector |
| Website and public reference service | [`apps/website`](apps/website/README.md) | Product documentation, source download, and read-only HTTP/MCP reference routes |
| Contract authority | `crates/contracts/src/lib.rs` | Rust DTOs, strict decoding, generated JSON Schema and TypeScript |
| Verification | `tests`, module tests, `verification` | Real process and protocol checks plus recorded evidence |

The desktop process and MCP adapter both connect to the broker. Neither owns a job's lifetime or silently restarts a job after a lost connection.

As the product grows, graph scheduling extends this broker and journal. A step attempt references its real operation; transport redelivery does not create another attempt. Mesh targets keep their local grants and settlement authority. The website presents public reference material without acquiring either role.

## Build the Linux core

Install the Rust toolchain specified in `rust-toolchain.toml`. Then, from this repository root:

```bash
cargo build --locked --bins
cargo test --locked --lib
```

The account (D22): `devagotchi login` signs in with your Shippin account from any terminal — it shows a code and a URL, you confirm on any device, and the tokens land owner-only in `~/.config/devagotchi/account.json` (`whoami` names the account, `logout` revokes). `devagotchi login --local` records the other choice: no account, everything on this machine. `devagotchi start` says which it is until one is on file. An agent reaches the hosted hub as that account with `devagotchi mcp --hub` — the same stdio command, relayed to the hub, the token never in the agent's config. `devagotchi connect` sets that up in Claude Code, Codex and Claude Desktop in one go (each config backed up and written whole), then starts the server once and lists its tools; `--local` points them at this machine's broker instead. A project's board moves to the hub with `devagotchi board bind`: from then on `devagotchi board …` and the hook pack run there as your account, your agents as seats, from every machine you sign in on (`board unbind` goes back).

The quick way: `devagotchi start` runs your broker with no paths to remember — the first run writes `broker.json` (it runs what your account can, `exec_policy` `account`; workspace: your home directory, or `--workspace`; grants, used only if you set `exec_policy` to `granted`: the toolchains found on PATH, plus `--grant`) under `~/.config/devagotchi`, keeps its state under `~/.local/state/devagotchi`, and tells `devagotchi mcp` where to find it. The explicit way, with your own config (`config/broker.example.json` as a template; absolute, existing paths; only executables you intend to authorize; the state directory's parent must already exist — the broker creates the state directory with mode `0700` or verifies an existing private directory):

```bash
./target/debug/devagotchi broker run \
  --state-dir /absolute/private/state/devagotchi \
  --config /absolute/path/broker.json
```

The endpoint is `/absolute/private/state/devagotchi/broker.sock`. Run the broker independently of the MCP client. `SIGTERM` or Ctrl-C requests bounded shutdown and cancellation of active owned jobs.

The example config's paths are placeholders, deliberately not broad default grants. The broker loads one workspace and one operator configuration at startup. Configuration errors prevent admission. MCP tools cannot modify grants.

## Build and run on Windows

Install the toolchain from `rust-toolchain.toml` for the MSVC host and the Visual Studio C++ build tools, which compile the bundled SQLite. From the repository root in PowerShell:

```powershell
cargo build --locked --bins
cargo test --locked --lib
```

Use `config/broker.windows.example.json` as the template. Grants must name `.exe` or `.com` images. Batch files are interpreted by `cmd.exe`, so grant `cmd.exe` itself when you intend to run scripts. The state directory's parent must exist. The broker creates the state directory owned by your user, with a protected DACL that grants only your user, and refuses an existing directory that inherits other permissions.

```powershell
.\target\debug\devagotchi.exe broker run --state-dir X:\absolute\private\state\devagotchi --config X:\absolute\path\broker.json
```

The ready line names the endpoint: a local named pipe, `\\.\pipe\devagotchi-broker-<digest>`, derived from your user SID and the state directory. Pass it to `devagotchi mcp --socket` and to the desktop app (`devagotchi start` leaves it in `<state>\endpoint` so `devagotchi mcp` needs no flag). The pipe's DACL grants only your user and it rejects remote clients. The broker refuses to start if a pipe with its name already exists, and each end checks that the other runs as your user. Ctrl+C, Ctrl+Break, closing the console, or system shutdown request bounded shutdown and cancellation of active owned jobs.

Each root is created suspended, assigned to a new Job Object, and only then resumed, so neither it nor anything it starts runs outside that job. The job permits no breakaway and terminates its members when the broker's job handle closes, including when the broker exits. Cancellation and deadlines call `TerminateJobObject` and verify that no active process remains. Windows has no general graceful-stop request for arbitrary programs, so there is no grace period. Arguments are encoded with the Microsoft C runtime quoting rules most programs parse; a program with its own command-line parser may read them differently.

## Connect a local MCP client

One step, no account: `devagotchi setup` gives Claude Code, Codex and Claude Desktop (every one
it finds) an MCP server named `devagotchi`, then starts it once to check it answers. The
server is `devagotchi mcp`, which starts your broker in the background when none is running
(log in `<state>/broker.log`) and finds it after that, so nothing has to be started first and
the broker keeps running when the client closes. Signed in with `devagotchi login`, `setup`
points the clients at your account's hub instead; `--local` or `--hub` choose explicitly.

For clients using an `mcpServers` configuration object, the entry has this shape:

```json
{
  "mcpServers": {
    "devagotchi": {
      "command": "/absolute/path/to/devagotchi/target/debug/devagotchi",
      "args": ["mcp", "--socket", "/absolute/private/state/devagotchi/broker.sock"]
    }
  }
}
```

`"args": ["mcp"]` is enough: it finds your broker, or starts it. On Windows, `command` is the path to `devagotchi.exe` and the `--socket` value is the broker's `\\.\pipe\` endpoint, escaped as `"\\\\.\\pipe\\devagotchi-broker-<digest>"` in JSON.

The exact configuration location and format depend on the selected host. A named desktop client's integration has not yet been certified. The adapter's raw stdio tests cover both `initialize` with protocol `2025-11-25` and discovery/per-request metadata with `2026-07-28`.

Those are historical local-adapter results. The separate Site reference MCP targets an explicitly modern `2026-07-28` profile and requires its own published-route verification; it does not expose the following runtime tools for execution.

| Tool | Effect |
|---|---|
| `run` | Run a command line in one call: start it, wait up to 50 s, return its output and exit code; one still running keeps running and is followed with the operation tools |
| `read_file` | Read a file (text as text, anything else as base64), 24 KiB a call: by lines (`start_line`, negative for the last lines, `line_count`; it gives `next_line` and `total_lines`) or by bytes (`offset`, negative for a log's end) |
| `list_directory` | List a directory, up to three levels deep |
| `write_file` | Overwrite (atomically), create or append; returns the SHA-256 to guard the next write |
| `edit_file` | Replace exact text an exact number of times, or change nothing |
| `read_image` | Look at a PNG, JPEG, GIF or WebP up to 3.75 MB and 8000 px a side: it comes back as an image the AI can see (type and size from the file's own header) |
| `read_multiple_files` | Read up to 20 files in one call; a file that cannot be read is reported beside the rest |
| `get_file_info` | Kind, size, times, read-only, Unix mode, a link's target, a text file's line count |
| `create_directory` | Make a directory and its missing parents; an existing one is not an error |
| `move_file` | Move or rename on the same filesystem; never replaces without `overwrite`, never a directory |
| `operation_start` | Admit a command line, or an executable with exact arguments, with a retry key |
| `operation_list` | List the broker's operations, newest first, before and after restarts |
| `operation_wait` | Wait until an operation finishes or prints, without polling |
| `operation_get` | Read the job's observed state and exact request |
| `operation_read` | Read retained output after a caller-owned cursor |
| `operation_cancel` | Explicitly request cancellation of an admitted job |

The hub serves the same tools for every linked machine, each with the machine's name, except `operation_list`: a broker the hub shares between people would list everyone's jobs, so it waits until the list is filtered to the caller's own.

Stopping an MCP request only stops waiting for that response. Once admission is committed, reuse the same retry key and request to reconcile a lost reply. A different retry key can start new work.

## Desktop app

The desktop source is in `apps/desktop`. Install the native dependencies in the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/), then:

```bash
cd apps/desktop
npm ci
npm run build
DEVAGOTCHI_SOCKET=/absolute/private/state/devagotchi/broker.sock npm run tauri dev
```

The UI does not create grants or start a hidden broker. Its unconfigured, offline, empty, failed, and connected states are explicit. See the desktop README for the exact native build and QA commands. The separate browser fixture is for UI tests only and is not included in the production app.

## Generate contracts

After installing the desktop development dependencies:

```bash
node scripts/generate-contracts.cjs
node scripts/generate-contracts.cjs --check
```

The Rust exporter produces one JSON Schema bundle. `json-schema-to-typescript` generates the UI DTOs. The check compares fresh Rust output and generated TypeScript without changing either checked-in artifact. Rust remains responsible for domain bounds and executable grants; JSON Schema generation is not a substitute for that validation or an implemented task-graph engine.

## Run the acceptance checks

These tests create disposable workspaces and launch only their own fixture processes. The Python suites are Linux harnesses; on Windows, `cargo test --locked --lib` runs the shared lifecycle tests plus the Windows IPC and journal tests. Linux local Unix sockets must be available; the boundary suite performs a preflight so an environment that blocks sockets cannot produce misleading passing startup-denial tests.

```bash
python3 -B tests/mcp_smoke.py --output test-results/mcp-smoke.json
python3 -B tests/security_boundary.py --devagotchi target/debug/devagotchi
python3 -B tests/mcp_protocol.py \
  --devagotchi "$PWD/target/debug/devagotchi" \
  --socket /tmp/devagotchi-protocol-unused.sock \
  --output test-results/mcp-protocol.json
```

The smoke test covers real process execution, split Unicode, independent readers, retries/conflicts, reconnect, cancellation, deadlines, output overflow, and restart without re-execution. The boundary tests cover malformed frames, grants, distinct UIDs, socket/state collisions, journal hardlinks, connection saturation, single broker ownership, and children that outlive their root.

## Authority and limits

The local trust boundary is the OS user. A same-user process can use the same private endpoint; this version does not authenticate individual MCP applications separately. Neither the Unix socket nor the named pipe accepts remote clients.

Future authenticated runtime HTTP and trusted peer routing must reuse broker authorization and recorded operation identity. A website login, graph edge, route advertisement, or client-supplied device label cannot widen a workstation grant. An unreachable target or uncertain attempt is not permission to execute the work on another machine.

Executable grants authorize the behavior of that executable as your OS user. Granting Python, a shell, or a similar interpreter grants its ability to run code. A checked working directory is not filesystem or network containment. Stronger restrictions require an actual OS isolation provider.

Output and command arguments can contain sensitive or hostile text. The broker and inspector do not send telemetry, and the journal is stored locally. Connected MCP clients receive tool results and may forward them to their configured model or services. The UI renders output as text. The journal contains exact arguments and retained output, so choose a private local state directory and appropriate retention. Logical output/frame and operation counts are bounded; a strict physical disk quota and a complete denial/audit event chain are later release requirements.

The current engine uses pipes, not a PTY. It clears inherited environment variables and closes stdin. On Windows it keeps only `SystemRoot`, which Windows system libraries require. Programs that need environment configuration, interactive input, or a real terminal are outside this initial execution contract and must receive explicit future support.

## Source and release status

DesktopCommanderMCP was inspected as the reference at commit `f508e524469c26f6366e17dbfd3ee5a69d112127`. The implementation here is a new foundation; it is not a claim of feature parity or benchmarked superiority over that project. The separate audit explains concrete behaviors being improved and preserves the reference's MIT license with its source snapshot.

The broker, desktop inspector, planning documents, audit evidence, and website source are published in this repository. The initial complete source import is commit [`72ea5db`](https://github.com/EonsofStupid/devagotchi/commit/72ea5db73e898efa0ebfc8800a531a2df29fe6f7), whose Git tree matches the verified 211-file source snapshot. Website code lives in [`apps/website`](apps/website/README.md) and retains its existing Sites deployment identity. The earlier `404` is preserved in historical audit artifacts; repository access is now verified.

The website is published at https://devagotchi.justsayit.chatgpt.site as version 3, source commit `91bb93c49eb35fa37315973666ce71dbb437a659`. The [publication receipt](verification/website-publication.json) records 12 passing reference-handler checks, successful type checking/build, 13 successful public GET checks, and the deployed MCP OAuth challenge. The MCP plugin is provisioned and installed; its tools did not surface in this chat, so an authenticated named-client call is not claimed. Browser visual QA remained unavailable. `devagotchi.net` is pending DNS validation. Public reference availability does not imply graph/mesh/runtime HTTP implementation, a native installer, or measured superiority.
