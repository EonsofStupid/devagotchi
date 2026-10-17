# Devagotchi performance and workstation experience specification

Planning revision: **2026-10-17 UTC**. Owner: the existing core, native-platform, and inspector maintainers, coordinated by the benchmark owner appointed for G0.

**All workloads and numeric budgets in this document are proposed acceptance criteria. None is a measured result or a current product guarantee.** The master [architecture and growth plan](product-and-platform-plan.md) owns scope, decisions, and phase ordering.

## 1. Evidence rules

The historical Devagotchi checks establish bounded Linux correctness scenarios and a real Linux native inspector path. They do not show lower latency, lower CPU, lower memory, better battery life, sustained operation, or native Windows/macOS behavior. The Desktop Commander audit is a pinned partial source inspection plus targeted harnesses, not a full application benchmark.

Every new result must distinguish:

- **Measured:** observed on a named build, host, OS, filesystem, and workload.
- **Derived:** a consequence of inspected code or configuration, with its calculation.
- **Proposed:** a target or mechanism awaiting evidence.
- **Unsupported/not run/blocked:** a workload with no successful result; never a zero value.

One useful derived observation is the foundation's output path: at most 1024 raw bytes enter a frame and each positive pipe read calls one journal append transaction. At least 1024 append transactions are therefore required per MiB of raw output; smaller reads can require more. That count is not a throughput measurement. See [drain_pipe and Shared::output](../src/execution.rs) and [Journal::append](../src/journal.rs).

## 2. Baselines and comparison design

### Three inputs

1. **D0:** recovered Devagotchi foundation, archive SHA-256 075a259e66f50168c84989d8cedb81c1debadd08e950bf2f3abb468c4e2b7987. Rebuild in release mode for performance; retain the historical debug/native evidence as a separate correctness record.
2. **D1:** candidate Devagotchi revision, with exact source and dependency hashes, release configuration, and relevant feature flags.
3. **DC:** full reproducible DesktopCommanderMCP checkout/build pinned for the comparison. Start from audit commit f508e524469c26f6366e17dbfd3ee5a69d112127 when available; if a current release is added, report it as a second baseline with its own pin. Do not extrapolate a full-server result from the saved partial snapshot.

Use the same physical machine or controlled VM for paired D0/D1/DC comparisons on one OS. Compare each OS against its own paired baseline; an ARM Mac and x64 Windows laptop are separate cohorts. Pin runtime and dependency versions. Record CPU, core allocation, RAM, storage, filesystem, power mode, architecture, display refresh rate, native webview/runtime, build optimization, and background-process conditions.

Start with a documented reference tier around four available CPU cores, 16 GiB RAM, and local SSD storage. Also run a constrained tier and one real representative developer workstation per OS before release. Those descriptions are testing proposals, not supported minimum hardware.

### Match semantics before timing

Match executable behavior, emitted bytes, retained output, durability, search corpus/ignore rules, decoding, reader policy, and requested result. A shell launch and direct executable launch may be useful distinct scenarios; report their setup and do not treat the shell's cost as an unexplained product difference.

If one product lacks a requested guarantee or feature, record a parity difference. If one returns only text and another returns raw bytes plus text, report payloads separately and add a matched text-only comparison where the contract supports it. Do not weaken candidate durability or discard output merely to improve its latency score.

Use two cost scopes: **backend** (broker/adapter or equivalent server process family) and **complete product** (backend plus inspector, native host, webview and owned helpers). Record child-workload resource use separately and also report total workstation impact. Never compare Devagotchi's broker alone to another product including all its UI/child processes.

### Repetition and uncertainty

Use alternating/randomized candidate-baseline order and paired runs. Separate cold startup/cache and warmed steady state. Keep preparation outside the timed interval, but report it.

For frequent latency metrics, collect at least 1000 successful observations per condition before reporting a p99; report sample count, errors, timeouts, p50/p95/p99, and uncertainty across independent runs. Run at least five independent process launches per steady-state condition and at least 20 cold starts for startup distributions; do not call a 20-sample startup maximum a trustworthy p99.

