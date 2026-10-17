# Devagotchi next execution guide

Planning revision: **2026-10-17 UTC**. Immediate public packet: **P0 — rebuild and publish the existing reference Site**. Core packet: **G0 — reconcile the source and establish a reproducible baseline**.

This guide is a standalone handoff for a new implementer. The rebuilt website/reference service is published on the existing Site as version 3. The [publication receipt](../verification/website-publication.json) records HTTP/download checks, the enabled OAuth-protected MCP and installed plugin, and remaining DNS, visual-QA, and authenticated-client limits. Core G0 and graph/mesh packets describe future implementation. This documentation revision changes no product code or historical test evidence and does not report those steps as completed.

## 1. Mission and product context

Devagotchi is an owned improvement to DesktopCommanderMCP: a universal workspace bridge beginning with a locally installed MCP host and a styled native desktop application, with every admitted task represented as a graph and growth into a trusted internal mesh. The app shows actual commands, dependencies, attempts, output, workspace authority, changes, and recovery. Windows, macOS, and Linux remain required targets. The intended repository is eonsofstupid/devagotchi. The existing Site is https://devagotchi.justsayit.chatgpt.site; devagotchi.net has been added and is pending DNS.

The user's governing requirement is to extend existing responsibilities before inventing new boundaries and demonstrate real workstation performance and experience gains. The latest priority also explicitly authorizes a useful public website, read-only reference API, and reference MCP now. P0 delivers that independently of native release. G0 produces baseline evidence and contract decisions; it does not implement graphs, files/PTY, mesh execution, or runtime HTTP. Public reference publication does not wait for G0–G5 to finish.

### Canonical sources

- [Architecture and growth plan](product-and-platform-plan.md): scope, ownership, decisions, phases, and traceability.
- [Performance and experience specification](performance-and-experience-spec.md): W01–W15 workloads and proposed B01–B14 budgets.
- [Task-graph and mesh contract](task-graph-and-mesh-contract.md): candidate records, lifecycle, retries, cancellation, delegation, partitions, public/runtime separation, and scoped successor packets.
- [Desktop Commander user evidence](desktop-commander-user-evidence.md): primary reports and follow-ups, including fixed and host-specific cases; workload hypotheses rather than prevalence or superiority claims.
- [Verification evidence](../verification/README.md): historical Linux checks, command details, hashes, and limits.
- [Project README](../README.md): current development entry points.
- The package's parent audit and pinned reference snapshot: upstream findings F01–F10 and their reproductions.

Use the master plan for architectural decisions. Record changes there and update its review summary; do not create another competing architecture document.

## 2. Starting evidence and constraints

The exact pre-planning foundation archive has SHA-256 075a259e66f50168c84989d8cedb81c1debadd08e950bf2f3abb468c4e2b7987. Its 77 source-manifest entries were verified during recovery. The current planning package preserves product source and historical raw test reports; its planning-revision manifest records documentation changes separately.

The source contains a Rust broker, official rmcp stdio adapter, SQLite journal, generated contracts, and Tauri/React inspector. Execution/journal are Linux-specific today. Historical results include seven real native WebKit/broker UI checks; no Windows/macOS runtime certification, named host integration, full Desktop Commander benchmark, or comparative performance result is available.

No graph engine or mesh owner was found in the recovered source. The proposed graph scheduler extends `Broker`/`Shared`; attempts reuse ordinary operations and the same journal. Runtime HTTP and trusted peer execution remain unimplemented. The existing managed Site is `appgprj_6a8150d1b19481919fa3a4e84c13824c`, using Vinext, React Aria Components, and Style Dictionary. Reuse that source/application; do not create a replacement Site merely because this desktop archive is separate. The rebuild is published. Custom-domain DNS validation, browser visual QA, and an authenticated named-client MCP tool call remain incomplete; see the separate publication receipt.

The latest repository read returned 404. Do not assume it is empty or copy over unseen work. The source audit reference is DesktopCommanderMCP f508e524469c26f6366e17dbfd3ee5a69d112127, represented by a partial saved snapshot. Obtain a full verified checkout before full-server comparison.

No installed MCP host is named. Scripted clients can measure transport paths, but cannot satisfy the actual-host gate. Native machines/OS versions also need to be recorded before their results can be claimed.

## 3. Entry checks

