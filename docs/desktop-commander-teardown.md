# Desktop Commander (wonderwhy-er/DesktopCommanderMCP) — evidence-backed teardown

## Provenance

| Item | Value |
|---|---|
| Source used | Fresh `git clone https://github.com/wonderwhy-er/DesktopCommanderMCP` into `/tmp/claude-0/-home-claude/92f61a44-293c-5903-8507-d0257b2e7590/scratchpad/dc-src`, fast-forwarded to `origin/main` on 2026-11-01. The vendored copy under `/home/claude/devagotchi/reference/` and `/home/claude/wonderwhy-er` were **not** used. |
| Commit (`git rev-parse HEAD`) | **`c2b87956217b58d170de58a0c951a3569b068ee3`** — 2026-11-01 23:56:53 +0000, "Merge pull request #982 from wonderwhy-er/codex/debug-server-url" (`git describe`: `v0.3.1-34-gc2b8795`) |
| Latest tag | **`v0.3.1`** → commit `7251da501d760e6eb5f95d802d9b42c741190950`, tag date **2026-10-22 07:49:43 +0000** |
| package.json version | `0.3.1` (`package.json:3`) |
| Side branch also inspected (NOT on main) | `origin/MQTT` @ `92e81b3f905004e2f616a02abf6a9d1405b3942b` (2026-11-01 23:51:18 +0000) — cited only where marked "MQTT branch" |

**Citation convention.** Every `path:Lx-Ly` is a path relative to the repo root **at commit `c2b8795…`** unless explicitly marked "MQTT branch @ `92e81b3`". Lines were read with numbered output from that exact checkout.

