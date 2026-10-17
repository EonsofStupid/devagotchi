# Devagotchi task graphs, mesh, and public contracts

Revision: **2026-10-17 UTC**. Status: **proposed contract and execution plan; runtime implementation pending**.

## 1. Intent, evidence, and ownership

The user has expanded Devagotchi into a universal workspace bridge: publicly adoptable through a website, API, and MCP; local first; able to grow into an internal mesh; with every admitted task represented as a graph. This extends the [architecture master](product-and-platform-plan.md). The master owns product decisions and sequencing; this document owns the candidate graph/mesh contract. The [performance specification](performance-and-experience-spec.md) continues to own measurement definitions.

The recovered foundation implements Linux process operations, a SQLite journal, typed local IPC, four stdio MCP tools, and a real desktop inspector. It implements neither task graphs nor mesh execution nor a public workstation endpoint. A source search found no existing graph scheduler or task/step model. The intended GitHub repository remains uninspected after a connector `404`; RRFlow and other ecosystem implementations have not been inspected and are not assumed dependencies. No new performance results accompany this document.

Extend the existing owners:

| Responsibility | Existing owner | Extension |
|---|---|---|
| Admission, scheduling, cancellation, recovery | `Broker`/`Shared` in `src/execution.rs`; proposed relocation to `src/broker.rs` in the master | Graph readiness and task aggregation inside this owner; attempts dispatch existing operations. |
| Durable facts and queries | `src/journal.rs` | Task/step/attempt records, dependency indexes, bounded change records, and later delivery records in the same database. |
| Types and validation | `src/contracts.rs` | Versioned graph DTOs; generate schemas and TypeScript through the existing pipeline. |
| Native work | `src/execution.rs`, planned file modules | Execute admitted attempts; retain native process, file, terminal, and settlement guarantees. |
| Access and communication | Existing broker grants, `src/ipc.rs`, MCP adapter | Add authenticated subjects and peer routing where required; adapters map the same broker contract. |
| Visibility | Existing inspector/data owner | Task summary, expandable graph, attempts, output, authority, and freshness. |

No second scheduler service, graph database, universal provider registry, or mesh authority is introduced. A later `broker/graph` module is organization within the existing owner, justified by actual code size.

## 2. Three graphs with different meanings

An **execution graph** is a bounded DAG of logical steps for one admitted task revision. Dependencies determine readiness; typed output bindings determine data flow. A single command is a one-node graph. Observational reads such as getting a task do not recursively create tasks or require durable jobs.

A **provenance graph** links task, step, attempt, operation, output, and change facts. Retries create new attempt nodes and retain old evidence. References can connect tasks without making those links executable dependencies.

A **mesh topology graph** describes trusted devices, advertised capabilities, workspaces, and routes. It may contain cycles. A topology edge establishes neither execution order nor authority. Route discovery must never turn into an implicit execution grant.

The first execution contract rejects cycles and freezes the definition at admission. Replanning creates an explicit new revision or child task; it does not mutate already-admitted inputs. Repetition expands into bounded new steps/attempts. Arbitrary loops and automatic distributed ownership transfer are later proposals, not prerequisites.

## 3. Candidate records and bounds

The following notation defines proposed fields, not a second implementation DTO source. The eventual Rust types remain authoritative. `Id` is an opaque string allocated by its record owner; submitted step IDs are validated task-local labels. Paths, PIDs, hostnames, client names, and transport request IDs are not interchangeable identities. Authenticated subject fields are broker-populated.