**P0:** confirm the existing Site/application and inspect its source, components, tokens, route conventions, and publication workflow. Reuse them. Read the public/runtime separation in the graph contract and record the current source/capability status before authoring claims. No new permission request is needed for the website/reference publication already authorized by the user. Pending GitHub access, native machines, named runtime clients, or custom-domain DNS do not block publication on the existing Site URL.

**G0:**

1. Read this guide, the master decisions, and the evidence limits. Record the exact source revision/package used.
2. Retry repository access through the authorized connection. If it works, inspect real instructions, default branch, manifests, source organization, CI, and relevant existing components before edits. Preserve unrelated work and reconcile in an isolated branch/worktree. If it remains inaccessible, label the work isolated and continue useful baseline work without claiming integration.
3. Inventory actual native test capacity and choose the initial supported-test candidates. Treat Windows, macOS, and Linux as required targets; record unavailable runs as blocked.
4. Record the named MCP host if available. Do not infer one from popularity or configured scripted fixtures.
5. Ensure disposable workspace/process fixtures and private output paths are available. Do not benchmark against the user's real documents, terminal sessions, or long-running applications.

G0 can proceed alongside P0. Missing native hardware or repository access remains visible; it does not justify fabricated evidence or a silent scope reduction. Each packet has its own receipt and cannot borrow the other's completion status.

## 4. Ownership and allowed edits

| Area | Current paths | Scoped allowed change | Owner |
|---|---|---|---|
| P0 website/reference | Existing managed Site source and publication workflow | Rebuild existing UI/content, actual read-only HTTP/MCP handlers, documentation/source links, bounded validation and publication | Site owner |
| Core timings | src/execution.rs, src/journal.rs, src/ipc.rs, src/bin/bridge-broker.rs | Bounded timing/counter instrumentation only, preserving behavior and wire semantics | Core benchmark owner |
| Adapter timings | src/bin/bridge-mcp.rs | Measure request/cancellation-of-wait boundaries; preserve official SDK ownership | MCP integration owner |
| Native UI measurements | apps/desktop/src/useInspector.ts, App.tsx, src-tauri | Opt-in fixture/measurement path that does not ship fake data or weaken native capabilities | Inspector owner |
| Harnesses | Existing tests and proposed tests/performance directory | Reproducible workload drivers, manifests, raw observations, reports | Verification owner |
| Decisions/docs | This docs directory and existing README/evidence notes | Reconcile ownership, resolve proposed semantics, record baseline/missing results | Architecture owner |

Do not add another broker, workspace daemon, audit DB, transport service, universal provider registry, or global design-system package in G0. P0 extends the existing Site and has no workstation execution authority. Do not move broker responsibilities into either UI. Do not reformat or relocate large amounts of product code while establishing the baseline.

A measurement hook may be unnecessary if black-box timing and OS tooling provide the needed evidence. Add only instrumentation that resolves an identified attribution gap, and measure its overhead. Keep metrics off protocol stdout.

## 5. Execution packets

### P0 — Publish the existing Site and useful read-only reference service

**Input:** existing Site identity/source, the current desktop source/contract/evidence package, and the user's publication instruction. P0 can complete before G0 benchmarks or native release.

1. Preserve the existing application's stack and reuse its components/tokens. Explain Devagotchi's local foundation, every-task graph direction, internal-mesh direction, and actual implementation limits. Examples must be visibly illustrative; a graph illustration is not live task activity.
2. Implement real bounded read-only HTTP endpoints and Site MCP tools for capability/status information, local tool schemas/documentation, and the graph example. These handlers must return usable content, not placeholder success. Publishing `operation_start` documentation does not make that runtime method callable through the Site.
3. Use the declared MCP 2026-07-28 profile. Verify discovery, the deterministic tool catalog, structured tool results, required metadata, optional `clientInfo` absence, header/body matching, Origin policy, invalid/oversized inputs, and unsupported versions/methods. The short reads may return JSON without Tasks or subscriptions. Do not add a legacy initialization shim without an observed provisioning requirement and an explicit recorded scope change.
4. Link the canonical plans, user-evidence ledger, and prepared public source package. Identify artifact revision/hash and distinguish source availability from installer/runtime support. Keep workstation paths, credentials, grants, and execution endpoints out of the Site handlers.
5. Publish through the existing Site workflow and exercise the actual deployed routes. Record the final Site URL, deployment/source identity, request/result evidence, and custom-domain status. If DNS is still pending, report the working Site URL and that exact remaining boundary; do not delay useful publication or claim devagotchi.net is verified.

