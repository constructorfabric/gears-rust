---
status: accepted
date: 2026-09-24
decision-makers: BSS Orders team (Architecture)
---
# ADR-0013: References, Not Payloads, Cross The Engine Boundary


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Payload passing](#payload-passing)
  - [Encrypted payloads](#encrypted-payloads)
  - [References (chosen)](#references-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-workflow-adr-references-not-payloads`
## Context and Problem Statement

Under ADR-0011 the serverless-runtime Temporal plugin drives the order process by calling this
gear's step operations. Whatever a `call` task sends and receives is persisted by the engine: the
plugin owns the full invocation record, the timeline and its internal execution state
([serverless-runtime DESIGN.md §1.4.2](../../../../serverless-runtime/docs/DESIGN.md#142-plugin-model)
line 174; [ADR-0005](../../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)
line 87), Temporal keeps that state in a Temporal Server persistence backend the platform
operates ([ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)
line 100), and the platform exposes it through the timeline, debug and trace endpoints
(DESIGN.md lines 1061–1063) with deep fetches of "stored payloads" delegated to the plugin (line
174). The platform has no sensitive-field annotation or data-classification model for what sits in
execution history ([NEXT_ADR_SCOPE.md](../../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md)
line 23, BR-017).

PRD §15 (line 1183) requires the substrate evaluation to name which commercial data — the
resolved total in approval context, approver identities, tenant axes, the saga log — would sit in
engine history, and how that history is isolated and retained; PRD §6.1's data-classification note
(line 263) requires process artifacts to be tenant-scoped and retained at audit grade by this gear
independently of engine history. The natural Serverless Workflow style is to carry state in the
workflow data and branch on it with `switch`; done naively, the frozen plan, the approval context
with its resolved total, the approver's identity and every last-error text would be in Temporal
history.

What may a task input or output contain, and what may the engine keep of the events that start
and advance an instance?

## Decision Drivers

* PRD §15: no commercial data in engine history is the criterion Q-01 is evaluated on; the content of task inputs and outputs, and what the start trigger and every `listen` keep of the events they consume, are what the engine persists.
* PRD §6.1 and the data-classification note: process artifacts carry commercial order context and must be retained at audit grade by this gear; a copy in a store this gear does not own is a second, uncontrolled retention.
* ADR-0010: every read of commercial data is a PDP-authorized read inside this gear, under a `(resource, action)` pair and a compiled `AccessScope`. Data that crosses into the engine is readable through the platform's timeline endpoints under the platform's authorization, not this gear's.
* Approver identities are authorization inputs (`owf_approval_gate.assigned_principal`, ADR-0010) and personal data (D-61 erasure rules); they must not be copied into a history this gear cannot erase.
* Tenant axes: `resource_tenant_id` is the platform's own isolation axis and is needed by the platform for every invocation; `seller_tenant_id` is a PDP constraint axis and is not needed by the engine.
* The definition still has to branch: it needs *some* outputs to `switch` on. Those must be small closed enums, not values.
* Residency: even references are data at rest in the Temporal persistence backend; the location of that backend is a platform property, not this gear's.

## Considered Options

* **Payload passing** — task inputs and outputs carry the working state (frozen plan, approval context with resolved total, approver identity, line outcomes, last error) as the workflow's data, in the ordinary Serverless Workflow style
* **Encrypted payloads** — the same payloads, envelope-encrypted by the operations with keys this gear owns, so history holds ciphertext
* **References (chosen)** — task inputs and outputs carry identifiers and small closed enums only; every operation resolves the identifiers against this gear's record and reads commercial data inside this gear under the PDP

## Decision Outcome

Chosen option: **references, not payloads**, because it is the only option that answers PRD §15's
first question with "none", keeps every read of commercial data behind this gear's PDP decision,
and leaves nothing in engine history that this gear's retention or erasure rules would have to
reach. Concretely:

* **What may cross**, as a task input or output, and nothing else:
  * `correlationId` — the process instance;
  * `orderId` and `orderVersion` — the frozen order identity the instance is correlated to;
  * `stepRef`, `taskRef`, `gateRef` — opaque identifiers of a step-log row, a fulfillment task and
    an approval gate in this gear's record, so a `fork` per task and a `listen` per gate can be
    expressed;
  * `resource_tenant_id` — the platform's isolation axis, required on every invocation;
  * the platform's own `invocation_id` and `attempt_id`, so `owf_step_log` can record them;
  * small closed enums the operations return for branching: an outcome class
    (`ok | retryable | terminal`), the recorded process phase, a verdict class
    (`required | not-required | unobtainable`), a wave number, an eligibility class, a reason
    **code** from the closed catalogue (D-64) — never a reason message;
  * a duration or instant the definition needs to arm a `wait` (an expected-fulfillment instant,
    a remaining escalation window), as a value without commercial meaning.
  Every operation declares its `input` and `output` as GTS reference schemas built from this list
  only; the schema is the closed contract, and ADR-0012 rule 5 checks the definition against it.

  > **Amended 2026-09-26 by D-131**: the list above named too little. The canonical definition
  > passes references it did not name (`lineRef`, `planRef`, `parkRef`, `requestRef`,
  > `cancelRequestRef`, `suspensionRef`, `subjectRef`, the identities of consumed events) and
  > counters, so a validator written from it would refuse the canonical definition, and one written
  > from the definition would have no closed list at all. The permitted members are therefore a
  > **closed vocabulary of six types**, each a registered GTS schema with a format. A member of a
  > task input, output, `export`, `body` or header, or of `$context`, **MUST** be one of these types,
  > and nothing else crosses:
  >
  > 1. **Identity**: `correlationId`, `orderId`, `orderVersion` (integer), `resourceTenantId`
  >    (`resource_tenant_id`), the platform `invocationId` and `attemptId`, the binding members
  >    `definitionId`, `definitionVersion` and `definitionSource` that `start-instance` records, and
  >    the envelope `id` of a consumed event (`triggerEventId`, `lifecycleEventId`, `resumeEventId`,
  >    `holdEventId`, `decisionEventId`);
  > 2. **Opaque record reference**: a UUID naming a row of this gear's record, with one type per
  >    kind: `stepRef`, `taskRef`, `gateRef`, `lineRef` (an `owf_fulfillment_task` row), `planRef`,
  >    `parkRef`, `requestRef`, `cancelRequestRef` and `suspensionRef`. A `subjectRef` is one of
  >    these, a `correlationId` or `ceiling:{round}`. An array of references is permitted, either
  >    bare or paired with a reason code (`failed[]`, `failedTaskRefs[]`, `failureSubjects[]`);
  > 3. **Counter**: a non-negative integer. This covers every round, sequence and position member
  >    (`…Round`, `…Seq`, `position`, `pass`, `attemptKey`, `openTaskCount`, `rowVersion`), and also a
  >    key tail made only of counters and a literal prefix (`{round}[:{attempt}]`); **amended
  >    2026-09-26 by D-150**: the apply-resume poll's tail also names its suspension,
  >    `poll:{suspensionRef}:{round}` — a literal prefix, an opaque record reference and a counter,
  >    each of this vocabulary — because that round family restarts for every suspension;
  > 4. **Closed enumeration or boolean**, including a catalogue reason code (D-64) and a routing
  >    literal the definition writes itself (`design/10-process-definition.md` §2.2 rule 1);
  > 5. **Instant or duration**, as in the list above (RFC 3339 or ISO 8601);
  > 6. **The step-surface base** `stepsBase`, a configuration literal that only the definition's
  >    `input.from` writes (`design/10-process-definition.md` §2.2 rule 2).
  >
  > A string member is valid only in one of these formats: a UUID, the key-tail pattern, an
  > enumeration member, RFC 3339 or ISO 8601, semver, or the `stepsBase` URL. The `Idempotency-Key`
  > header is composed only of members of the vocabulary (`design/01-foundation.md` §3.3).
  >
  > **Accepted residual: cardinality and counters.** An array of `lineRef`s, `gateRef`s or
  > `taskRef`s shows how many lines a wave carries, how many gates a position opens and how many
  > tasks are open, and so shows the order's line count. A round shows how often an operation was
  > re-invoked. That is process-shape metadata, and PRD §6.1 lists retry counters among the process
  > artifacts. It is part of what remains in history, stated in *Residual* below. It shows no line's
  > content: no product, offer, quantity, price or subscription. "Line counts" is therefore struck
  > from the list below, and "line items" stays.
* **What may never cross**: resolved totals, prices, currencies or any price-pin field; catalog,
  offer, plan or product references; line items or line counts; approver identities and any
  `subject_id`; buyer or seller identity, and any tenant axis beyond `resource_tenant_id`
  (`seller_tenant_id` included); payment-authorization identifiers or instrument data; subscription
  identifiers and transition-request identifiers (they stay in the intent record and the sweep's
  lookup tuple); the frozen plan or any part of it; approval context; the saga/compensation log;
  free text of any kind — error messages, manual-task text, operator notes; and anything the PRD
  classifies as commercial order context (§6.1, line 263).

  > **Amended 2026-09-26 by D-131**: "line counts" is struck. The count is the cardinality of the
  > reference arrays above, an accepted residual. Line content still never crosses.
* **Trigger inputs and consumed events.** The rule covers them as well as task data. The start
  trigger's input is the raw `$workflow.input` (Serverless Workflow DSL 1.0.0, dsl.md *Runtime
  expression arguments*) and a `listen` output is the array of consumed events (dsl-reference.md
  *Listen*), and the events as published are not references: Lifecycle's `OrderSubmitted` carries
  the tenant axes, per-line references and pins and the resolved total's per-line net components;
  `OrderApproved` and `OrderRejected` the deciding authority; `OrderHeld` the hold reason;
  `OrderCancelled` the cancelling actor and the cancel reason; `OrderCompleted` the
  line-to-subscription mapping
  ([Lifecycle `01 §4.4`](../../../orders-lifecycle/docs/design/01-foundation.md#44-events-audit-and-the-outbox-normative),
  lines 2396–2406); the Subscriptions outcome event carries a `subscriptionId`. The start trigger
  and every Lifecycle, Generic Approval and Subscriptions `listen` **MUST** therefore keep only the
  members of the permitted list above — the envelope's event identity, `orderId`, `orderVersion`
  and the fields a filter or correlation needs — through the workflow's `input.from` and the
  `listen` task's `read` mode, `output.as` and `export.as`. Whether that filtering keeps the raw
  event out of **history**, rather than only out of the workflow data, is not stated by the
  platform, so two routes are registered in `UPSTREAM_REQS.md` and either closes the gap: (1) the
  platform persists only the selected members
  (`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-consumed-event-member-storage`); (2)
  Lifecycle publishes thin event variants, or confirms that the full events may be stored
  (`cpt-cf-bss-orders-workflow-upreq-lifecycle-thin-events`), with the Generic Approval decision
  event reference-only under `cpt-cf-bss-orders-workflow-upreq-generic-approval-expectations-contract`.
* **Operations read commercial data under the PDP, inside Orders.** Every operation resolves
  `correlationId` (and `taskRef`/`gateRef` where given) to rows in this gear's record and performs
  its Lifecycle, Subscriptions, Payments and Generic Approval reads and writes through the seam
  clients under this gear's configured authority, narrowed to that instance's `resource_tenant_id`
  and `seller_tenant_id` exactly as the step executor did (ADR-0010 as amended: the step route is
  PDP-authorized for the serverless-runtime service principal; the caller supplies references,
  never authority). The resolved total reaches the approval request from Lifecycle through
  `obtain-verdict`/`open-gates` and is never returned to the definition.
* **Error answers** (added 2026-09-26 by D-132). A refused step call is task data too. The DSL
  raises its 4xx or 5xx answer as the communication error the `catch` sees as `$error`, and the
  engine records it. Every answer the step route (`/bss-orders-workflow/v1/steps/{operation}`)
  produces **MUST** therefore carry only these RFC 9457 members: `type`, `status`, `title`,
  `error_domain`, `error_code`, a `detail` equal to the fixed text registered for that
  `error_code`, and an empty `context` (`context.data` `{}`). It carries no variable text, no
  variant field, no `instance`, and no field violation that echoes a body member
  (`design/01-foundation.md` §4.9, §4.11). The definition reads only `$error.status` (and
  `error_code` once Q-11 (ii) answers), and never exports an `$error` member.
* **How this bounds the PRD §15 criteria.** *Which commercial data would sit in engine history*:
  none in task inputs and outputs — identifiers and enums only; in trigger inputs and consumed
  events, none once one of the two routes above lands, and the events as published until then
  (the residual below). *Isolation and retention of that history*: what remains to
  isolate and retain is a set of identifiers that name rows in this gear's record; their exposure
  through the platform timeline discloses that an order and a process exist and how far the
  process has come, not what was ordered, for whom, at what price or who approved it. *BSS/OSS
  boundary*: the engine is a platform gear, not an OSS store, and holds no commercial document.
  *Audit independent of engine purge*: the record is complete without the history by
  construction, since nothing crosses that is not already in the record.

  > **Amended 2026-09-26 by D-131**: "for whom" was wrong. `resourceTenantId` is in every task
  > input, in every `Idempotency-Key` header (`{tenant}:…`) and in the admission key, so the timeline
  > discloses **which resource tenant owns each order**. It also discloses the cardinalities and
  > counters above. It does not disclose what was ordered, the buyer or seller identity beyond that
  > one axis, the price, or who approved it. PRD §15's first question is answered accordingly:
  > *none* for commercial content in task data; in history, **identifiers (including the resource
  > tenant), counters and cardinalities**, plus the consumed events as published until one of the
  > two routes above lands. It is never an unqualified "none" (Q-01, Q-12, D-66 as amended).
* **Residual.** Task inputs are still in engine history. `correlationId`, `orderId` and
  `orderVersion` are pseudonymous business keys stored, with their timestamps, in a Temporal
  persistence backend whose location and retention the platform sets (serverless-runtime ADR-0004
  line 100; retention is a `TenantRuntimePolicy` concern, DESIGN.md line 739). That history must
  be residency-pinned to the jurisdictions this gear's tenants require, and its retention stated,
  before Q-01 closes (ADR-0011, Q-01 part 2). This is an upstream ask in `UPSTREAM_REQS.md`,
  serverless-runtime section, not an assertion. **Until the member-storage ask or the Lifecycle
  thin-event ask lands, the start trigger's input and the consumed events may sit in history in
  full**: the Lifecycle commercial fields listed above, the Subscriptions `subscriptionId`, and
  whatever the unspecified Generic Approval decision event carries. That is the stated residual of
  this decision, and the platform path is not ready for a tenant for whom it is disqualifying.

  > **Amended 2026-09-26 by D-131**: the residual identifiers are the whole vocabulary above, not
  > three of them. That is `correlationId`, `orderId`, `orderVersion` **and `resourceTenantId`**,
  > carried in every task input and in the headers that embed it, together with the opaque record
  > references, the counters and the cardinalities, and the fixed-text error answers of D-132, all
  > with their timestamps.

### Consequences

* Every operation is self-sufficient given a `correlationId`: it must be able to reconstruct everything it needs from this gear's record, which is also what the fallback sequencer of ADR-0011 and the reconciliation sweep need. The reference-only contract and the recoverability requirement of PRD §7.1 are the same property.
* Branching moves into enums the operations own. A new branch that needs a value the operations do not yet return as an enum is an Orders release (a new enum member on an output schema), which is the correct side of the adjustability contract for anything that reveals commercial state.
* The reason catalogue gains no free-text channel: a task output carries a reason code; the message lives in the audit entry and the manual task, inside this gear.
* Approver identity never leaves this gear, so D-61's erasure rules and ADR-0010's `assigned_principal` constraint stay enforceable in one store.
* Operator progress reads (`design/09-read-and-authz.md`) continue to serve from this gear's record; the platform timeline is not a progress surface for this gear's operators and grants nothing beyond identifiers if reached.
* The platform's per-invocation `Idempotency-Key` header and response caching (DESIGN.md lines 910–911, 943) act on the invocation start, not on step operations; step operations key on ADR-0006's families derived from the references they receive, so the reference set must be sufficient to derive every key family — it is, since each family is composed of `orderId`, `orderVersion`, a line or gate reference resolved from `taskRef`/`gateRef`, wave and kind.

### Confirmation

Verified by: a schema test that every registered operation's `input` and `output` GTS schema is
composed only of the permitted identifier and enum types and rejects any string field without a
closed enumeration (as amended by D-131: any string member not of one of the six vocabulary types
and their formats); the ADR-0012 rule-5 conformance test over the canonical definitions; a test
per operation asserting that its HTTP response body, serialized, contains no field outside its
declared output schema (a golden-response check under the fixture corpus), and, per D-132, that
every refusal the step route can answer — each registered reason of the operation, the envelope's
validation refusal, a PDP denial — serializes to the fixed members of *Error answers* and nothing
else; a test that an
operation given only `correlationId` and `resource_tenant_id` reconstructs the plan, the gate
and the intent it needs from this gear's record; a test that `obtain-verdict` and `open-gates`
pass the resolved total to the approval seam and return only a verdict class to the caller; a
review check that no operation logs its request or response at a level that reaches the platform;
a conformance check that the start trigger and every `listen` of the canonical definitions
select only permitted members; and — once the plugin exists — an end-to-end run followed by a read
of the invocation timeline asserting that no value outside the permitted list appears in any
recorded task input or output, trigger input or consumed event.

## Pros and Cons of the Options

### Payload passing

* Good, because it is the idiomatic Serverless Workflow style and lets the definition branch on anything.
* Bad, because it places the resolved total, the approver identity, the plan and every error text in a Temporal history this gear does not own, retain, erase or authorize reads of — failing PRD §15's first criterion outright.
* Bad, because the platform has no sensitive-field model (BR-017 unaddressed), so nothing on the platform side would mask or restrict it.

### Encrypted payloads

* Good, because history would hold ciphertext, and keys would be this gear's to rotate and destroy.
* Bad, because the definition cannot `switch` on ciphertext, so every branch would still need a plaintext enum beside it — at which point the enum is the reference design and the ciphertext is dead weight.
* Bad, because key management, envelope formats and crypto-shredding for erasure become this gear's to design and operate, for data that has no reason to leave this gear at all.
* Bad, because ciphertext of a known-shape payload still leaks size and timing, and still sits in a store whose residency is not this gear's.

### References (chosen)

* Good, because engine history contains nothing the PRD classifies as commercial, so PRD §15's first question is answered structurally.
* Good, because every commercial read stays a PDP-authorized read inside this gear, and the record remains the single place erasure and retention are applied.
* Good, because it makes each operation self-sufficient from `correlationId`, which is also what recovery and the fallback sequencer require.
* Neutral, because branching is limited to enums the operations return; adding a branch on a new dimension is an Orders release.
* Bad, because identifiers are still data at rest in a platform store; the residency and retention of that store are asks, and the timeline endpoints expose the identifiers under platform authorization.

## More Information

This decision refines ADR-0011 and is enforced by ADR-0012 rule 5. It carries forward the
data-classification note of PRD §6.1 and D-61's actor-reference rules; ADR-0010's catalogue and
D-64's reason contract apply by reference. Q-01 part 2 (ADR-0011) closes when the residency and
retention asks named here are agreed. The register entries carrying it are in `DECISIONS.md` from
D-65 onward.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — §6.1 (data classification note, line 263), §6.7, §7.1, §15
  Q-01 (line 1183)
- **DESIGN**: [DESIGN.md](../DESIGN.md) §4.2, §4.3;
  [`design/10-process-definition.md`](../design/10-process-definition.md) (reference schemas);
  each slice §3.3 (`input`/`output` per operation)
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — Q-01, D-61, D-64, D-66 (as amended), D-131, D-132, D-150, Q-12
- **Upstream asks**: [`UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) — serverless-runtime section
  (history residency and retention; member-only storage of trigger inputs and consumed events);
  Orders Lifecycle section (thin event variants); Generic Approval section (reference-only
  decision event)
- **Platform**: serverless-runtime [DESIGN.md](../../../../serverless-runtime/docs/DESIGN.md)
  §1.4.2, §3.1 (`TenantRuntimePolicy`), §3.3 (Executions API);
  [ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md);
  [NEXT_ADR_SCOPE.md](../../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) (BR-017)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` — the process record is complete in this gear because nothing crosses the engine boundary that is not already recorded here; engine history is reference-only
* `cpt-cf-bss-orders-workflow-fr-owf-authorization` — commercial data is read only inside PDP-authorized operations; the definition and the engine never hold it
* `cpt-cf-bss-orders-workflow-nfr-owf-retention` — retention and erasure of process artifacts apply in one store; the residual identifiers in engine history are covered by the residency and retention asks
* `cpt-cf-bss-orders-workflow-fr-owf-approval-request` — the resolved total reaches the approval request through the seam inside `obtain-verdict`/`open-gates` and is never a task output
* `cpt-cf-bss-orders-workflow-component-foundation` (**reason catalogue**, the component `design/01-foundation.md` §3.2 declares under its unchanged identifier) — task outputs carry reason codes only; messages stay in the audit entry and the manual task