**Evidence labels.** Plain statements = read directly in the cited lines. **INFERRED** = not visible in source; the basis is stated. Nothing in the repo was executed (an attempt to run the command-blocklist parser in a harness was refused by the session's permission policy, so blocklist bypass notes below are code-reading inferences, not test results).

---

## 1. Local server

### 1.1 Runtime, language, entry points

| Fact | Evidence |
|---|---|
| Node.js **>= 22.12.0** required | `package.json:15-17`; `install.sh:22-24` enforces the same; MCPB manifest `manifest.template.json:163` (`"node": ">=22.12.0"`) |
| Conflicting older claims in docs/manifests: remote README says "Node.js 18 or newer"; `plugin.yaml` says `min_node: "18"` | `src/remote-device/README.md:9`; `plugin.yaml:55` |
| ESM TypeScript, compiled with `tsc`; build also copies JS installers, onboarding JSON, the offline-update script and bundles the UI runtime | `package.json:14`, `package.json:39` |
| npm bins: `desktop-commander` → `dist/index.js`, `setup` → `dist/setup-claude-server.js`, `remove` → `dist/uninstall-claude-server.js` | `package.json:18-22` |
| Subcommands on the main binary: `setup`, `remove`, `remote`; flag `--no-onboarding` | `src/index.ts:29-53` |
| `bootstrap.ts` must load first to raise the libuv threadpool before fs work | `src/index.ts:3-5` |
| Startup: create stdio transport → load config → init feature flags (network) → `capture('run_server_start')` → `server.connect(transport)`; after `initialized`: background Chrome check/download and preload of Excel/DOCX/PDF packages | `src/index.ts:55-146` |
| Uncaught exception → telemetry + exit(1); unhandled rejection → telemetry + keep serving | `src/index.ts:80-115` |

### 1.2 MCP SDK, protocol, transports

| Fact | Evidence |
|---|---|
| `@modelcontextprotocol/sdk` **^1.32.1**, locked **1.32.1** | `package.json:94`; `package-lock.json:1678-1679` |
| Low-level `Server` (not `McpServer`), name `desktop-commander`, capabilities `tools`, `resources`, `prompts`, `logging` | `src/server.ts:2`, `src/server.ts:103-116` |
| Custom `initialize` handler: records `clientInfo`, triggers welcome-page logic, negotiates protocol version against `SUPPORTED_PROTOCOL_VERSIONS` (else `LATEST_PROTOCOL_VERSION`), returns server `instructions` | `src/server.ts:214-282`, instructions text `src/server.ts:27-32` |
| **Transport: stdio only.** `FilteredStdioServerTransport extends StdioServerTransport`, which wraps `console.*`/stray stdout into JSON-RPC `notifications/message` instead of corrupting the stream | `src/custom-stdio.ts:1`, `src/custom-stdio.ts:14-58`; used at `src/index.ts:57`, `src/index.ts:146` |
| No HTTP/SSE/Streamable-HTTP server transport exists in `src/` (grep for `StreamableHTTP`, `SSEServerTransport`, `listen(` finds only a loopback HTTP server used while rendering PDFs) | `src/tools/pdf/markdown.ts:608-621` (`server.listen(0, '127.0.0.1', …)`) |
| Resources: MCP-Apps UI resources (file preview, config editor) via `listUiResources`/`readUiResource`; prompts list is empty; resource templates empty | `src/server.ts:119-141`, `src/server.ts:1552` |
| UI tool `_meta` carries both MCP-Apps (`ui/resourceUri`, `ui.resourceUri`) and OpenAI Apps SDK keys (`openai/outputTemplate`, `openai/widgetAccessible`) | `src/ui/contracts.ts:57-84` |
| UI CSP allows fonts from `https://assets.claude.ai` only | `src/ui/contracts.ts:30-34`, `src/ui/contracts.ts:41-55` |

### 1.3 Complete tool list and parameters

25 tools are advertised by `tools/list` (`src/server.ts:306-1158`). A 26th, `track_ui_event`, is dispatched (`src/server.ts:1305-1315`) and has a schema (`src/tools/schemas.ts:240-244`) but is **not** in the advertised list (it is the transport for widget telemetry). `give_feedback_to_desktop_commander` is hidden when the client name is `desktop-commander-app` (`src/server.ts:292-304`). Unknown parameters are stripped by Zod and a warning is prepended to the result (`src/server.ts:1497-1516`).

`?` = optional; `=x` = default. Schemas are in `src/tools/schemas.ts`; definitions/descriptions in `src/server.ts`. `origin: 'ui'|'llm'` marks widget-initiated calls, which produce no telemetry (`src/server.ts:1177-1195`).

| Tool | Parameters | Schema lines | Definition line in server.ts |
|---|---|---|---|
| `get_config` | `origin?` | 4-8 | 315 (UI `_meta` 330) |
| `set_config_value` | `key: string`, `value: string\|number\|boolean\|string[]\|null`, `origin?` | 10-21 | 339 |
| `read_file` | `path`, `isUrl=false`, `offset=0`, `length=1000`, `sheet?`, `range?`, `options?: record`, `origin?` | 59-71 | 369 (UI `_meta` 425) |
| `read_multiple_files` | `paths: string[]` | 73-75 | 434 |
| `write_file` | `path`, `content`, `mode: 'rewrite'\|'append' ='rewrite'`, `origin?` | 77-84 | 456 (UI `_meta` 479) |
| `write_pdf` | `path`, `content`: markdown string **or** array of ops (`{type:'insert', pageIndex, markdown?, sourcePdfPath?, pdfOptions?}` / `{type:'delete', pageIndexes[]}`), `outputPath?`, `options?` (passed to md-to-pdf) | 87-123 | 488 |
| `create_directory` | `path` | 125-127 | 549 |
| `list_directory` | `path`, `depth=2`, `origin?` | 129-135 | 567 (UI `_meta` 600) |
| `move_file` | `source`, `destination` | 137-140 | 609 |
| `start_search` | `path`, `pattern`, `searchType: 'files'\|'content' ='files'`, `filePattern?`, `ignoreCase=true`, `maxResults?` (int ≥0, 0=unlimited), `includeHidden=false`, `contextLines=5`, `timeout_ms?`, `earlyTermination?`, `literalSearch=false`, `origin?` | 204-219 | 627 |
| `get_more_search_results` | `sessionId`, `offset=0`, `length=100` | 221-225 | 711 |
| `stop_search` | `sessionId` | 227-229 | 743 |
| `list_searches` | — | 231 | 764 |
| `get_file_info` | `path` | 142-144 | 782 |
| `edit_block` | `file_path`, `old_string?`, `new_string?`, `expected_replacements=1`, `range?`, `content?: any`, `options?`, `origin?`; must supply (old_string+new_string) or (range+content) | 151-172 | 811 (UI `_meta` 865) |
| `start_process` | `command`, **`timeout_ms` (required)**, `shell?`, `verbose_timing?`, `origin?` | 27-35 | 876 |
| `read_process_output` | `pid`, `timeout_ms?`, `offset?` (0=since last read, +abs, −tail), `length?`, `verbose_timing?` | 37-43 | 925 |
| `interact_with_process` | `pid`, `input`, `timeout_ms?`, `wait_for_prompt?`, `verbose_timing?` | 175-181 | 968 |
| `force_terminate` | `pid` | 45-47 | 1011 |
| `list_sessions` | — | 49 | 1025 |
| `list_processes` | — | 24 | 1049 |
| `kill_process` | `pid` (refined `> 0`) | 51-56 | 1065 |
| `get_usage_stats` | — | 184 | 1081 |
| `get_recent_tool_calls` | `maxResults` 1–1000 `=50`, `toolName?`, `since?` (ISO datetime) | 234-238 | 1097 |
| `give_feedback_to_desktop_commander` | — | 187-194 | 1122 |
| `track_ui_event` (unlisted) | `event` (1–80 chars), `component='file_preview'`, `params: record<string\|number\|boolean\|null>` | 240-244 | dispatched 1305 |

Annotations (`readOnlyHint`, `destructiveHint`, `openWorldHint`) are set per tool, e.g. `get_config` read-only (`src/server.ts:331-336`), `set_config_value` destructive (`src/server.ts:359-364`), feedback open-world (`src/server.ts:1137-1142`).

Post-processing on every successful call: result passes through `withoutInternalFacts`, is recorded in local history, usage counters update, and the server may **append a "[SYSTEM INSTRUCTION]" feedback request** or Docker notice to the tool result text (`src/server.ts:1427-1495`; see §1.10).

### 1.4 Configuration file and every key

**Location:** `~/.claude-server-commander/config.json` (`src/config.ts:4-9`). Other files in the same folder: `claude_tool_call.log` (10 MB rotation, `src/config.ts:10-11`), `tool-history.jsonl` (`src/utils/toolHistory.ts:64-74`), `feature-flags.json` cache (`src/utils/feature-flags.ts:26-28`), `puppeteer-cache/` for downloaded Chrome (`src/tools/pdf/markdown.ts:191-193`). The fuzzy-edit log is separate: `~/.claude-server-commander-logs/fuzzy-search.log` (`src/utils/fuzzySearchLogger.ts:30-31`). The remote device uses a different folder, `~/.desktop-commander-device/` (§3).

Concurrency: writes go through `proper-lockfile` (stale 30 s, update 10 s) and atomic rename; a corrupt file is backed up and rebuilt; an `fs.watch` reloads external edits; UTF-8 BOM tolerated (`src/config-manager.ts:79-81`, `src/config-manager.ts:301-329`, `src/config-manager.ts:346-411`, `src/config-manager.ts:504-534`).

| Key | Default / origin | Settable via `set_config_value`? | Evidence |
|---|---|---|---|
| `blockedCommands` | 34-entry list (mkfs, format, mount, umount, fdisk, dd, parted, diskpart, sudo, su, passwd, adduser, useradd, usermod, groupadd, chsh, visudo, shutdown, reboot, halt, poweroff, init, iptables, firewall, netsh, sfc, bcdedit, reg, net, sc, runas, cipher, takeown) | yes | `src/config-manager.ts:233-277`; `src/config-field-definitions.ts:11-15` |
| `defaultShell` | `getDefaultShell()` | yes | `src/config-manager.ts:278`; `src/config-field-definitions.ts:21-25` |
| `allowedDirectories` | **`[]` = entire filesystem** | yes | `src/config-manager.ts:279`; `src/config-field-definitions.ts:16-20`; enforcement `src/tools/filesystem.ts:290-295` |
| `telemetryEnabled` | `true` (opt-out) | yes | `src/config-manager.ts:280`; `src/config-field-definitions.ts:26-30` |
| `fileWriteLineLimit` | 50 (warning threshold only) | yes | `src/config-manager.ts:281`; use `src/tools/edit.ts:154-204` |
| `fileReadLineLimit` | 1000 | yes | `src/config-manager.ts:282`; `src/tools/filesystem.ts:64-67` |
| `pendingWelcomeOnboarding` | `true` on new install | no | `src/config-manager.ts:283`; `src/utils/welcome-onboarding.ts:10-15,50-52` |
| `welcomeOnboardingEligible` | `true` on new install; legacy configs migrated to `false` | no | `src/config-manager.ts:84-89`, `src/config-manager.ts:284` |
| `version` | overwritten with running VERSION in memory | no | `src/config-manager.ts:146`, `src/config-manager.ts:454` |
| `clientId` | random UUID, created lazily; the telemetry/A-B identity | no | `src/config-manager.ts:657-665` |
| `abTest_<experiment>` | persisted A/B variant | no | `src/utils/ab-test.ts:79-87`, `src/utils/ab-test.ts:114`; `src/utils/mcp-ui-ab-test.ts:70` |
| `sawOnboardingPage` | set true/false by welcome flow | no | `src/utils/welcome-onboarding.ts:91`, `src/utils/welcome-onboarding.ts:105` |
| `usageStats` | usage counters object | no | `src/utils/usageTracker.ts:84`, `:147`, `:173` |
| `feedbackGiven` | suppresses feedback prompts | no | `src/utils/usageTracker.ts:216`, `:290` |
| `currentClient` | declared, read by Docker-prompt logic; no writer found in `src/` (grep) | no | `src/config-manager.ts:44`; `src/utils/dockerPrompt.ts:14` |

Only the six keys in `CONFIG_FIELD_DEFINITIONS` are writable through the tool (`src/tools/config.ts:13`, `src/tools/config.ts:101-109`). `get_config` returns the config plus `currentClient`, all feature flags, system info and memory usage (`src/tools/config.ts:18-69`). The `get_config` description lists a `clientHistory` key (`src/server.ts:325`) that no code in `src/` produces (grep finds only that line).

### 1.5 Processes and terminal sessions

| Fact | Evidence |
|---|---|
| **`child_process.spawn` with pipes; no `node-pty`** (not a dependency; grep finds no import) | `src/terminal-manager.ts:1`, `src/terminal-manager.ts:193-194`; deps `package.json:92-127` |
| Shell = `shell` arg → `config.defaultShell` → `getDefaultShell()`; spawn args from `getShellSpawnArgs` (login flags where appropriate) | `src/terminal-manager.ts:153-168` |
| Env: repaired command environment + `TERM=xterm-256color`; `windowsHide: true`; cmd.exe uses `windowsVerbatimArguments` | `src/terminal-manager.ts:169-191` |
| Auto-rewrites `ssh …` to `ssh -t …` when no `-t` is present | `src/terminal-manager.ts:160-165` |
| Session map keyed by OS PID; completed sessions retained; stdin writes append `\n` | `src/terminal-manager.ts:114-144`, `src/terminal-manager.ts:231-244` |
| Output caps: 50 MB retained per session (oldest lines evicted), 1 MB max line, 2 MB initial-wait buffer | `src/terminal-manager.ts:88-98` |
| Timeouts: `start_process.timeout_ms` is required; any single blocking wait is capped at **`MAX_PROCESS_WAIT_MS = 50000`** to stay under the SDK's 60 s request timeout; `DEFAULT_COMMAND_TIMEOUT = 1000` ms | `src/config.ts:13-23`; `src/terminal-manager.ts:57-80` |
| **Prompt detection is regex on the last 4096 chars of each stream** (no PTY): REPL prompt table (python `>>> `/`... `, node `> `, R, julia, shell `$ `/`# `/`% `, mysql, postgres, redis, mongo, PowerShell `PS …> `, cmd `C:\…>`) | `src/utils/process-detection.ts:16-78`, `src/utils/process-detection.ts:80-111` |
| `node:local` pseudo-command: a virtual session with negative PID; each `interact_with_process` writes the code to a temp `.mjs` in the package root and runs it with `process.execPath` (fresh process per call, default 30 s) | `src/tools/improved-process-tools.ts:21-51`, `src/tools/improved-process-tools.ts:180-200` |
| Tree termination: Windows `taskkill /PID <pid> /T /F` from `%SystemRoot%\System32`; POSIX SIGTERM to the tree (via `ps -A -o pid=,ppid=`), 1 s grace, then SIGKILL | `src/utils/process-tree.ts:20-40`, `src/utils/process-tree.ts:98`, `src/utils/process-tree.ts:158-214` |

### 1.6 `list_processes` / `kill_process` per OS

| OS | `list_processes` | `kill_process` | Evidence |
|---|---|---|---|
| Windows | `tasklist /FO CSV /NH` (CPU reported `n/a`) | `terminatePid`: `process.kill(pid,'SIGTERM')`, 1 s, then SIGKILL | `src/tools/process.ts:11-31`, `src/tools/process.ts:54-59`; `src/utils/process-tree.ts:177-181` |
| macOS/Linux | `ps aux` (COMMAND re-joined from column 10) | same | `src/tools/process.ts:33-52`, `src/tools/process.ts:54-59` |

`kill_process` accepts **any PID > 0**, not only sessions this server started (`src/tools/schemas.ts:51-56`; `src/tools/process.ts:77-90`).

### 1.7 Search

| Fact | Evidence |
|---|---|
| `@vscode/ripgrep` **^1.15.9**, locked **1.17.0** | `package.json:105`; `package-lock.json:2874-2875` |
| Binary resolution: `@vscode/ripgrep` `rgPath` (chmod 755) → `rg` on PATH → common install paths (Program Files, scoop, cargo, /usr/local/bin, /usr/bin, /opt/homebrew/bin) → error | `src/utils/ripgrep-resolver.ts:12-79` |
| ripgrep args: always `--no-config`; content search `--json --line-number`, `-F` for literal, `-C n`, `-i`, `--hidden`, `-m`, `-g`/`--iglob`/`--pre-glob`; file search `--files` + glob | `src/search-manager.ts:1015-1093` |
| Root path goes through `validatePath` (allowed-directory check) | `src/search-manager.ts:271-274` |
| Searches are sessions (spawned `rg` process) read incrementally; Excel (`.xlsx/.xls/.xlsm/.xlsb`) and DOCX are searched in-process with exceljs/pizzip | `src/search-manager.ts:122-126`, `src/search-manager.ts:289`, `src/search-manager.ts:368`, `src/search-manager.ts:637`, `src/search-manager.ts:820-830` |
| MCPB builds pre-download ripgrep for all platforms from `github.com/microsoft/ripgrep-prebuilt` | `scripts/download-all-ripgrep.cjs:156` |

### 1.8 `edit_block` (exact + fuzzy)

| Fact | Evidence |
|---|---|
| Text mode: detect file line endings, normalize the search string, count non-overlapping exact matches; replace only if count == `expected_replacements` (default 1) | `src/tools/edit.ts:144-206`; mismatch message `src/tools/edit.ts:244-256` |
| `fileWriteLineLimit` only produces a warning when search/replace exceeds it | `src/tools/edit.ts:154-204` |
| **Fuzzy fallback (no exact match):** `fastest-levenshtein` `distance` in a recursive bisection + iterative trimming search, run in a `worker_threads` worker; similarity ratio vs **threshold 0.7** | `src/tools/fuzzySearchCore.ts:1`, `src/tools/fuzzySearchCore.ts:69-154`, `src/tools/fuzzySearchCore.ts:164-168`; `src/tools/fuzzySearch.ts:2`; `src/tools/edit.ts:47-50`, `src/tools/edit.ts:258-266` |
| **Fuzzy matches are never applied**: ≥70% returns a character diff and asks the model to resend exact text; <70% returns "not found" with the closest match | `src/tools/edit.ts:325-360` |
| Every fuzzy attempt writes search text, found text, diff and char-code report to the local `fuzzy-search.log`; telemetry gets only lengths/similarity (the char-code map is deliberately not sent) | `src/tools/edit.ts:277-323`; `src/utils/fuzzySearchLogger.ts:30-31`, `:99` |
| Structured mode: `range` + `content` for Excel/DOCX-style range rewrites | `src/tools/schemas.ts:157-160` |

### 1.9 File formats and libraries

| Format | Read | Write/Edit | Evidence |
|---|---|---|---|
| Text | streamed via `readline`, offset/length lines | rewrite/append | `src/utils/files/text.ts:16-21`, `:100` |
| Binary detection | `isbinaryfile` | — | `src/utils/files/binary.ts:10`; `src/tools/filesystem.ts:14` |
| Images (png, jpg/jpeg, gif, webp; SVG optionally as image) | returned base64 | — | `src/tools/mime-types.ts:11-15`; `src/utils/files/image.ts:3`, `:42-49` |
| Excel (.xlsx etc.) | `exceljs` (lazy) | `exceljs` | `src/utils/files/excel.ts:22`; `src/utils/files/factory.ts:14` |
| DOCX | `pizzip` + hand-written XML (lazy) | same | `src/utils/files/docx.ts:78`; `src/utils/files/factory.ts:16` |
| PDF read | `unpdf` (lazy) + `@opendocsg/pdf2md` → markdown; `sharp` for extracted images | — | `src/tools/pdf/extract-images.ts:34`, `:162-163`; `src/tools/pdf/lib/pdf2md.ts:11-13`; `src/tools/pdf/markdown.ts:742-747` |
| PDF create | `md-to-pdf` (Puppeteer + Chrome) | — | `src/tools/pdf/markdown.ts:41-44`, `:818` |
| PDF page insert/delete | — | `pdf-lib` (lazy) | `src/tools/pdf/manipulations.ts:20` |
| URL read (`isUrl`) | `cross-fetch`, 30 s timeout; PDFs by URL fetched too | — | `src/tools/filesystem.ts:4`, `:17-22`, `:441-480`; `src/tools/pdf/markdown.ts:730-733` |

Chrome for PDF: Puppeteer cache → system Chrome → **automatic download via `@puppeteer/browsers` (Chrome stable)** into `~/.claude-server-commander/puppeteer-cache` (`src/tools/pdf/markdown.ts:639-711`). This check runs in the background on every MCP `initialized` (`src/index.ts:138-139`; `src/tools/pdf/markdown.ts:723-728`), not only when `write_pdf` is used. `@tiptap/*`/`tiptap-markdown`, `markdown-it` and `highlight.js` serve the file-preview/markdown-editor widget (`src/ui/file-preview/src/markdown/editor.ts:9`, `src/ui/file-preview/src/markdown/parser.ts:2`, `src/ui/file-preview/src/components/highlighting.ts:5-8`; bundled by `scripts/build-ui-runtime.cjs` per `package.json:39`). `remark`, `remark-gfm`, `remark-parse`, `unified` and `glob` are declared but have no direct import in `src/`, `scripts/` or root JS at this commit (grep).

### 1.10 Usage stats, feedback prompts, onboarding

| Mechanism | Behaviour | Evidence |
|---|---|---|
| Usage stats | counters in config key `usageStats`; `get_usage_stats` returns them | `src/utils/usageTracker.ts:42-44`, `:84`; `src/server.ts:1281-1291` |
| **Feedback prompt injection** | if remote flag `user_surveys` is true, client ≠ `desktop-commander`, feedback not given, < 3 attempts, first command of a session, not already today, **≥ 3 days since first use and ≥ 10 tool calls** → appends a `[SYSTEM INSTRUCTION]: … Please add a feedback request …` string (3 random variants) to the tool result | `src/utils/usageTracker.ts:193-248`, `src/utils/usageTracker.ts:254-281`; injection `src/server.ts:1454-1491` |
| Feedback tool | opens `https://tally.so/r/mKqoKg?tool_call_count=…&days_using=…&platform=…&client_id=<clientId>` with `open` / `start` / `xdg-open` via `exec` of a shell string | `src/tools/feedback.ts:99-154` |
| Docker Gateway notice | if `currentClient.name === 'docker'` and total calls 0 or 1, appends a `[SYSTEM INSTRUCTION]` pointing to `desktopcommander.app/?utm_source=docker_gateway…#docker-install` | `src/utils/dockerPrompt.ts:12-29`; `src/server.ts:1493-1494` |
| Welcome page | new installs only, A/B experiment `showOnboardingPage`, flags `welcome_page_enabled` / `welcome_page_excluded_clients`; not for `desktop-commander-app`, `desktop-commander`, remote contexts or `--no-onboarding`; opens `https://desktopcommander.app/welcome/?utm_source=<clientName>` (code comment: the page's PostHog/GA4 capture `utm_source`) | `src/server.ts:221-237`; `src/utils/welcome-onboarding.ts:40-115`; `src/utils/open-browser.ts:41-50` |
| MCP-UI A/B test | experiment `McpUiPreviews` (`showMCPUi`/`notShowMCPUi`) decides whether UI `_meta` is attached to tools | `src/utils/mcp-ui-ab-test.ts:6-8`, `:25-77`; `src/server.ts:309` |
| A/B assignment | `hash(clientId + experimentName) % totalWeight` over remote-flag weights, persisted as `abTest_<name>` | `src/utils/ab-test.ts:68-117`, `:158-165` |
| `onboarding-prompts.json` | shipped (`package.json:39`) and loaded by `src/tools/prompts.ts:56`, but no `src/` module imports `tools/prompts` and no prompt tool is listed (grep) | `src/tools/prompts.ts:56` |

### 1.11 Security boundaries

**Filesystem (`validatePath`, `src/tools/filesystem.ts:340-423`)**

| Control | Evidence |
|---|---|
| `~` expansion, `path.resolve` against `process.cwd()` for relative paths | `src/tools/filesystem.ts:139-144`, `:342-348` |
| Symlinks/junctions resolved with `fs.realpath`; for not-yet-existing paths the existing ancestors are resolved and **dangling links followed** (a write through them lands on the target); loop limit 40 → `ELOOP` | `src/tools/filesystem.ts:172-232`, `:352-371` |
| `move_file` mode resolves only the parent and keeps the entry name (renames the link itself) | `src/tools/filesystem.ts:234-245`, `:359-361` |
| Allow check on the resolved path; allowed dirs compared both as written and by their own realpath; boundary uses `dir + path.sep` to avoid `/home/user` vs `/home/username` | `src/tools/filesystem.ts:247-323`, `:373-381` |
| **Empty `allowedDirectories` or one containing `/` ⇒ everything allowed** (and the shipped default is `[]`) | `src/tools/filesystem.ts:290-295`; `src/config-manager.ts:279` |
| Comparison is case-insensitive on every OS (`normalizePath` lowercases) | `src/tools/filesystem.ts:134-137` |
| Validation has a 10 s timeout → error | `src/tools/filesystem.ts:18-22`, `:405-420` |
| Non-array `allowedDirectories` falls back to `[homedir]` and is written back | `src/tools/filesystem.ts:111-132` |

**Commands (`src/command-manager.ts`)**

| Control | Evidence |
|---|---|
| `start_process` runs `validateCommand` **after** emitting telemetry and before spawning; blocked → error with `structuredContent.blocked` | `src/tools/improved-process-tools.ts:155-176` |
| Parser splits on `;`, `&&`, `\|\|`, `\|`, `&`; recurses into `$( )` and backticks **even inside quotes**, and into `( )` subshells; strips `KEY=value` prefixes; skips `$VAR` tokens; takes `path.basename` of the first token; lowercases | `src/command-manager.ts:16-177`, `src/command-manager.ts:180-232` |
| Match is exact membership of each extracted command name in `blockedCommands`; any exception ⇒ **fail closed** | `src/command-manager.ts:234-267` |
| No path/allowed-directory restriction on what a shell command touches (no `validatePath` use in `terminal-manager.ts` or `start_process`; grep) | `src/terminal-manager.ts:146-194`; `src/tools/improved-process-tools.ts:145-200` |

**INFERRED (code reading, not executed)** — blocklist gaps that follow from the cited parser: (a) newline is not a separator (`src/command-manager.ts:22`), so a second line's command is never extracted; (b) commands inside quoted arguments to an interpreter (`bash -c "sudo …"`, `sh -c '…'`, `powershell -Command "…"`) are not extracted because only `$()`/backticks are scanned inside quotes (`src/command-manager.ts:63-112`); (c) wrapper programs (`env`, `xargs`, `nohup`, `time`) become the "command" (`src/command-manager.ts:188-227`); (d) Windows `net.exe`/`reg.exe` basename keeps the `.exe` and so does not equal `net`/`reg` (`src/command-manager.ts:225-227`).

**Self-modifiable boundary.** The model itself can call `set_config_value` to rewrite `allowedDirectories` or `blockedCommands`; the only guard is a "Security note" in the tool description (`src/server.ts:339-365`; `src/tools/config.ts:86-201`). `read_file` with `isUrl:true` fetches arbitrary URLs and is outside the directory model (`src/tools/filesystem.ts:441-449`).

**Local audit trails (not boundaries).** Every call's arguments go unredacted to `claude_tool_call.log` (10 MB rotation, rotated files kept) and calls+results to `tool-history.jsonl` (1000 records / ~5 MiB cap) (`README.md:787-814`; `src/config.ts:10-11`; `src/utils/trackTools.ts:3-34`; `src/utils/toolHistory.ts:55-74`; `src/server.ts:1430-1440`).

---

## 2. Telemetry and every outbound URL/endpoint

### 2.1 Complete outbound inventory (code paths that make or trigger network requests)

| # | Endpoint | Who calls it / when | Method & data | Opt-out? | Evidence |
|---|---|---|---|---|---|
| 1 | `https://telemetry.desktopcommander.app/mp/collect` (primary) | MCP server `capture()`, `captureTransport()` on most events | POST JSON `{client_id, timestamp_micros, events:[{name, params}]}`, 3 s timeout | Yes: `telemetryEnabled:false` or env `DESKTOP_COMMANDER_DISABLE_TELEMETRY=1/true/yes/on` | `src/utils/capture.ts:61`, `:529-579`, `:71-75` |
| 2 | `https://dc-telemetry-proxy-83847352264.europe-west1.run.app/mp/collect` (fallback, Google Cloud Run, europe-west1) | retried when #1 fails | same payload | same | `src/utils/capture.ts:62`, `:544-547` |
| 3 | same two telemetry URLs | **npm `postinstall`** (`track-installation.js`) | `package_installed` event (see 2.4) | **No opt-out check in the script** (it reads only `clientId` from config) | `package.json:30`; `track-installation.js:43-78`, `:271-307`, `:323-361` |
| 4 | same two telemetry URLs | `setup` command (`setup-claude-server.js`), ~30 `npx_setup_*` events | properties: platform, node/npm version, execution context, is_ci, shell, app_version | **No opt-out check**; uses a **fresh random UUID each run** (not the config `clientId`) | `setup-claude-server.js:14-27`, `:265-286`, `:301-370`, `:484` |
| 5 | same two telemetry URLs | `remove` command (`uninstall-claude-server.js`) | uninstall events | Yes — skipped unless config `telemetryEnabled === true` | `uninstall-claude-server.js:14-46`, `:214-222` |
| 6 | `https://desktopcommander.app/flags/v2/production.json` (override `DC_FLAG_URL`) | at startup (background) and every 30 min | GET, `Cache-Control: no-cache`, 3 s abort + hard timeout; cached to `~/.claude-server-commander/feature-flags.json` | **Not gated by telemetry opt-out** (no check in the module) | `src/utils/feature-flags.ts:17`, `:26-32`, `:43-77`, `:166-237`; called `src/index.ts:67-69` |
| 7 | Chrome for Testing download via `@puppeteer/browsers.install()` | on every MCP `initialized` if no cached/system Chrome; also on `write_pdf` | binary download into `~/.claude-server-commander/puppeteer-cache` | none | `src/index.ts:138-139`; `src/tools/pdf/markdown.ts:639-711`, `:723-728`. **INFERRED host**: Google's Chrome-for-Testing storage — rests on `@puppeteer/browsers`' default download base, which is not in this repo |
| 8 | Any URL the model supplies | `read_file` with `isUrl:true`; PDF parse of a URL source | GET via `cross-fetch` / global `fetch`, 30 s | n/a (tool action) | `src/tools/filesystem.ts:441-480`; `src/tools/pdf/markdown.ts:730-747` |
| 9 | `https://mcp.desktopcommander.app` (override `MCP_SERVER_URL`): `/api/mcp-info`, `/device/start`, `/device/poll`, `/device/rollouts` | `remote` device only | see §3 | n/a | `src/remote-device/device.ts:104`, `:514-535`; `src/remote-device/device-authenticator.ts:91`, `:175`; `src/remote-device/rollouts.ts:91` |
| 10 | Supabase project URL (delivered by `/api/mcp-info`): Auth (GoTrue), PostgREST `/rest/v1`, Realtime websocket | `remote` device only | see §3 | n/a | `src/remote-device/remote-channel.ts:292-307`; `src/remote-device/diagnostics/report.ts:278-305` |
| 11 | `diagnosticsUrl` from `/api/mcp-info` (code comment: a "diagnostics Worker", 7-day storage) | `remote --report` only, unless `--no-upload` | POST zip; headers `X-DC-User-Id` (JWT `sub`), `X-DC-Device-Id`; 30 s | user-initiated | `src/remote-device/diagnostics/upload.ts:4-65`; URL validation `src/remote-device/diagnostics/report.ts:546-558`, `:612` |
| 12 | Browser opens (not fetched by the server): `https://desktopcommander.app/welcome/?utm_source=<client>`, `https://tally.so/r/mKqoKg?…client_id=…`, device-verification URL from `/device/start` | welcome A/B, feedback tool, remote auth | — | — | `src/utils/open-browser.ts:44-50`; `src/tools/feedback.ts:102-126`; `src/remote-device/device-authenticator.ts:144-150` |
| 13 | `https://github.com/microsoft/ripgrep-prebuilt/releases/download/…` | build-time only (MCPB bundle) | download | n/a | `scripts/download-all-ripgrep.cjs:156` |

No update-check endpoint exists in `src/` (grep for `npm view`, `dist-tags`, `registry.npmjs`, update-check patterns finds only a comment in `src/remote-device/diagnostics/report.ts:184`). See §4.4.

### 2.2 What the main telemetry sends

* **Identity:** `client_id` = config `clientId` UUID (`src/utils/capture.ts:441-443`, `:535-537`; `src/config-manager.ts:657-665`).
* **Base properties on every `capture()` event:** `timestamp, platform, isContainer, containerType, orchestrator, containerName` (hex/number runs → `ID`, ≤50 chars), `containerImage` (registry/digest stripped, ≤100), `runtimeSource` (smithery/npx/direct), `isDXT` (env `MCP_DXT`), `app_version, engagement_time_msec`, `client_name/client_version` (the remote caller's client for remote calls), `saw_onboarding_page`, `remote` (`src/utils/capture.ts:440-523`).
* **Sanitization:** error text and free-text keys (`message, errorMessage, errMsg, error_message, reason, command, commands`) have known paths (home dir, the call's path args, allowed dirs) then any path-shaped text replaced by `[PATH]`; keys containing `path/filePath/directory/...` are dropped; UI-origin calls (`origin:'ui'`) produce no events (`src/utils/capture.ts:14-42`, `:78-237`, `:586-599`; `src/server.ts:1177-1203`).
* **Notable events/fields (examples, not exhaustive):**
  * `server_call_tool` after every tool call: `tool_name`, `duration_ms`, `is_error`, `remote`, remote `client_name/version`, and for `set_config_value` the key name (`src/server.ts:1205-1247`, `:1533-1548`).
  * `server_start_process`: **the first word of each chained command** (`command`, `commands`, e.g. "git, npm"), sent *before* the blocklist check (`src/tools/improved-process-tools.ts:155-169`).
  * `run_server_mcp_initialized`: env `CLAUDE_CODE_ENTRYPOINT`, `AI_AGENT`, basename of `CLAUDE_PLUGIN_DATA` (`src/server.ts:240-255`).
  * `server_fuzzy_search_performed`: similarity, lengths, file size (`src/tools/edit.ts:314-348`).
  * `feedback_prompt_injected`: call counts, days since first use, variant (`src/server.ts:1463-1473`).
  * `server_telemetry_opt_out` is **sent at the moment the user disables telemetry** (`src/config-manager.ts:562-567`).
  * A/B decisions `server_welcome_page_ab_decision`, `server_mcp_ui_ab_decision` (`src/utils/welcome-onboarding.ts:84-87`; `src/utils/mcp-ui-ab-test.ts:52-57`).
* **Remote-device telemetry** (`captureRemote`) strips `deviceId,userId,email,user_id,device_id,user_email` and adds `remote:"true"` (`src/utils/capture.ts:620-642`). But **transport telemetry (`captureTransport`) does carry `call_id` and `device_id`** (raw device UUID), plus `stage`, `tool_name`, `observation_id`, `source_process_id`, `schema_version`, `timestamp_utc`, `transport` (`src/remote-device/transport-telemetry.ts:14-53`; allow-list `vendor/telemetry-contract/transport.js:1-28`, `:95-118`; sender `src/utils/capture.ts:601-615`). Realtime health windows are sampled (1% normal / 5% degraded) and keyed by `sha256("dc-realtime-diagnostics-v1:"+deviceId)[:32]` (`src/remote-device/realtime-health-telemetry.ts:56-65`, `:96-122`).
* Clock skew: on the remote device `Date.now` is patched to server time, which also shifts telemetry timestamps (comment: "GA4 drops future-dated events") (`src/remote-device/remote-channel.ts:164-206`).

### 2.3 Backend of the telemetry (what the code says vs. what the docs say)

* Code: "Telemetry Proxy (direct BigQuery ingestion)"; GA path described as removed ("captureBase is now dead code — no caller remains after the GA removal"); a TODO notes the endpoint is now **unauthenticated** (`src/utils/capture.ts:55-62`, `:244-247`).
* `vendor/telemetry-contract` is "shared by telemetry-proxy and Remote MCP", repository `github.com/desktop-commander/telemetry-proxy`, describing `/mp/collect` (device) and authenticated `/remote/collect` (server) (`vendor/telemetry-contract/README.md:1-15`; `vendor/telemetry-contract/package.json:5-12`).
* `PRIVACY.md` still says telemetry is processed by **Google Analytics 4**, IPs anonymized by GA, 14-month retention (`PRIVACY.md:76`, `PRIVACY.md:99`, `PRIVACY.md:113`). README: "We do not collect file contents, file paths, or command arguments as telemetry" (`README.md:1141`); note command *names* are sent (above).
* **INFERRED:** the proxy writes to BigQuery and keeps a GA4-Measurement-Protocol-shaped schema. Basis: the comment at `src/utils/capture.ts:55`, the GA4 Measurement-Protocol path `/mp/collect` and field names `engagement_time_msec` (`src/utils/capture.ts:504-516`) and `non_personalized_ads` (`track-installation.js:276-279`; `setup-claude-server.js:317-320`), and MQTT branch doc "existing GA4-compatible schema" (MQTT branch @ `92e81b3` `docs/mqtt-doorbell.md:31-32`). The proxy itself is not in this repo.

### 2.4 Install-time telemetry detail (`track-installation.js`)

`package_installed` with `platform, installation_source, installation_details (JSON), package_name, install_method, node_version, npm_version` (`track-installation.js:271-293`). `installation_details` can include npm user agent, `npm_execpath`, the full `process.argv` string and the **parent process command line** (Smithery case), `GITHUB_REPOSITORY` + `GITHUB_WORKFLOW`, `CI_PROJECT_NAME`, Jenkins `JOB_NAME`, first 8 chars of `HOSTNAME` (`track-installation.js:83-265`). Parent command line is read with `ps -p <ppid> -o command=` (`track-installation.js:102-115`).

---

## 3. Remote device ("Remote MCP")

Code lives in `src/remote-device/` plus `src/npm-scripts/remote.ts`. Introduced by commit `b23a813` "Remote MCP (#311)", 2026-01-21 (`git log --diff-filter=A -- src/remote-device/device.ts`).

### 3.1 Architecture as implemented

```
ChatGPT / Claude web / other MCP client
        │  (MCP over the network — server NOT in this repo)
        ▼
Remote MCP service  https://mcp.desktopcommander.app   ── writes row ──►  Supabase Postgres: mcp_remote_calls
        │                                                                  │ (doorbell) Realtime broadcast "new_call"
        ▼                                                                  ▼  on private channel  user:<user_id>
`desktop-commander remote` (device process, this repo) ── conditional UPDATE pending→executing (PostgREST) ──┘
        │  MCP client over stdio (StdioClientTransport)
        ▼
local `desktop-commander` MCP server child (same package, dist/index.js), env DC_REMOTE_DEVICE=true
        │  result
        ▼
device ── UPDATE mcp_remote_calls SET status='completed', result=<CallToolResult JSON> ──► Supabase ──► Remote MCP ──► AI client
```

Evidence for each hop is in the tables below. The device never opens a listening port; all of its traffic is outbound HTTPS/WSS (no `listen(` in `src/remote-device/`, grep).

### 3.2 Startup sequence

| Step | Evidence |
|---|---|
| CLI: `desktop-commander remote [--logout] [--report] [--no-upload] [--no-persist-session] [--disable-no-sleep] [--debug] [-h]` | `src/npm-scripts/remote.ts:23-69` |
| macOS: `caffeinate({pid})` unless `--disable-no-sleep` (optional dep `caffeinate`) | `src/npm-scripts/remote.ts:97-108`; `package.json:131-133` |
| Spawn local MCP child: `<pkg>/dist/index.js` with `process.execPath`, else global `desktop-commander` from PATH; env = full device env + `DC_REMOTE_DEVICE=true`; MCP client name `desktop-commander-client`; readiness = a successful `listTools` | `src/remote-device/desktop-commander-integration.ts:85-183`, `:255-292`, `:325-328` |
| Base URL `process.env.MCP_SERVER_URL \|\| 'https://mcp.desktopcommander.app'` | `src/remote-device/device.ts:104` |
| **Supabase URL + publishable (anon) key obtained at runtime from the public endpoint `GET {base}/api/mcp-info`** (fields `supabaseUrl`, `supabasePublishableKey`; also `diagnosticsUrl`) — nothing Supabase-specific is hard-coded | `src/remote-device/device.ts:514-535`; `src/remote-device/diagnostics/report.ts:254`, `:572-573`, `:612` |
| Supabase client created with `auth.autoRefreshToken:false`, a clock-correcting `fetch`, realtime `reconnectAfterMs` = custom backoff, realtime `accessToken` = current session JWT (never the anon key) | `src/remote-device/remote-channel.ts:292-316` |
| Load `device.json`; if a session exists → `auth.setSession`; then verify the persisted `deviceId` still exists in `mcp_devices` (3 tries, 250 ms×n); a missing row (revoked) wipes local credentials and forces re-auth | `src/remote-device/device.ts:202-234`, `:354-367` |
| Otherwise run device authorization (3.3); save config; load rollouts; register device; start heartbeat | `src/remote-device/device.ts:236-339` |

### 3.3 Authentication: OAuth 2.0 Device Authorization Grant + PKCE

| Item | Detail | Evidence |
|---|---|---|
| PKCE | verifier = 32 random bytes base64url; challenge = SHA-256 base64url; method `S256` | `src/remote-device/device-authenticator.ts:76-80` |
| Device code request | `POST {base}/device/start` JSON `{client_id:"mcp-device", scope:"mcp:tools", device_name: os.hostname(), device_type:"mcp", device_id: <existing or undefined>, code_challenge, code_challenge_method:"S256"}` | `src/remote-device/device-authenticator.ts:32`, `:82-103` |
| Response | `{device_code, user_code, verification_uri, verification_uri_complete, expires_in, interval}` | `src/remote-device/device-authenticator.ts:13-20` |
| User step | prints URL + code; auto-opens `verification_uri_complete` with the `open` package | `src/remote-device/device-authenticator.ts:134-153` |
| Polling | `POST {base}/device/poll` `{device_code, client_id, code_verifier}` every `interval` (default 5 s) for `expires_in/interval` attempts; handles `authorization_pending`, `slow_down` (extra wait), 5xx/non-JSON as transient; any other error is terminal | `src/remote-device/device-authenticator.ts:155-232` |
| Success payload | `{access_token, refresh_token, device_id}` — the **server assigns the device id** and (INFERRED) creates the `mcp_devices` row, because `registerDevice` only finds/updates and throws "Device not found" otherwise; `createDevice()` exists but has no caller | `src/remote-device/device-authenticator.ts:199-206`; `src/remote-device/remote-channel.ts:549-564`, `:566-625` |
| Token type | **INFERRED: Supabase GoTrue session tokens.** Basis: they are passed to `supabase.auth.setSession()` and refreshed with `auth.refreshSession()`; comments name GoTrue and its refresh-token reuse detection | `src/remote-device/remote-channel.ts:337-372`, `:1421-1435`; `src/remote-device/device.ts:109-113` |
| No local callback server | stated in README; consistent with code (no listener) | `src/remote-device/README.md:147` |

### 3.4 Token storage, persistence and refresh

| Item | Detail | Evidence |
|---|---|---|
| Path | `~/.desktop-commander-device/device.json` | `src/remote-device/device.ts:40-42` |
| Format | `{"deviceId": "...", "session": {"access_token": "...", "refresh_token": "..."}}` (pretty-printed JSON; `session:null` when not persisting) | `src/remote-device/device.ts:466-473`, `:500` |
| Protection | atomic write with mode `0600`; cross-process `proper-lockfile` lock (stale 30 s); no Windows ACL (README) | `src/remote-device/device.ts:44-59`, `:496-503`; `src/remote-device/README.md:44` |
| `--persist-session` | **default ON**; `--persist-session` is an accepted no-op; `--no-persist-session` disables reuse and writes `session:null` | `src/remote-device/device.ts:109-113`, `:388-403`, `:769-785`; `src/npm-scripts/remote.ts:72-77` |
| Refresh | own fixed **45-minute** `auth.refreshSession()` timer; every `TOKEN_REFRESHED` re-authorizes the realtime socket and re-persists the rotated refresh token; writes are serialized | `src/remote-device/remote-channel.ts:99-101`, `:373-395`, `:1421-1442`; `src/remote-device/device.ts:85-91`, `:115-119`, `:444-447` |
| Sign-out handling | on `SIGNED_OUT`: one restore attempt (`setSession` + forced `refreshSession`); failing that, stop heartbeat/rollouts, remove channel, disconnect realtime, mark offline, tell user to restart | `src/remote-device/remote-channel.ts:400-491` |
| Logout | `remote --logout` deletes `device.json` under the lock; a running device that sees its file removed/replaced stops saving | `src/remote-device/device.ts:61-75`, `:480-495`; `src/npm-scripts/remote.ts:52-63` |
| Clock skew | `Date` header of every response; if the absolute skew exceeds 5 min, `Date.now` is patched process-wide | `src/remote-device/remote-channel.ts:101-103`, `:164-206`; `src/remote-device/device.ts:518-522` |

### 3.5 Supabase usage

**Tables** (all access via the user's JWT through PostgREST; RLS is **INFERRED** — the code filters `user_id` on `findDevice` but relies on the server for every other ownership check):

| Table | Columns touched by the device | Operations | Evidence |
|---|---|---|---|
| `mcp_devices` | `id, user_id, device_name, capabilities (jsonb), status ('online'/'offline'), last_seen` (written as the string `'now'`, resolved by Postgres) | `select id,device_name where id & user_id`; `update {status:'offline', last_seen:'now', capabilities:{app_version}, device_name}` at register; `update {status, last_seen:'now'}` for heartbeat/status (10 s abort) | `src/remote-device/remote-channel.ts:57-63`, `:515-547`, `:578-599`, `:740-751`, `:1463-1493` |
| `mcp_remote_calls` | `id, device_id, status ('pending'→'executing'→'completed'/'failed'), timeout_at, created_at, tool_name, tool_args, metadata (jsonb, incl. `rollouts`), result (jsonb), error_message (text)` | conditional claim `update {status:'executing'} where id, device_id, status='pending', timeout_at > 'now' returning *`; recovery `select id,device_id where device_id, status='pending', timeout_at > 'now' order created_at limit 100`; result `update {status, result, error_message} where id` | `src/remote-device/remote-channel.ts:860-895`, `:917-1004`, `:1244-1321`; row fields read `src/remote-device/device.ts:600-603` |

Note: `registerDevice()` receives the local tool list but writes only `{app_version}` into `capabilities` (`src/remote-device/remote-channel.ts:566`, `:587`, `:740-746`; tool list from `src/remote-device/device.ts:290-295`). **INFERRED:** the hosted service supplies its own tool catalogue to AI clients; the device never uploads its tool schemas.

**Realtime**

| Item | Detail | Evidence |
|---|---|---|
| Channel | **`user:<auth user id>`**, `config.private: true`, presence enabled with `key = deviceId` | `src/remote-device/remote-channel.ts:763-774` |
| Event | broadcast **`new_call`** ("doorbell") carrying ids only: `call_id`, `device_id` (optionally `user_id`) | `src/remote-device/remote-channel.ts:775-787`, `:912-923`; `src/remote-device/transport-telemetry.ts:17-23` |
| Presence payload | `{device_id, device_name, app_version, platform}`; best-effort, dashboard only; retry cooldown 5 min doubling to 60 min (+≤50% jitter) | `src/remote-device/remote-channel.ts:681-732`, `:74-77`, `:153-156` |
| Private-channel authorization | **INFERRED** Supabase Realtime Authorization (RLS on `realtime.messages`) — basis: `private:true` channel, JWT `accessToken` override, and comment about refused private joins / "008 misconfiguration" | `src/remote-device/remote-channel.ts:299-306`, `:770`, `:830-832` |
| Readiness | the row is set `online` only when the channel is `joined`, the status write succeeded, **and** the local MCP child is alive; otherwise `offline` | `src/remote-device/remote-channel.ts:627-679`, `:1323-1350` |
| Diagnostics probe | `ws(s)://<supabase>/realtime/v1/websocket?apikey=…&vsn=1.0.0`, Phoenix `heartbeat` | `src/remote-device/diagnostics/report.ts:302-345` |

### 3.6 Heartbeat, health and reconnect constants

| Constant | Value | Evidence |
|---|---|---|
| `HEARTBEAT_INTERVAL_MS` (DB `last_seen`/`status` write) | **5 min**; "must stay under the server's 15 min offline sweep and the dashboard's ~7 min freshness window" | `src/remote-device/remote-channel.ts:65-67`, `:1404-1419` |
| Connection health check | every **10 s** | `src/remote-device/remote-channel.ts:1408-1410` |
| `REALTIME_RECONNECT_BASE_MS` | `[1000, 2000, 5000, 10000, 20000, 40000, 80000, 120000]`, then 120000; jitter ×(0.5–1.5) for attempts 1–4, full jitter (≥1 s) after | `src/remote-device/remote-channel.ts:68-72`, `:122-134` |
| `WATCHDOG_RECREATE_MAX_BACKOFF_MS` | 120000 (watchdog delay `1000·2^min(n,7)` capped, jittered) | `src/remote-device/remote-channel.ts:73`, `:136-141` |
| `PRESENCE_RETRY_BASE_MS` / `_MAX_MS` | 5 min / 60 min | `src/remote-device/remote-channel.ts:76-77` |
| `READINESS_RETRY_BASE_MS` / `_MAX_MS` | 5 s / 60 s | `src/remote-device/remote-channel.ts:79-80` |
| `RECREATE_TIMEOUT_MS` | 45 s | `src/remote-device/remote-channel.ts:84` |
| `MAX_UNREADY_JOIN_BUDGET` | 16 | `src/remote-device/remote-channel.ts:87` |
| `STABLE_BROADCAST_RESET_MS` | 120 s | `src/remote-device/remote-channel.ts:92` |
| `JOINING_WEDGE_TIMEOUT_MS` | 30 s (stuck in `joining` → recreate) | `src/remote-device/remote-channel.ts:95`, `:1085-1095` |
| `HEARTBEAT_STALE_TIMEOUT_MS` | 75 s ("~3x the 25s heartbeat" — realtime-js socket heartbeat, INFERRED from comment) | `src/remote-device/remote-channel.ts:96-98`, `:1045-1055` |
| `TOKEN_REFRESH_INTERVAL_MS` | 45 min | `src/remote-device/remote-channel.ts:101` |
| `CLOCK_SKEW_CORRECTION_THRESHOLD_MS` | 5 min | `src/remote-device/remote-channel.ts:103` |
| `DEVICE_ROW_WRITE_TIMEOUT_MS` | 10 s | `src/remote-device/remote-channel.ts:108` |
| `OFFLINE_SESSION_TIMEOUT_MS` | 500 ms | `src/remote-device/remote-channel.ts:110` |
| `SOCKET_SETTLE_MAX_MS` / `_POLL_MS` | 300 / 20 ms | `src/remote-device/remote-channel.ts:115-116` |
| `PENDING_RECOVERY_BATCH_SIZE` | 100 | `src/remote-device/remote-channel.ts:120` |
| Doorbell claim retries | 0, 500, 1500 ms | `src/remote-device/remote-channel.ts:938-956` |
| `SEEN_CALL_IDS_MAX` (in-memory dedupe) | 100 ids | `src/remote-device/device.ts:23-30`, `:590-598` |
| Local child restart backoff | `250·2^min(n,5)·(0.5–1.5)`, capped 5 s; recovery floor 1 s | `src/remote-device/desktop-commander-integration.ts:16-29`; `src/remote-device/device.ts:33-38` |
| Rollouts | `GET {base}/device/rollouts` with `Bearer <JWT>`, 3 s timeout, every 30 min; each call row's `metadata.rollouts` overrides | `src/remote-device/rollouts.ts:1-111` |
| Graceful shutdown budget | force-exit after 5 s; offline subprocess 3 s | `src/remote-device/device.ts:144-148`; `src/remote-device/remote-channel.ts:1551-1564`, `:1605-1609` |
| Realtime health telemetry window | 60–90 s, sampled | `src/remote-device/realtime-health-telemetry.ts:86-94` |

### 3.7 Life of a tool call (cloud → device → cloud)

1. **(Server, INFERRED)** The hosted service inserts a `mcp_remote_calls` row (`status='pending'`, `device_id`, `tool_name`, `tool_args`, `metadata`, `timeout_at`) and broadcasts `new_call {call_id, device_id}` on `user:<uid>`. Basis: the device's claim filters and the recovery query (`src/remote-device/remote-channel.ts:860-870`, `:938-947`); README: "temporarily stores tool arguments and results in `mcp_remote_calls`" (`src/remote-device/README.md:220`).
2. **Doorbell** → `onDoorbell`: ignores other devices; atomically claims with `UPDATE … SET status='executing' WHERE id AND device_id AND status='pending' AND timeout_at > 'now' RETURNING *` (Postgres time, so device clock skew does not matter); retries on error; on ambiguous error reads the row back and only dispatches if still `pending` (`src/remote-device/remote-channel.ts:912-1004`).
3. **Missed doorbells**: after each successful (re)join, up to 100 pending, unexpired rows for this device are claimed oldest-first (`src/remote-device/remote-channel.ts:672-679`, `:852-895`).
4. **Device handler** (`src/remote-device/device.ts:600-694`): in-memory duplicate check, DB claim if not already claimed (`markCallExecuting`, which **fails open** on a DB error, `src/remote-device/remote-channel.ts:1235-1267`); special-cases `ping` (returns `pong <ISO time>`) and **`shutdown` (exits the device process after 1 s)** (`src/remote-device/device.ts:647-669`); otherwise proxies to the local child.
5. **Local execution**: `client.callTool({name, arguments, _meta:{remote:true, ...row.metadata}})` over stdio (`src/remote-device/desktop-commander-integration.ts:294-316`). The local server marks the call remote and attributes it to `_meta.clientInfo` (e.g. "openai-mcp", "claude-ai") (`src/server.ts:1214-1243`). The same `allowedDirectories`/`blockedCommands` apply as for local use; DC's SDK client default request timeout is 60 s (comment, `src/config.ts:15-23`).
6. **Result**: `UPDATE mcp_remote_calls SET status='completed', result=<MCP CallToolResult with NUL bytes stripped> WHERE id`; on write failure, a second write sets `status='failed', error_message='Result could not be stored (…)'`; tool errors write `failed` + message. `completed_at` is stamped by a DB trigger (server side). Comments cite results up to **13 MB** and a **5-minute** call timeout otherwise (`src/remote-device/remote-channel.ts:1269-1321`; `src/remote-device/device.ts:680-707`).
7. **(Server, INFERRED)** The hosted service observes the terminal row (mechanism not in repo; the branch name `origin/chore/remove-result-doorbell` suggests a result broadcast existed) and answers the AI client. README states terminal rows are swept ≥1 min after completion with a 1-hour creation backstop (`src/remote-device/README.md:220`).

Device console output prints full tool arguments and full JSON results for every call (`src/remote-device/device.ts:618`, `:680`); the persistent device log keeps tool name and outcome only (`src/remote-device/diagnostics/device-log.ts:9-22`, `:50`).

### 3.8 Shutdown and offline marking

`SIGINT/SIGTERM` → stop heartbeat/rollouts → untrack presence + unsubscribe (≤300 ms each) → **`spawnSync(process.execPath, [blocking-offline-update.js, deviceId, supabaseUrl, supabaseKey, access_token, refresh_token])`** with a 3 s timeout, which `UPDATE mcp_devices SET status='offline', last_seen='now'` using the bearer token (refreshing it if expired) (`src/remote-device/device.ts:133-181`, `:709-755`; `src/remote-device/remote-channel.ts:1495-1629`; `src/remote-device/scripts/blocking-offline-update.js:1-135`). **INFERRED risk:** the access and refresh tokens are passed as command-line arguments, which are visible in process listings on typical multi-user systems for the ~3 s the helper runs.

### 3.9 Diagnostics (`remote --report`)

Builds a zip (versions, Node install kind, clock skew, timings for server `/api/mcp-info` ×5, Supabase REST `/rest/v1/` with `apikey`, Realtime websocket heartbeat, proxy env presence, device id and yes/no facts about `device.json`, `telemetryEnabled` and `clientId` from config, re-masked device log) and POSTs it to `diagnosticsUrl` unless `--no-upload` (`src/remote-device/diagnostics/report.ts:18-31`, `:250-345`, `:560-613`; `src/remote-device/diagnostics/upload.ts:1-65`). Device log: `~/.desktop-commander-device/remote-<day>.log`, 1 MB × 3 files × 7 days (`src/remote-device/diagnostics/device-log.ts:9-38`).

### 3.10 Remote server URLs referenced

| URL / path | Evidence |
|---|---|
| `https://mcp.desktopcommander.app` (default `MCP_SERVER_URL`) | `src/remote-device/device.ts:104`; `src/remote-device/diagnostics/report.ts:32`; `src/remote-device/README.md:48` |
| `/api/mcp-info` (public, unauthenticated) | `src/remote-device/device.ts:514-517` |
| `/device/start`, `/device/poll` | `src/remote-device/device-authenticator.ts:91`, `:175` |
| `/device/rollouts` (Bearer JWT) | `src/remote-device/rollouts.ts:91-93` |
| Supabase `<supabaseUrl>/auth/v1/*` (via supabase-js), `/rest/v1/*`, `/realtime/v1/websocket` | `src/remote-device/remote-channel.ts:296`; `src/remote-device/diagnostics/report.ts:281`, `:305` |
| `diagnosticsUrl` (https, or http on loopback only) | `src/remote-device/diagnostics/report.ts:546-558` |
| Dashboard (device revocation) — referenced, URL not given beyond mcp.desktopcommander.app | `src/remote-device/README.md:80-97` |

### 3.11 What is NOT in the repo (cloud side)

Not present at `c2b8795` (searched `src/`, `scripts/`, `test/` listings and grep): the Remote MCP HTTP server that ChatGPT/Claude connect to and its OAuth for AI clients; the implementations of `/api/mcp-info`, `/device/start`, `/device/poll`, `/device/rollouts`; device verification web page and dashboard; Supabase schema/migrations, RLS and Realtime policies, the `completed_at` trigger, the ≥15-min offline sweep and the row sweeper (all referenced only in comments: `src/remote-device/remote-channel.ts:65-66`, `:1271-1272`; README `:220`); the diagnostics Worker; the telemetry proxy (`github.com/desktop-commander/telemetry-proxy`, `vendor/telemetry-contract/package.json:5-8`); rollout assignment; billing/plan enforcement (e.g. the 10,000-calls/month Free cap in §6). Which Supabase project/region is used is not knowable from source (delivered at runtime by `/api/mcp-info`).

### 3.12 In-flight work on `origin/MQTT` (not on main)

MQTT branch @ `92e81b3f905004e2f616a02abf6a9d1405b3942b` adds a second doorbell transport in "shadow" mode: broker default **`mqtts://a1vw7p0stdnykd-ats.iot.eu-west-1.amazonaws.com:8883` (AWS IoT Core, eu-west-1)**, mutual-TLS client certificate enrolled via `POST /device/mqtt/enroll` and revoked via `/device/mqtt/revoke`, credentials in `~/.desktop-commander-device/.mqtt`, result topic `dc/mqtt/v1/users/{userId}/devices/{deviceId}/result-{origin_instance}`, device capability `transport_mqtt_v1`, server flags `MQTT_TRANSPORT_ENABLED` / `IS_MQTT_EXECUTION_ENABLED`; Broadcast stays authoritative (MQTT branch `docs/mqtt-doorbell.md:1-83`; `src/remote-device/mqtt/config.ts:1-3`). New deps on that branch: `mqtt 5.16.0`, `node-forge 1.4.0` (runtime), `aedes 0.51.3` (dev) (MQTT branch `package.json:115-116`, `:143`).

---

## 4. Distribution and updates

### 4.1 Package and channels

| Channel | Detail | Evidence |
|---|---|---|
| npm | **`@wonderwhy-er/desktop-commander`**, public; ships `dist`, `logo.png`, `testemonials`, `vendor/telemetry-contract` (bundled dependency) | `package.json:2`, `:23-28`, `:70-72`, `:128-130` |
| npm lifecycle | `postinstall`: install telemetry (`track-installation.js`) then `verify-ripgrep` (never fails the install) | `package.json:30` |
| Official MCP Registry | `io.github.wonderwhy-er/desktop-commander`, npm package, stdio; published with `mcp-publisher login github-oidc` + `publish` | `server.json:1-22`; `package.json:5`; `.github/workflows/release.yml:487-515` |
| MCPB / DXT bundle | `manifest.template.json` (manifest_version 0.3, `server.type: node`, entry `dist/index.js`, env `MCP_DXT=true`, `NODE_ENV=production`, platforms darwin/win32/linux, node ≥22.12.0); built by `scripts/build-mcpb.cjs` (pre-downloads all-platform ripgrep, builds, renders manifest) and attached as `desktop-commander-<ver>.mcpb` to the GitHub release | `manifest.template.json:1-36`, `:155-165`; `scripts/build-mcpb.cjs:9-79`; `.github/workflows/release.yml:14`, `:293-308`, `:460-482` |
| Claude Desktop setup | `npx @wonderwhy-er/desktop-commander@latest setup` writes `mcpServers["desktop-commander"]` into `claude_desktop_config.json` (Win `%APPDATA%\Claude`, macOS `~/Library/Application Support/Claude`, Linux `~/.config/Claude`); command `npx -y <spec>` (Windows: `cmd /c npx -y <spec>`), `<spec>` = the spec it was launched with, else `@latest`; then kills/restarts Claude | `setup-claude-server.js:208-234`, `:490-507`, `:816-840`, `:561-643`, `:863-870` |
| macOS one-liner | `install.sh`: if Node missing, downloads `node-v22.14.0.pkg` and `sudo installer`; then `npx …@latest setup` | `install.sh:16-46`; README `README.md:122` |
| Docker | `Dockerfile` (Smithery-generated): `node:lts-alpine`, `MCP_CLIENT_DOCKER=true`, `npm install --ignore-scripts`, `npm rebuild @vscode/ripgrep`, `node dist/index.js` | `Dockerfile:1-26` |
| Docker installers | `install-docker.sh` / `.ps1`: pull `mcp/desktop-commander:latest`, persistent volumes `dc-system:/usr`, `dc-home:/root`, `dc-workspace:/workspace`, `dc-packages:/var`, user folders bind-mounted under `/home/...`, `docker run -i --rm`, written into Claude Desktop config | `install-docker.sh:15`, `:71-74`, `:215-267`; `install-docker.ps1:70-72`, `:164-170`, `:205-230` |
| Docker MCP catalog style `server.yaml` | image `mcp/desktop-commander`, env `ALLOWED_DIRECTORIES`, `DISABLE_NETWORK`, `NETWORK_TIMEOUT` — **none of these env vars is read anywhere in `src/`** (grep) | `server.yaml:1-31` |
| Smithery | `smithery.yaml` stdio, `node dist/index.js`, no config | `smithery.yaml:1-13`; runtime detection of Smithery for telemetry `src/utils/capture.ts:492-502` |
| Claude Code / Cowork plugin | `.claude-plugin/marketplace.json` → `./plugins/claude` (skills: ai-tools-setup, computer-health-check, desktop-commander-overview, knowledge-base, obsidian-vault, terminal); root `plugin.json` v1.5.0 + `.mcp.json` (`npx -y …@latest`) | `.claude-plugin/marketplace.json:1-20`; `plugin.json:1-26`; `.mcp.json:1-8` |
| Cursor plugin | `.cursor-plugin/marketplace.json` → `plugins/cursor` (+ rule `desktop-commander-default.mdc`) | `.cursor-plugin/marketplace.json:1-19` |
| Gemini CLI extension | `gemini-extension.json` v1.5.0, `npx -y @wonderwhy-er/desktop-commander` (unpinned, no `@latest`) | `gemini-extension.json:1-12` |
| agent plugin listing | `plugin.yaml` (agenium) | `plugin.yaml:1-60` |
| Remote launchers | `curl …/scripts/remote/remote-{macos,linux}.sh \| sh`, `irm …/remote-windows.ps1 \| iex`; if Node absent and user answers Yes: download latest Node 24 from nodejs.org, verify SHA-256, install (.pkg / .msi / tarball to /usr/local); then `npx --yes @wonderwhy-er/desktop-commander@latest remote` | `scripts/remote/README.md:3-49`; `scripts/remote/remote-linux.sh:34-35`, `:103`, `:123`, `:196-208`; `scripts/remote/remote-windows.ps1:45-49`, `:111`, `:131` |

### 4.2 Windows / macOS specifics

* Windows: `cmd /c npx` wrapper in Claude config (`setup-claude-server.js:820-830`); `taskkill` from `%SystemRoot%\System32` (`src/utils/process-tree.ts:191-208`); `tasklist /FO CSV` (`src/tools/process.ts:54-56`); `cmd /c start "" <url>` for browser (`src/utils/open-browser.ts:28-33`); cmd verbatim-args quoting fix (`src/terminal-manager.ts:182-191`); program lookup avoids the working folder (`src/utils/ripgrep-resolver.ts:36-42`; `src/remote-device/desktop-commander-integration.ts:278-287`); no owner-only ACL for `device.json` (`src/remote-device/README.md:44`).
* macOS: `caffeinate` keeps the machine awake while `remote` runs (`src/npm-scripts/remote.ts:97-108`); Full Disk Access guidance in permission errors (`src/tools/filesystem.ts:96-103`); `open` for browser (`src/utils/open-browser.ts:25-26`).

### 4.3 Release mechanics

* Tags `v*` / `test-v*` or manual dispatch trigger `Release`: build MCPB → `npm publish` (OIDC trusted publishing, "No secrets required") → GitHub release + `.mcpb` → MCP Registry (`.github/workflows/release.yml:14`, `:54`, `:59-64`, `:99-100`, `:413-434`, `:460-515`).
* Alpha releases publish from a developer machine under npm dist-tag `alpha`, with no git tag/CI (`scripts/publish-release.cjs:18-19`, `:106-116`).

### 4.4 Auto-update behaviour

There is **no self-update or update-check code** in `src/` (see §2.1). Every first-party install path launches through `npx` with `@latest` (Claude Desktop setup default `setup-claude-server.js:232-233`; `.mcp.json:5`; remote launchers `scripts/remote/remote-linux.sh:206-208`; remote README `src/remote-device/README.md:18`). **INFERRED:** users therefore move to the newest npm release whenever the client restarts the server and npx re-resolves `@latest` (rests on standard npx semantics, not on repo code). MCPB, Docker (`:latest` pulled only at install time, `install-docker.sh:121`) and global installs update only when the user reinstalls.

### 4.5 Release cadence, last 6 months (2026-05-01 → 2026-11-01)

From `git for-each-ref --sort=-creatordate refs/tags` at `c2b8795`:

| Tag | Date (creatordate) |
|---|---|
| v0.3.1 | 2026-10-22 |
| v0.3.0 | 2026-10-13 |
| 0.3.1-alpha.0 | 2026-10-06 |
| v0.2.52 | 2026-09-29 |
| v0.2.51 | 2026-09-17 |
| v0.2.50 | 2026-09-09 |
| v0.2.49 | 2026-09-09 |
| v0.2.48 | 2026-09-02 |
| v0.2.47 | 2026-08-04 |
| v0.2.46 | 2026-07-14 |
| v0.2.45 | 2026-07-10 |
| v0.2.44 | 2026-07-09 |
| v0.2.43 | 2026-06-26 |
| v0.2.42 | 2026-06-05 |
| v0.2.41 | 2026-05-14 |

15 tags (14 stable + 1 alpha) in six months ≈ one release every 12 days, clustered (3 in 5 days in July, 2 on 2026-09-09). 445 commits reachable from HEAD since 2026-05-01 (`git rev-list --count --since=2026-05-01 HEAD`). Note: npm-only alpha releases do not create git tags (§4.3), so npm may list more versions than this table.

---

## 5. Runtime dependencies and license

**License:** MIT, "Copyright (c) 2024-2025 Eduard Ruzga and Desktop Commander Contributors" (`LICENSE:1-3`; `package.json:6`). Author "Eduards Ruzga" (`package.json:7`).

Declared range from `package.json` (line numbers there), resolved version and license from `package-lock.json` (lockfileVersion 3; line = the `"node_modules/<name>"` entry).

| Package | Declared (package.json line) | Locked (package-lock.json line) | License (lock) | Used for |
|---|---|---|---|---|
| `@desktop-commander/telemetry-contract` | `file:vendor/telemetry-contract` (93), bundled (128-130) | 0.1.1 (13383-13385; link entry 261-264) | none declared (`vendor/telemetry-contract/package.json` has no `license`) | transport-telemetry allow-list |
| `@modelcontextprotocol/sdk` | ^1.32.1 (94) | 1.32.1 (1678-1679) | MIT | MCP server + device's stdio client |
| `@opendocsg/pdf2md` | 0.3.4 (95) | 0.3.4 (1756-1757) | MIT | PDF → markdown |
| `@supabase/supabase-js` | ^2.89.0 (96) | 2.90.0 (2161-2162) | MIT | remote device: auth, PostgREST, Realtime (realtime-js 2.90.0 at 2133-2134, auth-js 2.90.0 at 2097, postgrest-js 2.90.0 at 2121) |
| `@tiptap/core` | ^3.31.4 (97) | 3.31.4 (2190-2191) | MIT | markdown editor widget |
| `@tiptap/extension-image` / `-table` / `-table-cell` / `-table-header` / `-table-row` | ^3.31.4 (98-102) | 3.31.4 (2349, 2471, 2485, 2498, 2511) | MIT | editor widget |
| `@tiptap/pm`, `@tiptap/starter-kit` | ^3.31.4 (103-104) | 3.31.4 (2564, 2589) | MIT | editor widget |
| `@vscode/ripgrep` | ^1.15.9 (105) | 1.17.0 (2874-2875) | MIT | search binary |
| `cross-fetch` | ^4.1.0 (106) | 4.1.0 (4471-4472) | MIT | `read_file` URL fetch |
| `exceljs` | ^4.4.0 (107) | 4.4.0 (5666-5667) | MIT | Excel read/write/search |
| `fastest-levenshtein` | ^1.0.16 (108) | 1.0.16 (5915-5916) | MIT | edit_block fuzzy match |
| `glob` | ^10.3.10 (109) | 10.5.0 (6452-6453) | ISC | no direct import in `src/`, `scripts/` or root JS at this commit (grep) |
| `highlight.js` | ^11.11.1 (110) | 11.11.1 (6714-6715) | BSD-3-Clause | widget highlighting (`src/ui/file-preview/src/components/highlighting.ts:5-8`); PDF code styles (`src/tools/pdf/markdown.ts:533`) |
| `isbinaryfile` | ^5.0.4 (111) | 5.0.7 (7223-7224) | MIT | binary detection |
| `markdown-it` | ^14.1.0 (112) | 14.3.2 (8129-8130) | MIT | widget markdown parser (`src/ui/file-preview/src/markdown/parser.ts:2`) |
| `md-to-pdf` | ^5.2.5 (113) | 5.2.5 (8193-8194) | MIT | markdown → PDF; pulls **`puppeteer` 25.12.0** (10421-10422, Apache-2.0) and `@puppeteer/browsers` 3.2.3 (1952-1953, Apache-2.0) |
| `open` | ^10.2.0 (114) | 10.2.0 (9620-9621) | MIT | opens device-auth URL |
| `pdf-lib` | ^1.17.1 (115) | 1.17.1 (10014-10015) | MIT | PDF page edits |
| `pizzip` | ^3.2.0 (116) | 3.2.0 (10091-10092) | **MIT OR GPL-3.0** | DOCX zip |
| `proper-lockfile` | ^4.1.2 (117) | 4.1.2 (10167-10168) | MIT | config/device.json locks |
| `remark`, `remark-gfm`, `remark-parse` | ^15.0.1 / ^4.0.1 / ^11.0.0 (118-120) | 15.0.1 / 4.0.1 / 11.0.0 (10625, 10641, 10659) | MIT | no direct import in `src/`, `scripts/` or root JS at this commit (grep) |
| `sharp` | ^0.35.5 (121) | 0.35.5 (11324-11325) | **Apache-2.0** (native libvips binaries) | PDF image extraction |
| `tiptap-markdown` | ^0.9.0 (122) | 0.9.0 (11997-11998) | MIT | editor widget |
| `unified` | ^11.0.5 (123) | 11.0.5 (12410-12411) | MIT | no direct import in `src/`, `scripts/` or root JS at this commit (grep) |
| `unpdf` | ~1.8.1 (124) | 1.8.1 (12506-12507) | MIT | PDF reading |
| `zod` | ^3.24.1 (125) | 3.25.76 (13355-13356) | MIT | tool arg schemas |
| `zod-to-json-schema` | ^3.23.5 (126) | 3.25.1 (13364-13365) | ISC | tool `inputSchema` |
| `caffeinate` (optional) | ^1.0.1 (132) | 1.0.1 (3857-3858) | MIT | macOS no-sleep for `remote` |

Overrides: `md-to-pdf → chokidar ^4.0.3`, `exceljs → uuid ^11.1.1`, `proxy-addr ^2.0.8`, `argparse ^2.0.1` (`package.json:151-160`). Dev-only: `@anthropic-ai/mcpb ^1.2.0`, `@modelcontextprotocol/ext-apps ^1.0.1`, `esbuild`, `nexe`, `tsx`, `typescript`, `ws`, `jsdom`, `js-tiktoken`, etc. (`package.json:134-150`). Not a dependency: `node-pty` (absent from `package.json:92-150`).

---

## 6. Architecture docs and public pricing

### 6.1 In-repo documents describing the cloud/remote product (main @ `c2b8795`)

There is no `docs/` directory on main and no design document for the hosted service. What exists:

| Document | What it says about the cloud side | Evidence |
|---|---|---|
| `src/remote-device/README.md` | Request path `AI client → Desktop Commander Remote MCP → Remote Device → local Desktop Commander MCP server`; OAuth device flow; `device.json` path/mode; persistence default on; logout vs dashboard revocation; `mcp_remote_calls` temporary storage, terminal rows swept ≥1 min after completion with a 1-hour creation-time backstop | `src/remote-device/README.md:1-66`, `:68-97`, `:214-220` |
| `README.md` | "Remote AI Control … via Remote MCP" (mcp.desktopcommander.app); remote setup; local history files; remote calls stored temporarily server-side; telemetry statement | `README.md:42`, `README.md:459-474`, `README.md:787-820`, `README.md:1139-1145` |
| `PRIVACY.md` | telemetry policy (opt-out, GA4, 14-month retention) — predates/contradicts the proxy in code (§2.3) | `PRIVACY.md:13`, `:76`, `:99`, `:113`, `:121-132` |
| `vendor/telemetry-contract/README.md` | shared contract between "telemetry-proxy and Remote MCP"; `/remote/collect` (server, authenticated, `user_id`) vs `/mp/collect` (device, `client_id`, `device_id`) | `vendor/telemetry-contract/README.md:1-15` |
| Code comments (de facto design notes) | 15-min server offline sweep, ~7-min dashboard freshness, DB trigger stamps `completed_at`, 5-min call timeout, results up to 13 MB, ~126k doorbells/day in prod, "No devices available" answer for offline devices | `src/remote-device/remote-channel.ts:65-66`, `:925-926`, `:1271-1288`; `src/remote-device/device.ts:553-557` |
| `CUSTOM_STDIO_EXPLANATION.md`, `CLINE_NOTIFICATION_PROBLEM.md` | local stdio transport only, not cloud | `CUSTOM_STDIO_EXPLANATION.md:1-20` |
| MQTT branch only | `docs/mqtt-doorbell.md` (AWS IoT Core MQTT shadow transport, see §3.12) | MQTT branch @ `92e81b3` `docs/mqtt-doorbell.md:1-83` |

### 6.2 Pricing / plans

* **The README does not link a pricing or plans page.** Its only price mentions are Claude Pro's $20/month (`README.md:1121`; `FAQ.md:98-111`). It links `https://mcp.desktopcommander.app` (`README.md:42`, `:470`) and `https://desktopcommander.app/` (`README.md:1055`).
* Fetch attempts for `https://mcp.desktopcommander.app`, `https://desktopcommander.app/` and `https://desktopcommander.app/flags/v2/production.json` were **blocked by this session's web permission prompt (not answered)**; their contents are not reported.
* Found via web search instead: `https://desktopcommander.app/mcp/` — page states Local MCP "Runs directly on your machine. Free & open source."; Remote MCP lets you "Connect one or more of your devices remotely", managed at `https://mcp.desktopcommander.app/`; it links a Pricing page. (WebFetch, 2026-11-01.)
* `https://desktopcommander.app/pricing/` (WebFetch, 2026-11-01; the tool returns a model-generated summary, not raw HTML):
  * **Free — $0/month:** "10,000 tool calls per month", connecting devices, community support.
  * **Pro — $20/month:** "Unlimited tool calls", connecting devices, priority support.
  * "The local MCP server is free and open source, with no account and no monthly limit."
  * Both plans list "ChatGPT, Claude and any MCP client"; no annual price; the FAQ entry "How many devices can I connect?" had no answer text in the fetched page.
  * **INFERRED:** the call cap applies to Remote MCP calls metered server-side; nothing in this repo counts or enforces it (§3.11).
* Context only (search-result title, not verified): tech.eu, "Desktop Commander lands €1.1M pre-seed for AI desktop automation", `https://tech.eu/2025/10/28/desktop-commander-lands-eur11m-pre-seed-for-ai-desktop-automation/`.

---

## 7. Doc-vs-code discrepancies worth knowing

| Claim | Code at `c2b8795` |
|---|---|
| Telemetry goes to Google Analytics 4 (`PRIVACY.md:99`) | sends to a first-party proxy; GA path marked removed; endpoint unauthenticated per TODO (`src/utils/capture.ts:55-62`, `:244-247`) |
| "We do not collect … command arguments" (`README.md:1141`) | first word of every chained command is sent (`src/tools/improved-process-tools.ts:155-162`); remote transport events carry raw `device_id` and `call_id` (`src/remote-device/transport-telemetry.ts:30-47`) |
| Opt-out stops all telemetry (`PRIVACY.md:132`) | holds for the server, but `postinstall` and `setup` send regardless (`track-installation.js:271-307`; `setup-claude-server.js:301-370`); feature-flag fetch is unconditional (`src/utils/feature-flags.ts:166-220`) |
| Remote device needs "Node.js 18 or newer" (`src/remote-device/README.md:9`) | engines `>=22.12.0` (`package.json:15-17`) |
| Feature flags "refresh every 5 minutes" (comment `src/utils/feature-flags.ts:62`) | interval is `cacheMaxAge` = 30 min (`src/utils/feature-flags.ts:17`, `:63-67`) |
| `get_config` exposes `clientHistory` (`src/server.ts:325`) | no producer in `src/` |
| Docker catalog env `ALLOWED_DIRECTORIES` / `DISABLE_NETWORK` (`server.yaml:25-31`) | not read anywhere in `src/` |