**Exit:** a real published reference service with a receipt, working links, and truthful capability boundaries. This does not certify workstation runtime HTTP, graph execution, mesh operation, a named MCP host, or another native OS. **Recorded result:** Site version 3 is published. Thirteen public GET checks returned 200; the source ZIP and public schema match their inputs. The deployed MCP returns its OAuth challenge and its provisioned plugin is installed. Twelve focused handler checks, type checking, and the production build pass. The managed browser preview failed to become healthy, installed MCP tools did not surface for an authenticated call, and devagotchi.net remains pending DNS. These limits remain explicit in the publication receipt.

### G0.1 — Reconcile existing owners

**Input:** recovered source, actual repository if accessible, S01–S10 in the master plan.

Inspect where commands, policy, persistence, IPC, UI data, styling, and native lifecycle already live. Record an owner map with the specific source inspected. If the real repository contains an equivalent implementation, extend it and reconcile this proof rather than installing a parallel stack.

Inventory existing reusable UI components/tokens before designing more screens. Other ecosystem repositories, including RRFlow, are not assumed dependencies; inspect their APIs, licensing, and actual ownership if a concrete integration is proposed. Read the user-evidence ledger when selecting workflows, preserving fixes, corrected diagnoses, and host/proxy boundaries rather than treating every report as a current server defect.

**Exit:** one responsibility map reconciled to accessible source, with uninspected areas explicitly marked. Any new boundary proposal identifies the inadequate existing owner, state authority, failure behavior, replacement code, and verification.

### G0.2 — Freeze semantics that affect performance

Resolve the following in the master decision register before optimized candidates are compared:

- The durability and overflow meaning of retained output: bounded tail capture, explicit gaps, persistent storage failure, and any separate full-capture mode.
- Independent active/queued/history/retry limits. Decide whether queueing is opt-in and how overload appears to callers.
- Versioned retry scope/expiry and migration of current config-hash-bound keys. Operational tuning must not silently create fresh execution.
- Stored record version versus wire version, and how old records preserve their original meaning.
- Outcome certainty, resource settlement, and output completeness. Unknown is not proof that cleanup finished.
- Snapshot/change-feed watermark and per-operation output cursor. No implicit replacement of one by the other.
- Deadline meaning across sleep and explicit long-lived/terminal jobs.
- Initial trust scope for the installation. Existing same-user credentials do not authenticate mutually untrusted applications; decide before advertising per-client authority.
- Native path representation, including non-UTF-8 names and Windows-native path edge cases.
- Graph identity placement: single-step tasks and attempt-to-operation bindings must preserve existing retry/admission evidence. Execution DAG, provenance links, and mesh topology have different meanings. Freeze only the graph semantics needed by the next packet; unneeded future mechanisms remain proposed.

Add three explicit gates to those decisions: queued work pins its environment and revalidates authority before dispatch/commit with separate queue/execution clocks; broker-validated retry expiry and restart/clock rules fence old clients/writers before evidence reclamation; any interior output loss records durable ranges and defines cursor/UTF-8 behavior across the hole. Exercise enqueue→revoke/expire/cancel→dispatch, old-version access after migration/reclamation, and dropped-middle-output→resume→restart.

Prefer concrete proposed defaults and evidence over asking the user to choose every implementation detail. Escalate only a genuine product/authority decision. Record each acceptance check. Pre-release contract changes may fail unsupported versions clearly instead of adding speculative compatibility shims, but cannot silently discard historical retry evidence or reinterpret saved effects. Nonblocking mesh details do not become entry conditions for P0 or local optimization.

**Exit:** decisions sufficient to compare equivalent behavior and begin G1; remaining choices identify the precise later gate they block.

### G0.3 — Build a repeatable measurement harness

Use a proposed tests/performance runner with explicit arguments for binary paths, baseline identity, workload, run count, seed, output directory, and configuration. It must refuse to overwrite evidence accidentally and must identify every owned fixture process for teardown.

Prefer small deterministic compiled/native fixture programs for process timing when language startup would dominate the metric. Also include representative real commands as separate user workflows. Capture fixture bytes/arguments/effects as correctness controls. Separate cold/warm state and keep state directories fresh when varying D0 configuration; otherwise its current config identity will correctly reject old keys.

Create structured reports containing:

1. Run manifest: source/dependency/binary hashes, build flags, OS/hardware/filesystem/power state, workload seed/corpus hash, retention/durability, sample counts, timing method.
2. Raw observations: timings, counters, errors, emitted/retained/dropped bytes, resource samples.
3. Summary: p50/p95/p99 where justified, variability, failed/blocked runs, and budget evaluation.
4. Correctness controls: independent readers, exact arguments/bytes, declared cancellation scope, no replay, and expected fixture effects.

Use monotonic clock calibration for native/UI intervals. Bound instrumentation queues and diagnostic storage. Do not log arbitrary user secrets or command contents as metric labels.

Predeclare all attempts and load arrival behavior; include failures in the denominator. Success-conditional percentiles must not reward dropping slow requests. Supported-load unexpected rejection/timeout is an acceptance failure under the default fixture policy.

**Exit:** the same workload can be rerun from its manifest, with meaningful failures and complete fixture teardown. A report without source identity or raw observations does not pass.

### G0.4 — Establish baselines

Build D0 with the pinned manifests in release mode for performance, keeping historical evidence separate. The existing core build route is Cargo; the inspector uses the documented frontend build and native manifest in its README. Inspect harness arguments before running them; some historical harnesses target debug paths and must not be silently labelled release evidence.

Measure W01–W05 and W07–W11 on available Linux/native paths. Exercise the foundation's supported workload sizes as configured. Record the cumulative admission boundary honestly. Do not remove the cap or alter durability to make D0 complete an unsupported tier.

Acquire and verify a full DC checkout/build for comparable ordinary-process workloads. If that is blocked, finish the D0 report and mark the DC comparison unavailable; no speed-superiority conclusion can follow. Do not substitute the source harness's fake child process for a full-server benchmark.

Record native Windows/macOS baseline absence as unsupported for D0. Use their actual implementation qualification later; a stub that reports unsupported does not constitute a workload timing.

**Exit:** reproducible results, including real failures and missing comparisons, with no unqualified faster claim.

### G0.5 — Attribute costs and select the next bounded change

Rank observed costs by user impact. The source audit suggests journal/registry coupling, eager history, repeated query/encoding work, UI polling/fan-out, and Linux observation as candidates. Select the first G1 packet using measurement.

For that packet record the primary metric, protected budgets, exact existing owner/files, preserved invariants, entry fixture, implementation steps, regression checks, and rollback. Prefer a narrow improvement such as shorter read critical sections if it addresses the measured issue; do not combine every optimization into an unreviewable rewrite.

Declare which absolute budgets apply to this packet before implementation. Keep unrelated baseline failures visible as later phase/release requirements; a narrow verified gain neither requires an unrelated rewrite nor establishes whole-product qualification.

**Exit:** one concrete G1 packet ready to implement and the master plan reconciled with actual evidence.

## 6. G1 invariants already identified

These are mandatory constraints for the next implementation author:

- The broker owns an accepted operation even if the IPC task or MCP wait disappears. A future async persistence handoff cannot leave a committed admission without independent broker ownership.
- Keep synchronous database waits out of the shared registry lock and async worker paths. Replacing a mutex with an async mutex or adding per-frame unbounded blocking jobs is not the intended solution.
- A single bounded journal worker may batch output; it must not publish durable state before commit. Give cancellation/control priority between bounded commits, without waiting for all unrelated output.
- Final completion orders after the required output/settlement evidence. A stale queued snapshot cannot overwrite a later cancellation or final state.
- Preserve raw retained bytes, incremental UTF-8, independent cursors, and explicit gaps.
- Page cold history while retaining the selected retry-evidence policy. Test expired history and legacy keys before deleting records.
- Preserve the native identity discipline around root/group signalling. Linux root events do not establish that descendants have settled.
- A subscription extends connection lifetime. Reserve/control capacity and define idle/liveness behavior; do not occupy all current one-shot permits indefinitely.
- The native host and MCP adapter may share typed client methods; their security declarations and wait-cancellation behavior remain deliberately separate.
- All process, workspace, and file work uses the same authority and journal. New file results must not invent executable/exit-code fields to fit a process-only record.
- Graph readiness and attempts extend that same lifecycle owner. Graph changes, attempt reservation, and local state transitions have committed ordering; a transport retry cannot accidentally allocate a second attempt.
- Graph cancellation does not certify resource settlement; dependency failure, output unavailability, an offline target, and uncertain effects remain visible. Inputs bind to recorded producer attempts and do not silently change when a producer is retried.