```text
TaskSubmit { retry_context: string, idempotency_key: string, spec: TaskSpec }
TaskSpec {
  schema_version: 1, steps: StepSpec[],
  limits: { queue_wait_ms, task_budget_ms, max_parallel },
  failure_policy: "continue_independent" | "fail_fast"
}
StepSpec {
  step_id: Id, needs: Id[],
  target: { device_id: Id, workspace_id: Id },
  operation: { kind: string, version: integer, input: object },
  bindings: { input_field: string, from: { step_id: Id, output_name: string } }[],
  retry: { max_attempts: integer, backoff_ms: integer }
}
TaskRecord {
  task_id: Id, spec_revision: integer, spec_digest: string,
  owner_device_id: Id, owner_epoch: integer,
  subject_ref: { issuer: string, subject: string },
  revision: integer, phase: "accepted" | "running" | "blocked" | "settled",
  outcome: "pending" | "succeeded" | "failed" | "cancelled" | "unknown",
  cancellation_requested: boolean, resource_settlement: Settlement,
  created_at_ms: integer, updated_at_ms: integer
}
StepRecord {
  task_id: Id, step_id: Id, revision: integer,
  state: "waiting" | "ready" | "active" | "succeeded" | "failed" |
         "cancelled" | "blocked" | "skipped" | "unknown",
  selected_attempt_id: Id | null, blocked_by: Id[]
}
AttemptRecord {
  attempt_id: Id, task_id: Id, step_id: Id, attempt_number: integer,
  operation_ref: { device_id: Id, operation_id: Id } | null,
  dispatch_id: Id, owner_epoch: integer, input_digest: string,
  resolved_input_ref: Id, bound_output_refs: Id[], grant_revision: integer,
  phase: "reserved" | "dispatching" | "admitted" | "running" | "settling" | "settled",
  outcome: "pending" | "succeeded" | "failed" | "cancelled" | "unknown",
  resource_settlement: Settlement, output_complete: boolean | null,
  revision: integer
}
Settlement = "active" | "settling" | "settled" | "unknown"
OutputRecord {
  output_id: Id, producer_attempt_id: Id, name: string,
  schema_ref: string, media_type: string,
  value: { kind: "inline", data: JSONValue } |
         { kind: "artifact", resource_ref: Id } |
         { kind: "stream", operation_ref: { device_id: Id, operation_id: Id } },
  sealed: boolean, byte_length: integer | null, digest: string | null,
  availability: "retained" | "partial" | "expired" | "unavailable"
}
ChangeRecord {
  event_id: Id, origin_device_id: Id, origin_sequence: integer,
  task_id: Id, step_id: Id | null, attempt_id: Id | null,
  operation_id: Id | null, record_revision: integer,
  kind: string, causation_id: Id | null, payload_ref: Id,
  recorded_at_ms: integer
}
```

Candidate initial limits are 64 steps, 128 dependency edges, 48 KiB serialized definition, four parallel attempts per task, one attempt by default and at most five by explicit policy. Broker-wide quotas remain authoritative. These are proposed admission bounds, not measured capacity claims. Inline output is bounded; large bytes remain in existing output storage or a later journal-owned artifact store. The total wire envelope must remain within its advertised bound.

Step kinds enumerate implemented, versioned operations; the initial runnable kind is ordinary process execution. File and terminal kinds are advertised only after implementation. No arbitrary expression, URL fetch, or tool-name forwarding language is implied by `input` or `bindings`. Admission validates identifiers, DAG order, binding types, capability availability, budgets, and authority before effects. At dispatch, bind each input to an exact successful producer attempt/output and record the resolved input digest. Later retries cannot silently replace those inputs.

A process attempt preserves the current operation ID and output sequences. Successful execution means its declared success predicate was observed; exit code zero alone does not verify every external effect. Initial bindings require sealed outputs; streaming dependencies are not implicit. Output completion, retained availability, result certainty, and resource settlement remain separate. Wall-clock timestamps describe time; revisions order updates.

## 4. Lifecycle, cancellation, and retries

Persist the task definition and initial readiness before acknowledging admission. A step becomes ready only when required dependencies succeeded and bound outputs are available and authorized. The scheduler reserves an attempt durably before dispatch. An admitted operation then belongs to its execution broker independently of HTTP, MCP, or inspector waits.

Default failure policy is `continue_independent`: a failed required dependency blocks its dependents while independent branches continue. The final task reports failure and explains blocked steps. `fail_fast` stops new ordinary dispatch and explicitly requests cancellation of admitted attempts. A task is not reported settled while owned resources remain active or their settlement is unknown. An unknown outcome can coexist with ongoing cleanup.

Task cancellation records durable intent, prevents new ordinary dispatch, cancels queued work, and sends explicit cancellation to admitted attempts. Report local intent, delivery, target acknowledgement, and observed settlement separately. A late completion can win a cancellation race; the journal records the actual result. Closing an observer cancels its wait, not the graph. Predeclared cleanup steps are separate authorized actions; cancellation does not invent compensating commands or universal rollback.

Three retry cases have different meanings:

1. **Lost response or transport redelivery:** reuse the same dispatch identity, validated retry context, and semantic digest; return the existing admission or a conflict. Never create another attempt simply because an acknowledgement is missing.
2. **Known retryable outcome:** create a new numbered attempt under the recorded policy. Automatic retries require broker-established evidence of no effects, or a qualified idempotent/reconcilable operation. A model-provided `safe_to_retry` flag and a nonzero exit code are insufficient.
3. **Unknown effects or ownership:** block automatic repetition and reconcile. An operator can authorize a new attempt with the ambiguity visible; the old evidence remains.

Queue waiting, task budget, and each attempt's execution deadline have declared start points. Authority and pinned environment/profile versions are checked again before dispatch and file commit. Cancellation, expiry, or revocation racing a ready step must prevent unauthorized launch. Retention or concurrency tuning does not alter semantic retry identity.

## 5. Journal, events, and graph observation