Predeclare attempt count, arrival rate or duration, and retry policy. Count every attempt in the outcome denominator and label successful-request percentiles as conditional on success. Under supported normal/mixed fixture loads, unexpected rejections and timeouts fail acceptance unless a different error budget was explicitly frozen beforehand; expected overload is evaluated separately in stress cases. Record intended arrival and actual send times so a slowed load generator or closed-loop workload cannot silently hide queueing or reduced throughput.

Retain outliers. Explain environmental interference and repeat only under a stated rule, preserving the excluded raw run and reason. When variability hides the difference, report inconclusive evidence. Acceptance never relies on a single fastest run.

## 3. Instrument existing owners

Instrumentation must not create a second operation model. Correlate through existing operation IDs plus a benchmark run ID. Add bounded counters/timing spans to the current owners and write private diagnostic artifacts. Record sampling and instrumentation overhead; compare an instrumented and normal build on a representative workload before treating timings as final.

| Owner | Timings/counters | Question answered |
|---|---|---|
| IPC/adapter | Endpoint validation, connect, serialization, decoded request/response bytes, inflight requests, permit rejection, wait cancellation | Is setup or transport material, and does saturation keep control usable? |
| Broker admission | Validation, registry wait/hold, durable admission, independent ownership handoff, scheduling delay | Where does request-to-admission time go? |
| Execution | Spawn, first read, bytes read by stream, deadline detection, stop request, root exit, resource settlement, drain completion | Is work fast and accurately supervised? |
| Journal | Transaction count/size, queue bytes/depth, wait time, commit latency, checkpoint time, loaded rows/bytes, DB/WAL/temp size | Is persistence stalling control or growing with history? |
| Output/query | Cursor range, retained/dropped bytes, scan work, encoding allocation/time, payload bytes | Are readers independent, cheap, and truthful about retention? |
| Inspector | Event-to-model, model-to-paint, input-to-feedback, selection-to-detail, render/frame cost, cache/DOM size | Does the native app remain usable under real output? |
| Native host/OS | Process-family RSS/private memory, CPU time, wakeups where available, handles/descriptors, lifecycle events | What does the installation cost during idle, bursts, and sustained use? |

Do not put full command arguments, environment values, file contents, or terminal secrets in routine metric labels. Exact diagnostic captures are opt-in fixtures with a clear retention path. Bound diagnostic queues and disk usage so measurement cannot create the growth problem being measured.

### Metric definitions

- **Admission latency:** complete request received by broker to durable admission response ready. Also report MCP/client round trip separately.
- **First-output latency:** fixture's declared emission event to first observable returned output. Separate command startup, broker capture, persistence, client wait strategy, and UI paint.
- **Output freshness:** emission/capture to visible text during a paced stream, not merely time to display already-buffered content.
- **Cancel acknowledgement:** explicit cancel request to the response recording cancellation intent. It is not proof that the process stopped.
- **Settlement latency:** cancel request to verified settlement of the declared owned resources, with output completeness reported separately.
- **Deadline overshoot:** configured expiry to actual stop request and then settlement, measured separately.
- **Ready time:** process start until broker accepts a real read/admission request; app readiness is separately the first usable real-data view.
- **Resource cost:** report baseline, peak, steady state, and post-work recovery. A logical byte cap is not an RSS or physical disk cap.

Use calibrated monotonic timestamps for cross-process intervals; document the calibration error. Do not subtract JavaScript and native clock values with unrelated origins. For suspend/resume cases, name whether the chosen clock includes suspension. Do not compare ambiguous timestamp domains.

## 4. Workload matrix

Run correctness and performance together: record expected effects, observed bytes, state transitions, and failed requests alongside timing.

