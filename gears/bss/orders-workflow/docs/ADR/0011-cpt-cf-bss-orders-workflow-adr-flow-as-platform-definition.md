---
status: accepted
date: 2026-09-24
decision-makers: BSS Orders team (Architecture)
---
# ADR-0011: The Order Process Flow Is A Versioned Platform Workflow Definition Executed By serverless-runtime


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Code-defined flow (today)](#a-code-defined-flow-today)
  - [B. Policy points only](#b-policy-points-only)
  - [C. Orders-local interpreter](#c-orders-local-interpreter)
  - [D. Platform definition (chosen)](#d-platform-definition-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition`
## Context and Problem Statement

Until this decision the order process **flow** — the order of steps, the branches, the waits, the
two-wave barrier release, the listening for Lifecycle and Subscriptions events, the hold/resume/cancel
handling and the shape of compensation — was Rust code in this gear: a step executor, a durable
timer service over `owf_durable_timer`, a retry controller over `owf_retry_state`, an escalation
timer owner, an activation-barrier timer owner, a suspension controller and five advisory-locked
workers (`design/01-foundation.md` §3.8, D-62). Reordering two steps, lengthening a wait or adding an
escalation arm was an Orders release. The durable-execution substrate underneath that code was left
open as `DECISIONS.md` Q-01 (PRD §15, line 1183), and ADR-0001 as first written said so.

The platform has since decided the questions Q-01 was waiting on. The platform gear
`serverless-runtime` adopts the CNCF Serverless Workflow Specification v1.0.0 as its workflow
definition language
([serverless-runtime ADR-0003](../../../../serverless-runtime/docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md),
lines 74 and 79–81: chosen because definitions are validated at submission and modified through an
API without code deployments), executes those definitions on **Temporal** through a custom engine
layer inside a runtime plugin
([serverless-runtime ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md),
lines 90 and 94), and places every orchestration primitive — durability, timers, retry, event
matching, compensation, checkpointing — inside that plugin rather than in the host
([serverless-runtime ADR-0005](../../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md),
lines 78, 86 and 87; [DESIGN.md §1.1](../../../../serverless-runtime/docs/DESIGN.md#11-architectural-vision)
line 85; [§1.4.2](../../../../serverless-runtime/docs/DESIGN.md#142-plugin-model) line 172). The
host exposes a Function Registry at `/api/serverless-runtime/v1/functions` (register draft,
validate, publish, new version — [§3.3](../../../../serverless-runtime/docs/DESIGN.md#33-api-contracts)
line 857), an Invocation API with generic `:control` actions `cancel | suspend | resume | retry |
replay` and a `:plugin-control` passthrough (lines 865–869, 883–889, 893), event triggers (lines
980–987) and a per-invocation timeline (line 1061). In-flight invocations are pinned to the exact
callable version at start (line 614, BR-029).

Three constraints of this gear's PRD shape any use of that platform. `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth`
(PRD §6.1, lines 241–243) requires this gear to own all process-execution state at audit grade,
independently of engine history, and to record the definition version an instance started under.
PRD §15 (line 1183) requires the substrate evaluation to name which commercial data would sit in
engine history, and how that history is isolated and retained. PRD §5.2 (line 227) puts migration
of a running instance onto a later definition out of scope. The Lifecycle seam rules R1–R5 and the
closed trigger and event sets are unchanged.

Where does the flow live: in this gear's code, in a policy layer over that code, in an
Orders-owned interpreter, or in a platform definition — and if the last, what exactly moves and
what exactly stays?

## Decision Drivers

* The flow must be adjustable without an Orders release: sequencing, waits, escalation arms and branch shape are operational policy that changes more often than what a step does.
* The platform has selected the definition language and the engine (serverless-runtime ADR-0003, ADR-0004) and has refused to build a second orchestration substrate in the host (ADR-0005); an Orders-owned scheduler, timer wheel and retry engine would be the duplicate the platform declined to build.
* PRD §6.1: this gear owns the process record at audit grade independently of engine history. Whatever drives the flow, every step must land in Orders' own tables in Orders' own transaction, with the outbox enqueue of ADR-0008 in that same transaction.
* PRD §15 criteria: commercial data must not sit in engine history; history isolation, retention and residency must be assessable; gear-owned audit must survive an engine purge. Task inputs and outputs are what an engine persists, so their content is the lever.
* PRD §5.2: an instance is pinned to the definition version it started under; the platform pins in-flight invocations to the callable version (DESIGN.md line 614), and Orders must record the same binding on its side.
* Seam rules R1–R5: Lifecycle, Subscriptions, Payments and Generic Approval are called only from inside Orders code. A definition that called them directly would move seam authority into a document that a platform operator can publish.
* The closed sets stay PRD-owned: nine Lifecycle triggers in, six process events out, a closed registered reason catalogue. The definition may order the flow; it may not widen a set.
* Honesty: `gears/serverless-runtime/` contains documentation and a `gear.toml` and no crate — no `Cargo.toml`, no Rust source, no `plugins/<backend>-plugin/` directory (the layout ADR-0005 line 83 prescribes). The decision must state what holds if the platform slips.

## Considered Options

* **A. Code-defined flow (today)** — the flow stays Rust in this gear over `owf_durable_timer`, `owf_retry_state` and the five workers; adjustments are Orders releases
* **B. Policy points only** — the flow stays code; timeouts, retry curves, escalation windows and a few branch switches are externalised as configuration; sequencing cannot change without a release
* **C. Orders-local interpreter** — this gear defines its own flow document format and ships its own interpreter over its own timer, retry and worker tables
* **D. Platform definition (chosen)** — the flow is a versioned Serverless Workflow definition registered in serverless-runtime and executed by its Temporal plugin; this gear provides step operations and the process record

## Decision Outcome

Chosen option: **D, platform definition**, because it is the only option under which the flow is
adjustable without an Orders release *and* no second orchestration engine is built — in this gear
or anywhere — while the gear-owned record of PRD §6.1 is unchanged in ownership, transaction shape
and audit grade. Concretely:

* **The flow is a definition.** The order process is a Serverless Workflow definition (v1.0.0,
  per serverless-runtime ADR-0003) registered through the platform Function Registry as a
  `gts.cf.core.sless.workflow.v1~` callable and executed by the serverless-runtime Temporal plugin.
  The definition uses the grammar subset `call` (HTTP to a step operation; a registered Function
  only for a `composable` operation), `listen` (the nine Lifecycle triggers, the approval decision,
  the Subscriptions confirmation and failure events and Orders' own two terminal process events —
  event-broker GTS events; the last two are allowed in the closed set but unused by the canonical
  definition, whose overdue monitor stops with the invocation (`design/10-process-definition.md`
  §2.2 *The closed trigger set*) — and the four operator signals `cancel-requested`,
  `reauthorize-requested`, `task-resolution-requested`, `unpark-requested`; the closed set is
  ADR-0012 rule 3), `wait`, `switch`, `fork`,
  `try`/`catch`/`raise` and `set`. It does not use `run` (containers and scripts) and does not use
  `emit`: the six process events are produced by this gear's own producer outbox inside step
  operations (ADR-0008), never by the definition. `design/10-process-definition.md` is the
  normative home of the definition, its grammar subset and its canonical versions.
* **Orders provides step operations.** Every step the definition can order is a Rust operation in
  this gear, exposed on the internal surface `POST /bss-orders-workflow/v1/steps/{operation}`, one
  route per registered operation, callable only by the serverless-runtime service principal
  (`subject_type` service, `token_scopes` naming this gear) and authorized as the PDP resource
  `gts.cf.bss.orders_workflow.process_step.v1~` × action `execute` with the operation name as a
  resource property (ADR-0010 as amended). Each operation declares once, in its slice §3.3 and
  mirrored into the registry table `owf_step_operation`: `name`, `protection`
  (`protected | composable`), `input` and `output` (GTS reference schemas), the
  `idempotency_key` derivation (ADR-0006 families), `declared_event`, `compensation`, `reasons`,
  `audit_kind`, `retry_class` (`retryable-on: transient` or `never`) and `deadline`. The canonical
  operation names and their protection are fixed by ADR-0012.
* **Orders provides the process record.** `owf_process_instance`, `owf_step_log`, the idempotency
  registry, the audit chain (D-59…D-61), the saga/compensation log and the manual tasks remain this
  gear's tables. Every step operation writes its record in its own database transaction, with the
  audit entry, the idempotency settlement and — where the operation declares an event — the
  producer-outbox enqueue in that transaction (ADR-0008 unchanged). `owf_process_instance.phase` is
  a **recorded projection** written by step operations, never derived from engine state. A new
  table `owf_definition_binding` (`correlation_id` PK, `definition_id`, `definition_version`,
  `definition_source` ∈ `platform | code`, `pinned_at`, `published_by`, `resource_tenant_id`) is
  written by `start-instance`, and `owf_process_instance.definition_version` is a foreign key to
  it. `owf_step_log` records an attempt identity on every row: the definition-derived stand-in
  `$workflow.id + ":" + $task.reference` until the platform asserts an `attempt_id` on the call,
  which is an upstream ask (`design/01-foundation.md` §3.3 *Attempt identity*).
* **Timers, retry policy, waits and signals are the platform's.** `owf_durable_timer`,
  `owf_retry_state` and slice 08's `owf_timer_pause` are removed. The per-task retry policy (the
  DSL's `use.retries` policy referenced from a `try`'s `catch.retry`, Serverless Workflow DSL
  1.0.0, dsl-reference.md *Try*, *Retry*), the waits (expected-fulfillment, escalation, overdue, lifetime ceiling) and the hold/resume/cancel
  signals are expressed in the definition and executed by the plugin (DESIGN.md line 632: the
  plugin owns step identification, retry scheduling, checkpointing, suspend/resume and
  event-driven continuation). Hold and resume are the Lifecycle events `OrderHeld` and
  `OrderResumed`, consumed by a `listen` arm, never platform `suspend`/`resume` control actions,
  which would pause every `wait` including the lifetime ceiling (`design/10-process-definition.md`
  §4.4); cancel, re-authorisation and a task resolution are `:plugin-control` signals recorded in
  Orders before delivery. Each arm then calls `apply-hold`, `apply-resume`, `authorize-cancel` or
  the recording operation so that Orders records it. The rule "only approval-escalation waits
  pause on hold; the lifetime ceiling and the barrier keep running" is a definition pattern: the
  escalation re-check tick sits inside the arm a hold cancels, and after the resume the re-check
  continues against the deadline Orders re-based — the remainder stays on the gate row and is
  never returned to the definition, whose first answer after a resume is `apply-resume`'s `due`;
  the lifetime `wait` is at the top level. A
  1.0.0 `wait` takes only a fixed duration, never a runtime expression (dsl-reference.md *Wait*,
  *Duration*), so a deadline is re-checked by the bounded re-check loop of D-70 as amended — a
  fixed-granularity `wait`, a `call` to the operation that owns the deadline, a `switch` that
  loops while it answers `due: false` — and never by a Function that sleeps; whether the plugin
  accepts a runtime-expression duration as an extension is Q-11 (i).
* **Workers reduce to three.** `timer-wakeup` and `dead-lease-scan` are removed from the D-62
  roster; `reconciliation-sweep`, `retention-purge` and `audit/<tenant>` remain. A dead lease is
  detected by the sweep's status read, which the definition drives through a `wait`/retry rather
  than an Orders timer.
* **Bounds nest, and the nesting is validated.** Per-operation deadline < task retry budget <
  task timeout < overdue window < lifetime ceiling (D-02, D-53, D-67, D-70). The ordering is a validation rule
  of ADR-0012, not a convention.
* **Engine history is reference-only and non-authoritative.** Task inputs and outputs carry
  identifiers and small enums only, and the start trigger and every `listen` must keep only
  references of what they consume (ADR-0013, whose residual names the consumed events until the
  member-storage or thin-event ask lands). Nothing in this gear reads the platform timeline
  (`GET …/invocations/{id}/timeline`, DESIGN.md line 1061) as process state, audit or operator
  progress; ADR-0001 as rewritten states the record side of the same rule.

### Consequences

**Responsibility split.**

| Concern | Owner after this decision | Where it is stated |
|---|---|---|
| Step sequencing, branching (`switch`), parallelism (`fork`) | Platform definition, executed by the Temporal plugin | `design/10-process-definition.md` §2 |
| Timers and waits (expected-fulfillment, escalation, overdue, lifetime ceiling) | Platform definition (`wait`), plugin-native timers | `10` §2; D-02, D-53 as nesting bounds |
| Per-task retry policy and attempt scheduling | Platform definition (`use.retries` referenced from `catch.retry`), executed by the plugin; not the platform `RetryPolicy`, which is invocation-level by SDK error category (DESIGN.md lines 354–370). Orders declares `retry_class` per operation | `owf_step_operation.retry_class`; ADR-0006 as amended; D-70 as amended |
| Event listening (nine triggers, approval decision, Subscriptions confirmations) | Platform definition (`listen`) over the platform event-trigger path | `10` §2; ADR-0009 as amended |
| Hold / resume events; cancel / re-authorisation / task-resolution signals | Lifecycle events over the platform event-trigger path, and `:plugin-control` signals, → definition `listen` arm; never `:control` `suspend`/`resume` | `10` §3.3, §4.4; ADR-0007 as amended; Q-11 |
| Compensation **structure** (`try`/`catch`) | Platform definition | ADR-0005 as amended |
| What a step does; every seam call (R1–R5) | Orders step operations | each slice §3.3 |
| Process record, phase projection, definition binding | Orders (`owf_process_instance`, `owf_step_log`, `owf_definition_binding`) | `design/01-foundation.md` |
| Idempotency keys and registry | Orders (ADR-0006) | `01`; ADR-0006 as amended |
| Audit chain | Orders (D-59…D-61) | `01` |
| Process events and their outbox | Orders (ADR-0008, D-58) | `01`; ADR-0008 |
| Authorization of step routes and of everything Orders does inside them | Orders through the platform PDP (ADR-0010 as amended) | `09` |
| Authorization of definition publish, invocation start and control | Platform (DESIGN.md line 847) | platform |
| Refusal reasons | Orders' closed catalogue (D-64) | `01` §4.9 |
| Compensation **ordinal** and the reverse walk | Orders (`compensate-order`, one operation) | ADR-0005 as amended |
| Manual tasks and incidents | Orders | `07`; ADR-0009 |

**Adjustability contract.** The following change by publishing a new definition version and
require **no Orders release**: the relative order of `composable` operations; insertion or removal
of a `composable` operation from the path; the duration of any `wait` and the retry policy of any
task, inside the nesting bounds and the operation's `retry_class`; the shape of escalation arms
(which manual task or escalation operation an expired wait calls); branch predicates that read
only reference fields and enums the operations return; which `listen` event, from the closed
trigger set, gates which arm; the `fork` shape over `composable` operations. The following are
**Orders releases**, because they change what a step does or what may cross the boundary: the
behaviour of any operation; a new operation; the `protected` list and its order constraints; any
operation's input or output schema; the reason catalogue; the six-event set and the nine-trigger
set; the seam calls; the idempotency families; audit kinds; the record tables; the PDP catalogue.
A definition cannot make Orders do anything an operation does not already do.

**Fallback property.** If the platform readiness gate below does not pass, or if the Q-01
evaluation in PRD §15 fails on the platform asks, the process record and the step operations are
unchanged: the same tables, the same transactions, the same audit chain, the same idempotency
families, the same PDP catalogue, the same reasons. Only sequencing falls back to code — a
sequencer in this gear calling the same operations in the same order the canonical definition
states, binding the instance with `definition_source = code`. This is stated as a property of the
decomposition, not as a plan: nothing in the record or the operations depends on which sequencer
called them, and `owf_definition_binding.definition_source` records which one did.

**Runtime gate.** No serverless-runtime host crate, no Temporal plugin and no SDK crate exists in
this repository today: `gears/serverless-runtime/` holds `docs/`, `gear.toml`,
`serverless-runtime/docs/` and `serverless-sdk/docs/` and nothing else. The canonical definitions
are documentation until the platform host, its Temporal plugin and the platform asks below exist
and the readiness gate passes. This gear cannot be ready for order traffic on the platform path
before then; the fallback property is what holds in the meantime.

**Q-01 is answered in two parts.**

1. *The substrate choice is made.* The durable-execution substrate is the serverless-runtime
   Temporal plugin executing a platform definition; not the OSS Workflow Engine of
   `PRD-workflow-engine-202501051430` and not a BSS-local mechanism. ADR-0001 is rewritten
   accordingly and no longer says it selects no engine.
2. *The PRD §15 evaluation of engine-history isolation, retention and residency is pending on the
   platform asks.* ADR-0013 bounds what can be in history to references, which answers "which
   commercial data would sit in engine history" with "none" for task inputs and outputs; for the
   start trigger's input and the consumed events the answer is "none" only once the platform
   stores selected members or Lifecycle publishes thin events, and until then those events are
   ADR-0013's stated residual. Isolation, retention and residency of
   the history that remains are platform properties this gear cannot assert from the platform's
   documents today; they are raised as upstream asks in `UPSTREAM_REQS.md` under the
   serverless-runtime section, and Q-01 closes fully when they are agreed.

**Platform asks this decision depends on** (recorded in `UPSTREAM_REQS.md`, serverless-runtime
section; stated here as asks because no platform document states them as facts):

* **Execution identity on outbound `call` tasks.** A triggered execution's identity is already a
  trigger field, `execution_context: system | event_source`
  ([DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) line 1697),
  and the start triggers take `system`. What is asked is how that identity is presented on the
  plugin's outbound HTTP call: `POST /bss-orders-workflow/v1/steps/{operation}` as the
  serverless-runtime service principal with a token whose `token_scopes` name this gear, refreshed
  across a long-running invocation. The platform lists the execution-identity model as an
  unaddressed blocker
  ([NEXT_ADR_SCOPE.md](../../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) lines 15, 96–97,
  BR-006, BR-013).
* **Named signals to a running invocation.** Hold, resume, cancel and re-authorisation must reach a
  `listen` arm as distinguishable signals; the platform has generic `suspend`/`resume`/`cancel`
  (DESIGN.md lines 885–888) and a plugin-control passthrough (line 893) but records "no signal
  delivery model" (NEXT_ADR_SCOPE.md line 40, BR-108). The same ask covers the operator re-drive
  (D-86 as amended): `retry` keeping `invocation_id` from `failed` (line 888) and also from
  `dead_lettered`, where a failure without an `on_failure` handler lands (line 458), and one stated
  path for the verb, which the platform describes both as host-executed (line 873) and as routed
  to the plugin (line 893).
* **Event-trigger binding to the platform event broker.** `listen` targets are event-broker GTS
  events; the platform's event-broker integration is "TBD per deployment" (DESIGN.md line 150) and
  event matching is plugin-native (line 808).
* **Member-only storage of trigger inputs and consumed events**, per ADR-0013: the plugin persists
  only the members the definition selects from the start trigger's input and every consumed
  event; the alternative route is a Lifecycle ask for thin event variants.
* **Attempt identity and deadline on each outbound call.** The SDK `Context` already carries
  `attempt_number` and a `deadline` with `remaining_time()`
  ([serverless-sdk DESIGN.md](../../../../serverless-runtime/serverless-sdk/docs/DESIGN.md) lines
  123, 303, 307); the ask is that a `call: http` task carries them to the callee, and that the
  DSL's `$workflow.id` is the platform `invocation_id`, which `start-instance` binds.
* **No tenant cap on suspension below the lifetime ceiling.** The Workflow declares
  `workflow_traits.max_suspension_days: 90`, the required field whose default is 30
  ([DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) lines
  520–529), for the 90-day `max_process_lifetime` (D-53); what is asked is whether a tenant policy
  may cap it lower — the platform commits to at least 30 days and a tenant-configurable maximum
  ([serverless-runtime PRD](../../../../serverless-runtime/docs/PRD.md) line 404, BR-009) — and a
  field for the async-only declaration DESIGN.md line 653 says `workflow_traits` SHOULD carry but
  its schema (DESIGN_GTS_SCHEMAS.md lines 466–530) does not.
* **Engine-history isolation, retention and residency**, per ADR-0013 and Q-01 part 2: Temporal
  Server's persistence backend is a platform infrastructure dependency (serverless-runtime
  ADR-0004 line 100) whose location and retention this gear cannot pin.
* **A consumer-registered pre-publish validation hook and audit of publishes**, per ADR-0012.

### Confirmation

Verified by: a CI test that parses every canonical definition against the Serverless Workflow
v1.0.0 schema and against the ADR-0012 validation rules, and fails on an unregistered `call`
target, a `listen` target outside the closed trigger set, a missing or misordered `protected`
operation, a non-nesting bound or a non-reference task input; a startup assertion that every
route under `/bss-orders-workflow/v1/steps/` maps to exactly one `owf_step_operation` row and
every row to exactly one route; a test that a step route refuses any caller that is not the
serverless-runtime service principal before the PDP is asked, and that the PDP is asked for
`process_step × execute` with the operation name; a structural test that no code path in this
gear schedules a timer, computes a retry delay or reads the platform timeline; a test that
invoking one operation twice with the same inputs settles one idempotency key and writes one
audit entry (ADR-0006 as amended); a fault-injection test that kills the operation between its
committed transaction and its HTTP response and asserts the platform's re-invocation is absorbed
as a duplicate; a test that an instance started under definition version *n* completes under *n*
after version *n+1* is published; a test that the record and audit chain of a completed instance
are complete and verifiable with the engine's history absent; and — once the plugin exists — an
end-to-end run of each canonical definition against the serverless-runtime Temporal plugin with
the record asserted after every operation. The fallback property is confirmed by running the same
operation-level tests under the code sequencer with `definition_source = code`.

## Pros and Cons of the Options

### A. Code-defined flow (today)

* Good, because every guard, wait and retry is Rust under this gear's tests, and the record and the sequencer share a process.
* Bad, because changing a wait or reordering two steps is a release of a `p1` order-taking service.
* Bad, because it keeps a durable timer service, a retry engine and two scheduling workers in this gear — the orchestration substrate the platform decided (ADR-0005) not to build twice.
* Bad, because it leaves Q-01 open and every slice's timer, retry and history-isolation design provisional (DESIGN.md §4.9).

### B. Policy points only

* Good, because it is the smallest change: the flow stays code and a few numbers move to configuration.
* Bad, because the adjustments that matter operationally — an extra escalation arm, a different listen-before-dispatch order, a rebuilt barrier — are sequencing, which configuration cannot express.
* Bad, because it still owns the timer, retry and worker machinery of option A, so it inherits A's duplication and A's open Q-01.

### C. Orders-local interpreter

* Good, because the format and the interpreter would fit this gear exactly, with no platform dependency and no readiness gate.
* Bad, because it is a second workflow engine — a DSL, a validator, a scheduler, timers, signals and replay — owned by a business gear, which is the duplication the platform explicitly refused in its own host (ADR-0005 line 78) and which no sibling gear carries.
* Bad, because an Orders-local format has no platform registry, versioning, publish governance or tooling, so every one of those would be built here too.

### D. Platform definition (chosen)

* Good, because sequencing, waits, retry, signals and event listening are the platform's, adjustable by publishing a version, and this gear keeps exactly the parts PRD §6.1 requires it to own.
* Good, because it is the platform's stated model — a definition validated at submission and executed by a plugin — rather than an exception to it.
* Good, because the split is testable at the boundary: operations are HTTP routes with declared inputs, and the record is asserted after each one regardless of who called it.
* Neutral, because compensation structure moves into the definition while the ordinal-driven walk stays one Orders operation; the two halves are held together by ADR-0012's validation rules.
* Bad, because none of the platform code exists yet, so the decision ships as documentation behind a readiness gate and depends on the asks above.
* Bad, because task inputs and outputs, even as references, live in a Temporal history this gear does not own; ADR-0013 bounds the content and the residency ask bounds the location, but neither makes the history this gear's.

## More Information

This decision answers `DECISIONS.md` Q-01 in the two parts stated above; the register entries that
carry it are numbered from D-65 onward in `DECISIONS.md`, and Q-10 (a seller-scoped publish role)
and Q-11 (whether the plugin's DSL expresses the constructs the definition needs — a
runtime-expression `wait` duration as an extension, `error_code` on `$error`, a dynamic parallel
construct, a cancellable `listen` in a competing `fork`, the hold pattern — or they need
fallbacks) are the questions it opens. It
rewrites ADR-0001, is refined by ADR-0012 (definition versioning and protected steps) and ADR-0013
(references, not payloads), and amends ADR-0002, 0003, 0004, 0005, 0006, 0007, 0009 and 0010 with
dated notes; ADR-0008 is unchanged. Everything decided in the platform-alignment commits stays:
the platform producer outbox (ADR-0008, D-58), gear-owned audit on the frozen hash contract
(D-59…D-61), PDP authorization through the shared PolicyEnforcer adapter (ADR-0010, D-63),
ContractError reasons (D-64) and toolkit-db advisory locks for the remaining workers (D-62 as
amended). Serverless-runtime is cited, not restated: its DSL, engine and host/plugin boundary are
its ADR-0003, ADR-0004 and ADR-0005, and its API surface is its DESIGN.md §3.3.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — §5.2 (no migration of a running instance), §6.1, §7.1, §13
  (durable execution infrastructure), §15 Q-01 (line 1183)
- **DESIGN**: [DESIGN.md](../DESIGN.md) §1, §2.2, §3.4, §4.2, §4.3, §4.9;
  [`design/10-process-definition.md`](../design/10-process-definition.md);
  [`design/01-foundation.md`](../design/01-foundation.md) §3.7, §3.8
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — Q-01 (answered in two parts), D-02,
  D-53, D-62 (amended), D-65, D-69, D-70 (as amended), D-86 (as amended), Q-10, Q-11
- **Upstream asks**: [`UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) — serverless-runtime section
- **Platform**: serverless-runtime
  [ADR-0003](../../../../serverless-runtime/docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md),
  [ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md),
  [ADR-0005](../../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md),
  [DESIGN.md](../../../../serverless-runtime/docs/DESIGN.md) §1.1, §1.4, §3.1, §3.3

This decision directly addresses the following requirements or design elements:

* `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` — the platform drives and Orders records: every step operation writes the gear-owned record in its own transaction, the definition version is pinned in `owf_definition_binding`, and engine history is reference-only and never read as process state
* `cpt-cf-bss-orders-workflow-fr-owf-retry` — retry scheduling is the definition's per-task retry (`use.retries` from `catch.retry`), executed by the plugin; the operation's `retry_class` and idempotency key make a platform re-invocation an absorbed duplicate rather than a second effect
* `cpt-cf-bss-orders-workflow-fr-owf-hold-resume` — hold and resume are signals handled by a definition arm that calls `apply-hold`/`apply-resume`; the pausable-escalation-only rule is a definition pattern
* `cpt-cf-bss-orders-workflow-nfr-owf-durability` — zero loss for committed steps is asserted against the gear-owned record, which no sequencer change touches
* `cpt-cf-bss-orders-workflow-nfr-owf-audit` — the audit entry is written by the operation in the transaction that makes the step true, independent of engine history
* `cpt-cf-bss-orders-workflow-component-foundation` (**step executor**, the component `design/01-foundation.md` §3.2 declares under its unchanged identifier) — becomes the family of step operations behind `/bss-orders-workflow/v1/steps/{operation}`; the sequencing role it held moves to the definition
* `cpt-cf-bss-orders-workflow-component-foundation` (**durable timer service** and **retry backoff controller**) — retired by this decision; their responsibilities are the definition's `wait` and per-task retry (`use.retries` from `catch.retry`), executed by the plugin
* `cpt-cf-bss-orders-workflow-adr-durable-execution-substrate` — rewritten by this decision: the substrate is now selected