Keep authoritative records and compact committed change facts in the existing SQLite journal. Commit a state transition, its dependency/readiness consequences, and its change watermark atomically where they share the local owner. Do not replay executable effects from an event log. Separate record versions, task/operation revisions, local change watermarks, and per-operation output cursors.

Serve bounded task summaries, paged steps/attempts, and explicit graph expansion. Snapshot plus watermark must be consistent. Expired event history returns `resync_required`; expired output returns declared ranges/availability. Slow graph observers cannot stall storage or pipe drainage. Bound dependency fan-out, ready queues, event payloads, subscriptions, cached graphs, and artifact transfers. Reuse the master's physical disk, writer-failure, and retry-evidence policies.

The inspector initially shows a readable task row and expandable dependencies, not an obligatory full graph canvas. Selecting an attempt opens the existing operation detail/output view. Show failure cause, blocked dependencies, attempt history, exact target/workspace, authority, output retention, and stale/offline evidence. Never render a sample graph as live activity.

## 6. Mesh: route between existing owners

The local task owner is an existing broker. A task keeps one fixed owner device in the initial mesh contract. Each execution broker is authoritative for its admitted operation, local workspace grants, and native settlement facts. The task owner aggregates those facts and schedules dependencies; it cannot override a target grant. No replicated multi-writer task authority or automatic owner failover is proposed initially.

Device identity is a persisted key binding, not a supplied device ID or hostname. Pairing records trusted peer keys, permitted subjects/workspaces/capabilities, and revocation state under the existing broker configuration/journal owner. Initial routing uses explicit trusted peers and authenticated encrypted connections. A route advertisement is versioned, bounded, and freshness-labelled; it is not proof that an operation is running. Workspace/resource references include their owning device. File transport is an explicit authorized transfer with size, integrity, retention, and destination checks; a remote pathname does not imply a shared filesystem.

Candidate dispatch envelope:

```text
Dispatch {
  dispatch_id, task_id, step_id, attempt_id,
  task_owner_device_id, owner_epoch, target_device_id, workspace_id,
  retry_context, input_digest, operation,
  delegation_ref, admission_context, remaining_budget_ms
}
```

Identity and delegation are authenticated outside caller-controlled attribution fields. At the target, authorize the subject, audience, target, workspace, operation, expiry, and current grant before admission. Persist the inbox binding before effects. The sender records an outbox item before transmission. Acknowledgement links to the target operation; authenticated result facts retain origin and causation. Do not reuse a token intended for the public website or another broker as a workstation grant.

The target-issued admission context has bounded validity checked at that target; redelivery does not extend it. A sender's remaining duration is a budget cap, not proof of freshness across clock skew or delayed delivery. A local cancellation cannot instantly revoke an already-transmitted remote admission: show pending delivery until the target processes it or the context expires, then reconcile possible effects.

`owner_epoch` fences new stale dispatches: a target records the highest accepted epoch for a task owner and rejects older scheduling commands. Restart must reconcile outstanding attempts before issuing new ones. Fencing does not stop a previously admitted process or undo an external effect. Retry evidence and epoch state must survive the stated retention window; reclaiming them requires expired contexts to fail closed. Key rotation or owner transfer needs explicit authenticated state migration, not hostname reassignment.

During a partition, admitted work follows its recorded local policy. New remote steps wait or expire; unreachable attempts are marked stale/unknown, not reassigned. Cancellation remains pending delivery until acknowledged and settled. Restoring connectivity deduplicates delivery and reconciles origin facts. Local-only tasks remain usable without a mesh coordinator. Routing cycles are bounded with hop limits and duplicate detection; routing cannot expand the delegation. There is no exactly-once promise across two journals, partitions, and arbitrary side effects.

## 7. Public reference, runtime API, and MCP

The website's **public reference API and MCP** expose real read-only capability information, documentation/local tool schemas, and a labelled illustrative graph. They receive no workstation credentials and cannot invoke `operation_start`, access local paths, alter grants, or contact a desktop broker. Publishing a schema is not publishing its execution endpoint. Reuse the existing Site implementation and its components; this document does not create a competing web application.

The future **runtime API** maps typed task submission/get/list/cancel and paged graph/event/output reads to the same broker. Submission returns an admitted ID; deletion is not cancellation. HTTP, local IPC, and MCP must share semantic validation and retry scope. Generate runtime documentation from canonical Rust schemas and mark proposed versus callable methods. Public reference examples never claim runtime availability.