| ID | Workload and controlled variables | Required measurements and assertions |
|---|---|---|
| W01 | Empty installation, then repeated no-op, stdout/stderr, failure, Unicode and large-argument fixture commands | Admission/start/first-output/settlement, exact child-observed arguments, warm/cold costs, no unintended shell interpretation |
| W02 | 1/4/8/32 active jobs; mix short commands, long-running roots, paced output, and long searches once available | Throughput, latency tails, active/queue limits, per-client/workspace fairness, cancellation and list responsiveness |
| W03 | 1 GiB aggregate stdout/stderr, both full-buffer and tiny chunks, plus invalid and split UTF-8 | Drain rate, commit count/size, retained byte correctness, explicit gaps, memory/disk plateau, control latency during flood |
| W04 | 1/2/8 independent readers; slow/frozen reader; repeated detach/reattach; old cursor | Equal retained ranges, reader isolation, response bytes/CPU, bounded backlog, explicit resnapshot/gap behavior |
| W05 | History at 0/64/1024/4096 records for D0, then 10,000/100,000 for capable candidates; low/high retained output; cold reopen | Admission availability, SQL/loaded bytes, ready time, RSS, indexed page latency. D0's unsupported tiers remain unsupported; no bypass of its cap for a flattering comparison. |
| W06 | 100,000-file generated corpus with deterministic content; million-file stress tier; shallow/deep trees; sparse/dense matches; ignored/binary/large files | First page and total search time, cancellation, memory, descriptor count, emitted result bytes, correct ignore and truncation semantics |
| W07 | Root alive versus root exited with inherited children still running; small and realistic large host process tables | Observation calls/scans, CPU/wakeups, root/group settlement, no early completion, no unrelated process signalled |
| W08 | At/beyond request/subscriber capacities, incomplete frames, slow request senders/readers, output flood plus cancellation | Explicit overload, bounded memory/permits, control capacity, recovery after overload, continued broker ownership |
| W09 | Slow commits, full storage, busy database, output queue saturation, interrupted admission/output/terminal commits | No false durable response; bounded queues; failure visible; no duplicate effects after recovery; policy-specific output loss/stop behavior |
| W10 | Native app switching A→B→A→B while outputs and cancel replies race; scroll/follow, filters, paging, reopening selected history | Correct selected ID/action, no stolen scroll/focus, usable keyboard input, native frame and selection latency, bounded cache/DOM |
| W11 | Window close, inspector quit, MCP exit, broker stop/crash, suspend/resume, logoff, reboot | Each event's declared job/deadline behavior; stale versus confirmed state; no reconnect-triggered execution; no PID-only recovery |
| W12 | Real PTY/ConPTY prompt, full-screen terminal fixture, Unicode, resize, Ctrl-C, EOF, writer transfer, detach | Terminal semantics, bounded input/output queues, one authorized writer, input/resize latency, no pretend stdout/stderr separation in combined terminal output |
| W13 | File proposal/commit with external edit, symlink/junction swap, missing destination, cross-volume attempt, disk-full and interrupted commit | Object identity, expected revision, conflict/uncertainty, metadata policy, verified result, no universal atomic-CAS claim |
| W14 | Clean-machine install, named MCP host connection, workspace grant, first command, reopen, update, interrupted upgrade, uninstall | Steps/time to first useful command, truthful connection state, config preservation, migration recovery, removal of owned startup resources only |
| W15 | Two-hour development soak and eight-hour release soak with repeated short work, capped output, readers and history eviction | Memory/storage/handle plateau at fixed working set, no progressive control slowdown, no lost admission/cursor evidence, no replay |

For current D0, W06/W12/W13/W14 contain missing product capabilities. Preserve that result. Do not simulate file or terminal support to fill the matrix. For stress tiers, report the boundary where the configured service declines work and whether it recovers; “accepted everything” is not the goal.

## 5. Proposed quantitative budgets

Freeze these against the selected reference machines before seeing D1 results. A justified adjustment remains visible in the report and decision register. These are user-experience and engineering budgets, not OS-enforced resource guarantees.

**Normal load:** up to four fixture jobs and two readers, local SSD, supported inputs. **Mixed load:** eight fixture jobs including one output producer paced at 5 MiB/s aggregate, eight observers, 100,000 historical metadata records on capable candidates, and active inspector navigation. A separate 32-job tier explores overload and scaling; it does not silently redefine supported defaults.

