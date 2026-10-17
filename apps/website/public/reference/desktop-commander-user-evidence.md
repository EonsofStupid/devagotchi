# Desktop Commander user evidence and Devagotchi requirements

**Retrieved:** 17 October 2026, UTC. **Purpose:** turn reported user problems into reviewable product requirements, while preserving fixes, uncertainty, and host-specific limitations.

## Method and interpretation

This is a purposive sample of primary reports in `wonderwhy-er/DesktopCommanderMCP`, found through GitHub issue search, including searches for Windows and timeout symptoms and issues with substantial discussion. Issue bodies, individual comments, current issue metadata, and selected linked pull-request metadata were read. Dates and open/closed states below come from GitHub's API; public issue URLs provide the review trail. Some web-indexed pages omit recent comments, so resolution claims link directly to the relevant comment where possible.

This is **not** a census, a random sample, a count of affected installations, or a comparison benchmark. Multiple accounts provide independent authorship, not independently verified identities or statistically independent experiments. A follow-up, cross-reference, bot analysis, or repeated post by the same author is not another independent user. Claimed reproduction results remain the reporters' results; this research did not execute Desktop Commander, reproduce failures, test a candidate fix, or audit the hosted service.

An open issue can contain a confirmed fix. A closed issue can mean no new reproduction was supplied. A proposed PR is not a released fix. Package labels, the actual installed artifact, the host's bundled runtime, and a hosted service's deployment can differ. These distinctions are material to this sample.

All Devagotchi requirements and acceptance cases below are **proposals**, not statements of implemented behavior or performance superiority. They should be reconciled with the existing product, platform, and performance specifications before implementation. Sample counts are proposed test designs. They are not existing test results.