## 7. Validation and completion report

Run only the checks needed for the actual changed risk and required gates. G0 documentation-only reconciliation does not justify a full native rebuild; instrumentation or harness changes do require representative correctness and overhead checks.

P0 requires actual published HTTP/MCP route checks and visible content/link review. Save that receipt separately from historical Linux verification. A browser page loading is insufficient evidence for its MCP/API handlers; a reference endpoint returning schemas is insufficient evidence for workstation execution. Do not label a staged rebuild deployed before publication verification.

For altered execution/journal/IPC behavior, retain the historical correctness scenarios and add the new case that resolves the measured risk. A mocked unit result cannot replace real process/IPC acceptance. For altered native UI measurement, use actual app/webview data paths and retain selected-cancellation isolation.

The G0 completion report must include:

- Source/repository status and exact baseline identities.
- Owner map and any justified boundary decision.
- Frozen metric definitions, targets, and semantic choices.
- Measured results with distributions, resource costs, and failed/blocked/unsupported runs.
- Instrumentation overhead and limitations.
- First proposed G1 packet, its preserved invariants, and acceptance/rollback criteria.
- Remaining external inputs: repository access, named host, and native test capacity where still missing.

G0 completion means baseline and next-change evidence are ready. It does not mean performance superiority, file/PTY parity, graph/mesh execution, runtime HTTP, cross-platform support, or release readiness. P0 may independently have completed website/reference delivery; cite its receipt rather than treating it as a G0 result.

## 8. Recovery and stopping conditions

Use disposable fixture state and stop only processes created by the harness. Record ambiguous teardown as failure; do not kill processes from remembered PIDs without owned identity.

Preserve historical reports. New runs go to new run directories with new manifests. Do not overwrite their binary/source attribution with current hashes.

If a measurement requires changing semantics, stop that comparison and record a separate experiment. If repository content conflicts with this isolated foundation, reconcile the real owner and update the plan before copying code. If a new architectural boundary becomes necessary, document why extension fails and the concrete acceptance case before implementation.

G0 requires no new mesh service, account enrollment, external message, or repository publication. P0 explicitly includes publication of the existing Site/reference service and is already authorized. Complete each packet's useful work and report its actual blockers without inventing another approval ritual.

## 9. Scoped graph and mesh successors

The master retains G0–G5 and their owners. Use the following bounded packets after their prerequisites rather than launching a second architecture program:

| Packet | Start when | Scope and exit |
|---|---|---|
| G1.GA — Graph records | Versioned identity/storage and canonical schemas are stable | Contracts/journal: bounded single-step/DAG types, stable historical mapping, retries/migrations, atomic admission, and paged reads. |
| G1.GB — Local graphs and inspection | G1.GA, admission/cancel, and snapshot/revision semantics are ready | Existing broker/execution/IPC/inspector: real attempt-operation bindings, branching and failed dependencies, explicit retries, cancel races, bounded graph expansion. Ordinary-process graphs do not wait for files or PTYs. |
| G2.GC — Native qualification | Local graph lifecycle plus a participating native provider | Real graph attempts through each claimed OS and host, preserving native cancellation, environment, lifecycle, and output evidence. |
| A1 — Authenticated runtime API/MCP | Versioned broker API, authenticated subject/grant path, and qualified participating runtime | Thin HTTP/MCP binding to the existing owner; audience/scope/target checks, request abandonment, overload, typed responses, and named-client gates. Public reference publication is not this packet. |
| M1 — Two trusted brokers | Local graphs and durable authenticated handoff contracts | Existing grants/configuration/journal/routing: pairing, outbox/inbox binding, target receipts, bounded admission contexts, partitions, stale epochs, pending cancellation, and reconciliation. No fallback execution on another target. |

The [task-graph and mesh contract](task-graph-and-mesh-contract.md) supplies candidate records and adversarial cases. The [user-evidence ledger](desktop-commander-user-evidence.md) supplies additional target, readiness, cleanup, content, and adoption scenarios. Define packet-specific budgets using the performance specification; preserve correctness and count failed attempts. Pilot only the qualified platforms, then earn the full Windows/macOS/Linux matrix. Broader routing and file/terminal graph steps follow their demonstrated dependencies, while future implementation choices remain proposed.