| Budget | Proposed target | Measurement scope |
|---|---|---|
| B01 Admission | p95 ≤100 ms; p99 ≤250 ms | Broker request to durable response, normal and mixed load; client round trip reported separately |
| B02 Get/list/workspace query | p95 ≤100 ms; p99 ≤250 ms | Bounded page under mixed load, including large history after G1 |
| B03 Visible live output | p95 ≤150 ms normal; ≤250 ms mixed | Calibrated fixture/capture to actual native paint; broker capture and UI portions also reported |
| B04 Cancel intent | p95 ≤100 ms; p99 ≤250 ms | Explicit request to acknowledged intent under mixed output/control load |
| B05 Owned fixture settlement | p95 ≤1 s; p99 ≤2 s | Defined cancellable fixture and owned scope; unsupported/unverified cleanup remains a failure, not hidden in timing |
| B06 Selection and feedback | Cached selection/input feedback p95 ≤100 ms; uncached detail p95 ≤250 ms | Actual native UI; selected-action correctness is mandatory regardless of latency |
| B07 Native rendering | p95 frame work ≤16.7 ms on 60 Hz reference display during paced output/navigation | Frame work, dropped frames, long tasks, and DOM/cache size; no promise that every frame is 60 fps |
| B08 Ready time | Broker p95 ≤2 s at 100,000 metadata records; usable app warm ≤2 s, cold ≤4 s | No eager historical-output materialization; startup sample count reported |
| B09 Quiet installation | Broker/adapter CPU <0.5% of one logical core; whole visible product <1% | Ten-minute settled sample; no active child work; polling and wakeups reported separately |
| B10 Quiet observation | No periodic output/history polling when no subscribed data changes; any heartbeat ≤1/minute | Broker/UI instrumentation; OS/webview internal work is separately accounted |
| B11 Backend memory | Broker+adapter steady state ≤128 MiB quiet, peak ≤256 MiB mixed | Candidate process family on reference tier; child workload and inspector separated |
| B12 Inspector memory | Additional native host/webview steady state ≤350 MiB quiet, peak ≤500 MiB mixed | Count all inspector-owned processes; report per-OS native metrics and measurement caveats |
| B13 History growth | B02/B08 remain within budget; broker hot memory grows ≤25% from 4096 to 100,000 metadata records at the same active/output hot set | Absolute B11 budget also applies; detailed history stays paged |
| B14 Fixed-workload soak | No continuing resource growth after caches/retention reach bounds; memory, disk, descriptors and control latency stay within declared limits | W15 after warmup, including post-work state; use profiles to distinguish retained allocator pages from live growth |

Physical resource limits must have a configured accounting policy rather than rely on these observed RSS targets. G1's configuration proposal should name byte limits for the active output cache, pending journal work, subscriber queues, diagnostic output, and durable history/output. Set a small retention budget in W03/W09/W15 and prove bounds under load.

No pure performance win excuses a missed cancellation, duplicate execution, silent output corruption, a false completed state, or unauthorized authority widening.

### Comparative improvement gate

Before each optimization, name one primary metric and the protected secondary budgets. A proposed material-gain threshold is **at least 20% improvement** in the primary user-facing latency or resource metric, beyond observed run-to-run variation, with no unexplained regression greater than 10% in a protected metric and no absolute-budget/correctness failure.

Use that threshold for a claimed performance improvement, not to reject essential correctness fixes. An architectural change can be justified by sustained-capacity evidence or eliminated duplication; report that reason rather than calling it faster. A release-wide “improved performance” statement must enumerate which workflows/OSes actually improved.

For a narrow packet, “absolute-budget failure” refers to the applicable budgets declared before implementation. Protect secondary metrics from regression and retain unrelated baseline misses as explicit phase/release debt. A local gain does not require an unrelated feature rewrite, and does not qualify the whole product. Full phase/release gates remain conjunctive. Supported-load availability is a gate alongside latency; rejecting slower requests cannot produce a passing result.