## 1. Startup and installation can consume the host's entire connection budget

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#655: plugin installation loop](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/655) | 25 Aug 2026; `iwallplace`; **open** | Windows Claude plugin startup repeatedly resolved `npx @latest`, installed dependencies, and was killed before connecting. The reporter measured approximately 75 seconds for installation. The DXT extension worked concurrently: these were different installation paths. Proposed [PR #657](https://github.com/wonderwhy-er/DesktopCommanderMCP/pull/657) remained open and unmerged at retrieval. |
| [#510: intermittent initialize timeout](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/510) | 14 Jun 2026; `JEM-Fizbit`; **open** | macOS, 0.2.42 extension. The reporter observed attach timeouts and attributed them to a feature-flag refresh against an unreachable host. Network causality is the reporter's diagnosis; this research did not reproduce it. |
| [#645: shared-pool startup delay](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/645) | 21 Aug 2026; `greatpal88`; **closed 15 Oct**, reason `not_planned` | Windows report, with independent macOS and Windows comments by `ForAlfred` and `erfeeneo-rgb`. [PR #777](https://github.com/wonderwhy-er/DesktopCommanderMCP/pull/777) merged 6 Oct; maintainer says 0.3.0 lazy-loads document libraries. The [closure](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/645#issuecomment-6186867627) explicitly says shared-pool delays are not all confirmed resolved; no new retest was supplied. |

**Recurrence assessment:** independent reports support a startup-friction theme. They do not establish one shared cause or prove all installation methods are affected.

**Proposed Devagotchi requirement:** separate installation/update from launch; report installation, transport connection, and execution readiness distinctly. Local readiness should not require optional network services. Expose the running artifact and host/runtime identity in diagnostics.

**Measurable acceptance:** on each supported native host, run at least 30 cold and 30 warm starts with normal networking, unavailable optional endpoints, and an interrupted update. Record all attempts, successes, timeouts, and latency distributions. Verify that an interrupted update leaves the prior usable installation available and that local readiness still completes with optional networking blocked. Use the performance specification's approved budgets, not successful-request latency alone.

## 2. Session changes expose ownership and diagnostic gaps across the host boundary

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#315: Project works only for the first message](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/315) | 22 Jan 2026; `vsnmic`; **closed 5 Feb**, completed | Windows Claude Desktop/0.2.29. Maintainer discussed DC 0.2.32/0.2.33 fixes, including protocol versioning, then [closed after a Claude-side fix](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/315#issuecomment-3853178119). Historical resolved report. |
| [#451: first call in second session hangs](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/451) | 28 Apr 2026; `Autonomaniac`; **open**, updated 19 Aug | Original report describes multi-minute waits and restarting Claude. Independent [19 Aug evidence from `renaudm`](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/451#issuecomment-5343775077) says affected calls did not reach any of three MCP servers, including other extensions. This contradicts the original server-lifecycle hypothesis and points toward host dispatch, without proving its exact failure point. |

**Recurrence assessment:** users encounter session-related failures, but this sample does not justify attributing them all to Desktop Commander's execution engine.

**Proposed Devagotchi requirement:** keep operation ownership independent of an observing MCP connection. Expose request receipt, durable acceptance, operation identity, client connection generation, and terminal outcome separately. An adapter cannot diagnose an upstream request it never received; the UI and documentation must say where visibility starts.

**Measurable acceptance:** exercise 100 client connect/disconnect/reconnect cycles with active disposable jobs. Verify one admitted operation for a repeated idempotency key, continued ownership after observer disconnection, reconnectable output cursors, and explicit ambiguity after a broker crash. Logs must distinguish a received but rejected request from an accepted operation. No automatic replay of an operation with unknown effects.

## 3. Windows execution behavior depends on the actual host environment

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#480: stripped PATHEXT/WINDIR](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/480) and [#481: ARM64 PATHEXT](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/481) | 26 and 29 May 2026; `tangerine4home`, `coffeedogAI`; both **open** | Independent Windows command-resolution reports, with different causal conclusions. #480 attributes filtering to Claude's MSIX boundary; #481 attributes its observed difference to the DC launch path. #481's timeline references a PATHEXT repair commit on 1 Jun. Open status must not be read as proof that repair is absent. |
| [#623: missing Windows variables](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/623) | 5 Aug 2026; `RaMich271de`; **open** | Reporter says missing standard environment variables broke SAPI speech on two machines; identical Task Scheduler execution worked. The affected distribution was DXT inside Claude. |
| [#702: silent OpenSSH failure](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/702) | 13 Sep 2026; `hubner`; **open** | Original console hypothesis was corrected. On [11 Oct](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/702#issuecomment-6112368486), the original reporter said both machines worked after a Claude update, without changing DC. Independent `sf-75` reported that restoring `ProgramData` fixed their environment and [reproduced that distinction on 13 Oct](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/702#issuecomment-6140319330) through another host path. |

**Recurrence assessment:** strong evidence for host/environment compatibility as a user problem; mixed and sometimes corrected evidence for its owner. The OpenSSH complaint is not proof that a pseudoterminal alone fixes it.

**Proposed Devagotchi requirement:** make executable, argv, shell choice, environment policy, and platform provider explicit. Define required Windows environment behavior without silently inheriting arbitrary secrets. Publish support by tested host/runtime combination.

**Measurable acceptance:** run the packaged native app and supported MCP hosts, not only a developer terminal, on Windows x64/ARM64 and each claimed macOS/Linux platform. Compare documented native-command fixtures, environment-dependent fixtures, Unicode output, argument boundaries, exit status, and cancellation. Missing provider support must produce a clear unsupported response. Do not convert Linux correctness evidence into Windows/macOS claims.

## 4. Tool contracts and visible content can diverge between clients

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#291: multiple-file read shows metadata only](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/291) | 11 Dec 2025; `milesian01`; **open** | Reporter used ChatGPT developer mode through Tailscale Funnel/Supergateway. The same server returned contents to another client; single-file read worked. This is a particular proxy/client interoperability report, not proof that all file reads fail. |
| [#446: directory listing shows only metadata](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/446) | 27 Apr 2026; `JesperLive`; **open** | Windows 0.2.39/DXT report describes missing children on several nonempty directories despite advertised listing behavior. No verified current-artifact fix was found in the issue discussion. |
| [#686: per-call pagination expectation ignored](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/686) | 8 Sep 2026; `Flynn-Kawolsky`; **open** | Reporter supplied `limit: 3` and received a whole small file or the configured 1,000-line cap. The actual loaded schema and parameter binding were not attested. Treat this as a reported argument/documentation mismatch; do not assert a proven defect in correctly named pagination parameters. |

**Recurrence assessment:** independent authors report a mismatch between expected content and what their client displays. The layers involved differ, and the schema/adapter/UI route matters.

**Proposed Devagotchi requirement:** derive SDK/UI contracts from a canonical definition, reject unknown arguments visibly, and test both structured and human-visible responses in named clients. Pagination and truncation must be explicit and byte-bounded. Optional visual rendering must not hide the only usable result.

**Measurable acceptance:** a client matrix must obtain identical fixture contents and state through the documented API/MCP path, including clients that ignore optional UI metadata. Test exact page boundaries, invalid parameters, empty/nonempty directories, Unicode, oversized results, and interrupted output. Assert byte equality where the contract promises raw bytes, and verify that unsupported fields cannot silently change the requested operation.

## 5. Completed reads can leave Windows files unavailable for replacement

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#476: prefix read blocks later overwrite](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/476) | 18 May 2026; `jonathanzhan1975`; **open** | Reported Windows atomic-replace failures after bounded reads; Windows Restart Manager identified the DC Node process as holder. |
| [#502: unreleased file handles](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/502) | 10 Jun 2026; `ChrisCatCP`; **open** | Independent author reported locked source temporary files and identified the holder with Sysinternals Handle. No version was supplied. |
| [#631: line-budget handle lifetime](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/631) | 19 Aug 2026; `AAS2482`; **open**, updated 17 Oct | Explicitly a follow-up to #476, although the author differs. Further independent comments cover several versions. [`joshuatheorigin` on 14 Oct](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/631#issuecomment-6166932117) and [`telemetrya2018-dotcom` on 17 Oct](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/631#issuecomment-6227959555) report isolated reproduction on published 0.3.0. Reports disagree about 0.2.52 behavior/artifacts; no single clean version boundary is established here. [PR #674](https://github.com/wonderwhy-er/DesktopCommanderMCP/pull/674) was open and unmerged at retrieval. |

**Recurrence assessment:** the strongest continuing resource-cleanup complaint in this sample, supported by different authors, different affected workflows, and recent published-package reports. Their tests have not been independently rerun by this research.

**Proposed Devagotchi requirement:** ownership includes deterministic closure of every file/stream on success, short read, timeout, cancellation, and error. A read tool must not leave an undisclosed lock that blocks another legitimate writer.

**Measurable acceptance:** in a long-lived Windows service, perform bounded, EOF, error, and cancelled reads of disposable files, then immediately atomically replace each target from another process. Require every replacement to succeed after the tool reports completion. Repeat 1,000 cycles and verify that fixture-specific handles are released and process handle count reaches a stable bound. Preserve CRLF/encoding according to the declared byte/text contract.

## 6. Online indicators do not necessarily mean a command can execute

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#633: online/pong while tools fail](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/633) | 19 Aug 2026; `neckzoozoo-cloud`; **open** | Independent Windows/macOS comments corroborated the symptom. Maintainer announced a 0.2.51 fix; `vitalshytskostudent-oss` reported [passing controlled recovery](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/633#issuecomment-5729941237) and [live checks](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/633#issuecomment-5730204615) on 18 Sep. Their original oversized-image stress case was not retested. This failure mode has positive fix evidence despite open status. |
| [#798: dashboard Online, dispatch offline/timeouts](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/798) | 29 Sep 2026; `500234xx`; **open**, updated 12 Oct | Independent authors reported related symptoms on Windows/macOS. The thread includes distinct causes and recoveries: time synchronization, self-restarting wrappers, spontaneous recovery, and an October service incident. Do not count a corrected dirty-environment report or autonomous code analysis as a fresh independent reproduction. |
| [#838: Realtime/Presence outage](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/838) | 12 Oct 2026; `ckechel`; **closed 14 Oct**, completed | macOS/Linux report, with an independent Debian confirmation. Maintainer [closed the October Supabase incident as resolved](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/838#issuecomment-6168895359), citing 0.3.0 reconnect/backoff hardening. This is historical incident evidence, not an ongoing outage claim. |

**Recurrence assessment:** substantial independent evidence for misleading health and difficult recovery. It spans local forwarding, remote dispatch, host clocks, and upstream service availability; it is not a single persistent bug.

**Proposed Devagotchi requirement:** distinguish registered, transport reachable, authenticated, and execution-ready states. Health must include a bounded non-mutating probe through the relevant execution path, with its timestamp and connection generation. Recovery must preserve operation receipts and uncertainty.

**Measurable acceptance:** inject local worker death, stale presence, network partitions, backend unavailability, clock skew, and duplicate connection attempts. The UI must stop advertising execution readiness within the declared detection budget. Recovery must be single-flight and bounded; no side-effecting operation may be blindly replayed after uncertain acceptance. Report error rate, recovery time, and all attempted probes alongside latency.

## 7. Devices, conversations, and operation handles need explicit ownership

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#586: explicit target sent to wrong device](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/586) | 14 Jul 2026; `eumel1982`; **open** | Reported an actual wrong-host write on 0.2.46. Maintainer said fixed on 20 Jul. The reporter's [28 Sep retest](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/586#issuecomment-5866374084) confirmed correct explicit routing, rejection of missing/nonexistent targets, and no fallback on 0.2.51/current service. Remaining observations: stale online status, ambiguous offline timeout, and callers must repeat device ID for process reads. PID-collision risk was an inference, not reproduced cross-device disclosure. |
| [#599: process/search session loses device affinity](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/599) | 18 Jul 2026; `riprayx`; **open** | Independent three-device report about follow-up handles. Maintainer said it should be fixed on 20 Jul; no equivalent reporter retest appears in this thread. Preserve that distinction. |
| [#722: conversation-scoped Remote sessions](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/722) | 18 Sep 2026; `cxhttt233`; **open feature request**, updated 16 Oct | Separate current requirement, not proof of the old routing bug. [`josh87523`](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/722#issuecomment-5749846499) reported missing conversation metadata; [`zhongyangchuwu`](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/722#issuecomment-5782141316) reported one concurrent chat could enumerate/interact with another chat's process on the same authenticated device. Device-wide sharing may be intentional; requested conversation isolation is a stronger trust boundary. |

**Recurrence assessment:** independent reports motivate clear target and session ownership. The original wrong-target problem has positive fix evidence; conversation isolation is a separate product requirement.

**Proposed Devagotchi requirement:** bind an operation to authenticated principal, execution device, workspace, and operation identity at admission. Add conversation or agent scope only when authenticated provenance exists; caller-supplied labels must not manufacture identity. Explicitly define permitted sharing within one operator's trust domain. A graph node references the authoritative operation receipt; graph display does not acquire a second execution owner.

**Measurable acceptance:** with two devices and two concurrent clients, start distinguishable fixtures and verify target identity in each receipt. An offline or unknown requested target must never cause fallback. A handle cannot be redirected by changing device labels. Under a declared isolated-session policy, one session cannot read, feed, cancel, or approve the other's operation; under declared sharing, the UI must make that scope visible. Exercise ID collisions, stale generations, missing provenance, and reconnects.

## 8. Unrequested prompts, telemetry, and logs damage adoption trust

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#83: telemetry control](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/83) | 20 Apr 2025; `regismesquita`; **closed 2 May**, completed | Multiple participants requested choice and transparency. On 1 May, maintainer announced an opt-out, privacy policy, and random client identifier. The original missing-control complaint is historical; this ledger does not claim telemetry can never be disabled. |
| [#180: unwanted feedback content](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/180) | 16 Jul 2025; `jeremy-green`; **open** | `pandel` and `delano` independently objected. Maintainer [disabled feedback prompts in 0.2.6 that day](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/180#issuecomment-3079505052) after a broken disable mechanism. The issue title's malware accusation is not adopted as a factual conclusion. |
| [#256: tool-history logs mixed with agent output](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/256) | 9 Oct 2025; `tommy-waltmann`; **closed 24 Oct**, completed | A pydantic-ai user requested quiet output. Maintainer [reported the fix in 0.2.19](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/256#issuecomment-3444679915). This also demonstrates an actual non-Claude MCP integration. |
| [#543: onboarding/browser surprise](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/543) | 8 Jul 2026; `jecanore`; **open** | Reporter objected to browser opening and shipped onboarding instruction text. On [9 Jul](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/543#issuecomment-4924578674), maintainer said injection was remotely disabled, described a forthcoming fail-closed change and local disable controls, and said welcome-page opening was intentional. The reporter's auto-install attribution was disputed; it is not established here. |

**Recurrence assessment:** multiple independent authors across time and client types object to surprise behavior and output pollution. Several concrete defects were fixed or disabled. This is evidence for explicit product consent and clean integration boundaries, not a malware finding.

**Proposed Devagotchi requirement:** document network activity and default data handling before installation; keep promotion out of tool results and protocol streams. Put onboarding in an operator-visible UI. Separate optional diagnostics, update checks, and operational communication so each control has a precise meaning.

**Measurable acceptance:** capture outbound requests and protocol output through install, first launch, ordinary calls, error paths, and shutdown with optional collection disabled. Require zero optional collection calls, zero unsolicited browser launches, and zero promotional instructions in results. Validate the declared operational network allowlist and ensure diagnostic redaction under synthetic sensitive inputs. Test an SDK client as well as an interactive assistant.

## 9. Command refusal needs accurate parsing and an actionable explanation

| Primary evidence | Date, author, and current state | Reported experience and resolution limits |
|---|---|---|
| [#731: PowerShell literal content classified as a command](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/731) | 20 Sep 2026; `Justin-Time319`; **open**, updated 15 Oct | Reported rejected development work on 0.2.51, with parser-only analysis. Independent comments describe related literal-string cases. Later discussion distinguishes historical refusals from current-version parser checks. A proposed correction, [PR #792](https://github.com/wonderwhy-er/DesktopCommanderMCP/pull/792), remained open/unmerged. Do not describe parser replay as a successful live command or a verified deployed fix. |
| [#834: hashtable string classified as init](https://github.com/wonderwhy-er/DesktopCommanderMCP/issues/834) | 10 Oct 2026; `xyyxyyxx`; **open**, updated 15 Oct | Independent author reports a blocked private workflow and a sanitized parser-only explanation involving a descriptive string. The public example was not executed; actual admission-rule provenance and current deployed validator are not fully attested. The author asks for a supported correction/review path, not weaker restrictions. |

**Recurrence assessment:** independent reports support a false-positive/explainability theme. They do not establish every denial as incorrect or justify bypassing command policy.

**Proposed Devagotchi requirement:** make policy ownership and evaluated input explicit. Prefer exact executable/argv contracts where suitable. If shell execution is offered, its grammar and grant implications need separate treatment; an executable allowlist does not sandbox what an allowed executable can do. Return stable error categories, safe diagnostics, and a supported operator review path.

**Measurable acceptance:** parser-only fixtures must distinguish literal source text from executable syntax, with positive and negative controls for every supported shell. Real forbidden executable forms must remain denied. Unsupported/ambiguous syntax must fail visibly. An operator-approved request must be bound to its exact canonical content, scope, and expiry; changing content or target invalidates approval. Avoid publishing private rejected commands in diagnostics.

## Five themes suitable for a public product page

Use these as **design commitments or roadmap language until their acceptance evidence exists**. Keep the competitor evidence in a dated research note rather than turning resolved incidents into current accusations.

| Public theme | Safe wording | Evidence behind the requirement |
|---|---|---|
| Predictable startup | “A desktop bridge should start predictably and tell you when it is ready.” | Installation/initialize reports; native-host environment differences. Sections 1 and 3. |
| Clear execution ownership | “Every task should identify its workspace, execution device, and operation owner.” | Historical routing/affinity problems and continuing conversation-scope requests. Sections 2 and 7. |
| Honest status and recovery | “See whether a connection is reachable, whether work was accepted, and what remains uncertain.” | False-Online reports, recovery fixes, and corrected host-dispatch diagnoses. Sections 2 and 6. |
| Faithful, bounded results | “Get usable results through the API, MCP, and desktop UI, with explicit limits and completed resource cleanup.” | Cross-client content complaints and continuing Windows read-handle reports. Sections 4 and 5. |
| Integration on the operator's terms | “Keep optional collection explicit, tool output focused, and refusals understandable.” | Consent/output complaints, shipped mitigations, and parser false positives. Sections 8 and 9. |

## Architectural implication for a mesh and task graphs

The evidence supports preserving one authoritative operation owner while adding richer relationships and presentation. A future graph should connect requests, dependencies, approvals, operation receipts, output/artifacts, and verified outcomes. A mesh should route to an explicit authenticated owner and surface loss of reachability without silently relocating work. Neither a graph nor a mesh by itself provides process containment, transactionality across machines, exactly-once external effects, or conversation isolation.

Public API/MCP adoption should therefore be demonstrated with a small documented client, the same canonical operation identity through each adapter, and reproducible acceptance cases. A public API should not be described as already available merely because internal DTOs exist. Existing Devagotchi Linux correctness tests establish only the behavior and platform actually exercised; this complaint research supplies no comparative performance claim.