Use an explicitly **2026-07-28-only** profile for the initial Site MCP. Standard per-request metadata includes required protocol version and client capabilities; client information is optional. `server/discover` is required, successful results carry `resultType: "complete"`, and request IDs are strings or integers, never null. Self-reported client/server identity is not authentication. [MCP core](https://modelcontextprotocol.io/specification/2026-07-28/basic), [discovery](https://modelcontextprotocol.io/specification/2026-07-28/server/discover).

The HTTP binding requires POST, matching protocol/method/name headers where applicable, valid Origin policy, and JSON or request-scoped SSE responses. The short reference tools can always return JSON. Header errors use HTTP 400 / `-32020`; unsupported versions use HTTP 400 / `-32022` with `{supported, requested}`; unknown methods use HTTP 404 / `-32601`. Modern GET/DELETE return 405; protocol sessions and `Last-Event-ID` resumption are absent. [HTTP binding](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http), [error schema](https://modelcontextprotocol.io/specification/2026-07-28/schema#unsupportedprotocolversionerror).

For authenticated runtime HTTP MCP, implement protected-resource discovery, appropriate OAuth authorization, per-request bearer validation, intended audience, expiry, and scopes; target broker grants remain necessary. Current MCP authorization recommends Client ID Metadata Documents and retains deprecated dynamic registration for compatibility. No OAuth flow is necessary merely to publish anonymous public documentation. Do not pass tokens through to unrelated resources. [MCP authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization).

Devagotchi task graphs are independent of the optional **MCP Tasks extension**. Ordinary typed tools can return a durable Devagotchi task ID immediately. An extension adapter is added only after verifying SDK and named-host support; its handle maps to existing graph state, not another job engine. Extension cancellation is cooperative. The current `rmcp` 3.5.1 stdio feature set and historical protocol probes establish neither HTTP nor Tasks-extension certification. [Tasks extension](https://modelcontextprotocol.io/extensions/tasks/overview), [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools).

## 8. Migration and initial implementation packets

Pre-release status permits clean versioned contracts, not silent reinterpretation of saved effects. Keep the historical v1 fixture immutable. A migration either maps an old operation to stable single-step/attempt identities while preserving retry evidence, or exposes a clearly historical read-only view. Unsupported record/wire versions fail before effects. Do not add speculative legacy protocol shims; preserve the evidence needed to prevent accidental re-execution.

| Packet | Scoped paths/owners | Concrete exit |
|---|---|---|
| Reference publication | Existing Site only | Actual read-only HTTP/MCP requests pass; examples and Linux/runtime limits remain explicit. |
| Local graph records | `src/contracts.rs`, `src/journal.rs`, existing schema generator | Bounded single-step/DAG schemas, stored versions, migration fixtures, atomic admission and paged queries. |
| Local graph dispatch | Existing broker owner and `src/execution.rs` | Attempts reuse real operations; dependency, cancel, retry, fairness, and crash semantics pass. |
| Graph inspection | `src/ipc.rs`, existing Tauri host/data owner/views | Snapshot/resume and expanded attempt details use real data with bounded UI costs. |
| Authenticated adoption | Existing adapter/typed client plus a thin HTTP binding | Named client, auth, header/version, request-abandonment, and capability tests pass. |
| Two-broker mesh | Existing grant/config, journal, and broker routing owners | Pairing, durable handoff, partitions, fencing, cancellation delivery, and artifact authorization pass. |

## 9. Acceptance and adversarial gates

- Reject duplicate IDs, missing bindings, cycles, unsupported step kinds, oversized graphs, and unauthorized cross-workspace references before effects.
- Retry a lost admission response repeatedly: one attempt/operation binding. Conflicting input under that identity fails. Expired retry context never becomes new execution.
- Crash before/after every admission, dispatch, target acknowledgement, state commit, and final-output boundary; preserve uncertainty and never replay unknown effects.
- Race cancellation/revocation/expiry against readiness and dispatch; forbid new unauthorized launch and report real settlement, including escaped native descendants.
- Partition two brokers after target admission but before acknowledgement; no alternate-device execution. Deliver delayed old-epoch commands after reconnect and verify fencing.
- Duplicate/reorder peer events, forge source/device attribution, rotate/revoke keys, and replay old delegation: reject unauthorized actions without deleting valid evidence.
- Fail one branch, lose an output artifact, and retry a producer: dependents use only their recorded successful binding or remain blocked; no silent rebinding.
- Saturate graph/output/history traffic while cancelling and reading. Extend existing workloads with 1/8/64-step DAGs, bounded fan-out, multiple observers, and offline peers; report failed attempts and process-family costs.
- Exercise malformed modern MCP metadata, optional clientInfo absence, header mismatch/Base64 encoding, invalid Origin, unsupported version/method, oversized body, and attempted runtime calls against the public reference service.
- Verify actual Windows/macOS/Linux runtime and named-client paths before claiming them. Graph visuals, a published website, or a successful compile establish none of those guarantees.

These packets extend the master plan's owners and correctness/performance gates. Future details remain proposed until their packet; they do not block useful local or public-reference work.