AI interaction cost also matters: measure calls needed for a useful result and serialized response bytes. Repeated full invocation metadata, base64 plus text, empty polls, and excessive tool-result pages should be visible in this accounting. Evaluate selective output representation in the existing contracts after measurement; retain an explicit raw-byte route and honest truncation. Do not claim token savings from byte counts without a named tokenizer/model and equivalent result content.

## 6. Native workstation acceptance

These scenarios belong to the same product contract and test suite, with native fixtures for real OS differences. A shared Rust compile or browser mock is not a native acceptance run.

### Windows

Test child-observed arguments containing empty strings, spaces, quotes, trailing backslashes, Unicode, and control-shaped text. Use an explicit executable path and a declared command-line encoding policy; test PowerShell/cmd/batch invocation separately when supported. Verify inherited handles and startup failure cleanup. [CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw).

Exercise root/child job membership, assignment failure, nested jobs, breakaway attempts, parent exit, TERM-equivalent policy, forceful stop, broker-handle closure, and completion observation. Keep an unrelated sentinel process. Scope success to associated owned resources and retain uncertainty where proof fails. [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).

Exercise ConPTY bidirectional traffic, resize storms, final output during closure, and a disconnected inspector. The native implementation must account for the documented synchronous communication channels; inspector lifetime must not own terminal handles. [ConPTY session](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session).

Test named-pipe DACL/local-client policy, wrong-user denial, stale/live endpoint collisions, slow clients, and essential control capacity. Test filesystem sharing violations, reparse points, case-sensitive directories where enabled, replacement metadata, and same-volume limits. File replacement APIs do not establish preimage CAS or universal power-loss guarantees. [Named-pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights), [ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew).

### macOS

Certify ordinary groups/children, Darwin exit observation, PTY controlling sessions, and escapes from the declared process group. Linux /proc logic must not be the implementation. Test Unicode and case behavior on the actual selected APFS configurations, links, metadata, permission denial, and coordinated document locations when in scope.

Run from Finder and from the selected MCP host, with stable app/helper signing. Test a selected accessible folder, protected locations, denial and recovery, changed/missing workspace access, close/quit, sleep/wake, and upgrade. Explain permission failures through actual native errors; requesting Full Disk Access is not the default remedy. If sandboxed distribution is later chosen, add its bookmark/entitlement scenarios as a separate requirement. [Apple permission model](https://developer.apple.com/forums/thread/678819).

### Linux

Keep current unreaped-root cancellation regressions. Add long-lived roots, root-exited children, session-changing descendants, ignored TERM, concurrent exit, and inaccessible observation cases. If pidfd observation is introduced, prove identity acquisition/lifetime and keep separate descendant-settlement evidence. Optional cgroup guarantees require configured delegation and native tests. [pidfd_open](https://man7.org/linux/man-pages/man2/pidfd_open.2.html), [kernel cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html).

Test private runtime endpoint ownership, distinct UID, stale/live collision, missing runtime directory, and durable-state separation. Test local filesystem rename/synchronization failures, links, case, non-UTF-8 names according to the declared path representation, and network-filesystem rejection or separately qualified behavior. File and parent-directory durability are separate concerns. [XDG directories](https://specifications.freedesktop.org/basedir/latest/), [fsync](https://man7.org/linux/man-pages/man2/fsync.2.html).

Run the app under each claimed desktop/session/runtime condition, including selected Wayland/X11 environments as applicable. Confirm tray/window behavior, suspend, logout, and startup ownership. Screen/input automation is outside this gate; terminal and file work do not imply permission to manipulate other applications.

### Shared user journeys

1. **First useful command:** install, open, select workspace, understand grant, configure one named host, run a fixture, see its actual command and output. Record median time and friction; proposed goal is under five minutes without editing endpoint paths by hand in the normal flow.
2. **Stay in control:** cancel the selected job during noisy output; pending intent and verified settlement are visually different.
3. **Leave and return:** close the view and MCP adapter, reopen, recover the same job and cursor, show any gap. A disconnect shows last-known state with freshness.
4. **Understand a file change:** preview exact diff, reject stale input, apply under the active grant, inspect the committed revision or actionable conflict.
5. **Use real terminals:** resize, send raw input/interrupt/EOF, preserve scrollback within policy, transfer writer ownership explicitly.
6. **Recover from failure:** expose unknown/incomplete state, failed storage, permission denial, incompatible version, and disconnected broker without rerunning work.
7. **Use the app comfortably:** keyboard-only navigation, visible focus, screen-reader labels and status announcements, readable text/contrast, high-DPI scaling, reduced motion, stable selection/scroll, and clear empty/partial/loading states.
8. **Maintain the installation:** upgrade with retained history/grants, handle interrupted migration, revert safely where supported, and uninstall only owned startup/configuration resources.

Do not turn a command journal into an assistant chat transcript. The app's main task is to show and control workstation work. Branding and styling should support that task through the existing components and tokens.

## 7. Failure and growth contract gates

### Persistence and retry

Test legacy records before introducing required workspace/kind/revision fields. An old record without authority detail stays explicitly legacy; do not fabricate a principal or grant. Tuning retention/concurrency must follow the chosen versioned retry policy, never silently create a new attempt. After detailed history expires, a retry returns the specified expired/retained identity response and cannot be mistaken for new work.

Validate retry contexts at the broker, including restart and clock-change rules. Before reclaiming retry evidence, establish a compatibility cutoff or effective fence against old writers. Test old clients and, where relevant, the old broker against migrated/archived state; unsupported downgrade must not reinterpret removed evidence as a fresh request.

For queued work, test queue-wait versus execution-deadline start points, pinned environment/profile versions, and revalidation before dispatch/commit. Enqueue, then revoke/remove the workspace grant, expire, or cancel before dispatch. The outcome must match the declared policy without unintended execution.

A future contract must distinguish lifecycle, result certainty, resource settlement, and output completeness. In the current source, Unknown may be visible while active cleanup continues: State::fault requests cancellation, while Shared::finish later clears active ownership. An event consumer must not interpret Unknown or a generic terminal enum as proof that all resources stopped. See [contracts](../src/contracts.rs) and [State::fault/Shared::finish](../src/execution.rs).

### Subscription and cache

Test snapshot/change races, reconnect during commit, evicted revisions, broker restart, duplicate/coalesced notifications, and slow subscribers. Subscriber capacity and essential request capacity need distinct budgets; current 32 one-shot connection permits and five-second frame waits cannot simply become 32 permanent subscriptions.

Output-available events carry operation identity and latest sequence; readers retain their own cursors. Caches remain bounded projections of authoritative state. They cannot turn an unavailable capability into local simulated success.

If output saturation can drop pending frames, force a missing middle range followed by resumed capture. Read across the hole, reconnect, and restart with independent readers. Require durable dropped-range evidence, correct cursor advancement, no false continuous/complete output, and declared UTF-8 behavior when missing bytes split a character. Current prefix-eviction gap behavior alone does not satisfy this case.

### File changes and terminal input

Per-process serialization cannot control arbitrary unrelated writers. Test and document the actual supported commit guarantees. Network shares and cross-filesystem moves require explicit contracts.

Input acknowledgement means bytes were accepted into the input path, not that an interactive program understood them. Do not automatically replay raw terminal input after a lost acknowledgement; that can repeat effects. Use explicit writer ownership and caller-visible uncertainty.

## 8. Report and release format

Each run directory should contain a manifest, raw machine-readable observations, summarized distributions, correctness assertions, resource traces, and failures. These are proposed G0 artifacts; no performance run directory is included as a passed result in this planning revision.

The manifest records run ID/time, source/dependency/binary hashes, workload seed and corpus hash, platform/build data, configuration, sample counts, timing calibration, scope, and excluded/failed measurements. The summary must show baseline and candidate values, percentage change, variation, pass/fail against frozen budgets, and any remaining parity gap.

Release gates are conjunctive: correctness, native behavior, performance, usability, migration, and distribution must each pass for the capability/OS being claimed. Keep exact claim wording beside its evidence. Publish results and limitations with the eventual support matrix at devagotchi.net.
