<!-- CONFLUENCE_TITLE: [BSS]: Orders Workflow — Design Set -->
<!-- Related: ../DESIGN.md, ../PRD.md | Owners: BSS Orders team -->

# Orders Workflow — Design Set

<!-- toc -->

- [Slice documents](#slice-documents)
- [Slice map (PRD ↔ implementation phase)](#slice-map-prd--implementation-phase)
- [Honesty disclosures](#honesty-disclosures)
- [Validation posture](#validation-posture)

<!-- /toc -->

This folder holds the Orders Workflow technical design as a **set of documents**: a shared
**foundation** (`01-foundation.md`), the **process definition** (`10-process-definition.md`) and
eight capability slices. Since [`../ADR/0011`](../ADR/0011-cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition.md)
the foundation is two things: the **step-operation envelope** every step runs inside — idempotency
resolution before the effect, the record, the audit entry, the typed process event and the
settlement in one transaction after it, a per-operation deadline around it — and **the process
record**: the process instance with its recorded phase, the definition binding, the step log with
the platform attempt identity, the idempotency registry, the hash-chained audit trail, the
operation registry and the machine-readable reason catalogue. Each capability slice provides
**step operations** and a **definition fragment**. The **flow** — the order of steps, the
branches, the waits, the barrier, the event listening and the signal arms — is a versioned
Serverless Workflow definition executed by the platform gear `serverless-runtime`, specified in
`10-process-definition.md`. The platform drives; Orders records.

This gear acts **on** the order document but does not own it — Orders Lifecycle is the system of
record. All provisioning flows through Subscriptions; this gear never invokes OSS directly.

**[`../DESIGN.md`](../DESIGN.md) is the canonical index for the architecture overview, the
component model, the cross-cutting posture, and a registry of the gear's tables — which table is
specified where, who owns its content, and whether it is mutable.** It is an index, not a second
schema: column-level schemas, endpoint paths, operation declarations and state enums live in the
documents below and nowhere else, so there is exactly one normative statement of each and no
"authoritative once authored" deferral between them. Requirements (WHAT/WHY) live in
[`../PRD.md`](../PRD.md); decision rationale lives in [`../ADR/`](../ADR/) and in the register
[`../DECISIONS.md`](../DECISIONS.md); asks on gears this one does not own live in
[`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md).

## Slice documents

- [`01-foundation.md`](./01-foundation.md) — **step operations and the process record**: the
  internal step surface `POST /bss-orders-workflow/v1/steps/{operation}` and its envelope, the
  step-operation contract, the operation registry, the idempotency registry and its closed
  outcomes, the process instance and its recorded phase, the definition binding, the step log,
  the audit writer and hash-chained trail, the platform event producer adapter, the reason
  catalogue, the three-worker roster and the platform readiness gate; its own six operations
  (`start-instance`, `settle-from-lookup`, `retry-step`, `park`, `unpark`, `terminate-instance`).
- [`10-process-definition.md`](./10-process-definition.md) — **the process definition**: the
  Serverless Workflow grammar subset Orders uses, the closed `listen` set, the validation rules and
  hook, versioning, pinning and publish roles, the signals, the canonical definition as six path
  fragments, and the fence of protected operations.
- [`02-triggers-and-start.md`](./02-triggers-and-start.md) — admission and start: `admit-trigger`
  over the nine-trigger closed vocabulary, the two platform start triggers, supersession as
  unwind-then-start, and `terminate-on-terminal-event`.
- [`03-approval-execution.md`](./03-approval-execution.md) — the approval step: verdict,
  reflection, gates, decision recording, the fail-closed park and escalation as six operations
  over the approval record, the stand-in behaviour while the Generic Approval service does not
  exist, and the gate-window port.
- [`04-fulfillment-plan.md`](./04-fulfillment-plan.md) — payment-authorization eligibility, plan
  construction and freeze, the begin-fulfillment seam call, activation eligibility and the
  pre-activation re-check.
- [`05-provisioning-intents.md`](./05-provisioning-intents.md) — the six intent operations: wave
  dispatch under the composed key with admission on dispatch, draft liveness, wave-1 rebuild,
  reconciliation and the spawn signal; the reconciliation-sweep worker.
- [`06-saga-and-compensation.md`](./06-saga-and-compensation.md) — the cancellation fence, the
  one-operation reverse compensation walk resumable by pass, and outcome reporting as the sole
  Lifecycle outcome caller.
- [`07-manual-tasks.md`](./07-manual-tasks.md) — manual-task creation, resolution, override
  verification and overdue escalation as four operations over the task record; the operator
  queue and its per-action routes.
- [`08-hold-and-cancel.md`](./08-hold-and-cancel.md) — `apply-hold`, `apply-resume` and
  `authorize-cancel` over the suspension record, the pause of an escalation window through the
  gate-window port, and the apply-time cancel re-check.
- [`09-read-and-authz.md`](./09-read-and-authz.md) — no step operations of its own: the
  authorization of the step surface (`process_step × execute`), the control operations that
  record a request and signal the invocation, the progress projection, the seller-scoped audit
  read over `01`'s `owf_audit_entry` (D-186), and the per-actor authorization matrix.

## Slice map (PRD ↔ implementation phase)

The word "phase" carries three different meanings across this design set, so this table fixes
which one it uses: **the `Phase` column below is the build phase — the order in which the
documents are implemented.** It is not the *delivery* phase a slice body means when it says a
capability is "inert in phase 1" (that is about what ships working, and is driven by whether an
upstream dependency exists), it is not the authoring-plan phase number some slices cite when
pointing at where a section was written, and it is not the recorded process phase of
`owf_process_instance`. Where any of those appears elsewhere, it refers to a different axis and
does not index this table.

**The numeric prefix is historical, not build order.** `02` through `09` were written before
ADR-0011, and `10-process-definition` was added afterwards under the next free number because `02`
was taken; it is nonetheless **first in build order after the foundation**, because every other
slice's §3.6 is now a path through the definition it specifies, and every slice's operations are
declared against the contract and the fence it fixes. A document is scoped by PRD decomposition but
built when its dependencies exist.
[`../ADR/0002`](../ADR/0002-cpt-cf-bss-orders-workflow-adr-slice-decomposition.md)
(`cpt-cf-bss-orders-workflow-adr-slice-decomposition`) names this table the build-order authority
(as amended by `../DECISIONS.md` D-139).

**What an edge is.** A slice depends on another when it calls that slice's operation or port
in-process, or reads or writes its table. The definition's `call` to an operation is not an edge:
every slice needs `10`, and the definition is exercised end to end only by the publish job's
behavioural gate (`10 §4.2`), once every operation exists. The *Depends on* column lists the
earlier slices a slice needs; the *Back-edges* column lists the places where a slice needs a later
one: the foundation's workers and `terminate-instance` calling slices 05, 06, 07 and 08, and its
`retention-purge` worker deleting rows from the tables of slices 03 through 09 (`01 §3.8`); slice
07 reading slice 06's fence row; and slices 06, 07 and 08 using slice 09's catalogue, scope
predicate, cancel request record and request-delivery port. `retry-step` re-running a named
step's operation is not an edge: it dispatches through the operation registry (`01 §3.2`), as
the step route does. Each back-edge is a port the later slice owns: the earlier slice is built and
unit-tested against a double of that port, and its integration test runs once the owner lands —
the way Orders Lifecycle builds capture against an `EventBrokerApi` double
([`../../../orders-lifecycle/docs/design/README.md`](../../../orders-lifecycle/docs/design/README.md),
*Phase 0/1*). The purge's edges are table deletes, not ports: each slice's purge predicate and
retention index join the worker's roster in that slice's own build step, where their test runs.

| Order | Doc | PRD § | Phase | Depends on | Back-edges (built against a double) |
|-------|-----|-------|-------|------------|-------------------------------------|
| 1 | `01-foundation` | process engine core — step operations and the record | 0/1 | — | 05 (`reconcile-intent`, run in-process by the reconciliation sweep's intent pass); 06 (`run-cancellation-fence`, `compensate-order`, `report-outcome`, driven by the liveness pass's dead-instance unwind); 07 (the closure port `close_open_tasks` in `terminate-instance`; the creation port for the `invocation-dead` task; `resolve-manual-task` for the pass's scoped `sla-check` and the unwind's task resolutions); 08 (`authorize-cancel` in the dead-instance unwind); and 03, 04, 05, 06, 07, 08 and 09 (the `retention-purge` worker's bounded deletes over their tables, each table's window and retention index declared in its slice's §3.7 and rostered in `DESIGN.md` §3.7) |
| 2 | `10-process-definition` | the flow (§6.1–§6.4 paths, §17.1) | 0/1 | 01 | — |
| 3 | `02-triggers-and-start` | trigger + start | 1 | 01, 10 | — |
| 4 | `03-approval-execution` | approval | 1 | 01, 10, 02 | — |
| 5 | `04-fulfillment-plan` | fulfillment planning | 1/2 | 01, 10, 02, 03 | — |
| 6 | `05-provisioning-intents` | provisioning | 2 | 01, 10, 04 | — |
| 7 | `07-manual-tasks` | manual tasks | 2/3 | 01, 10, 03, 04, 05 | 06 (the fence-row read in `resolve-manual-task`'s re-check); 09 (catalogue rows, §4.4 scope predicate) |
| 8 | `08-hold-and-cancel` | hold / cancel | 3 | 01, 10, 03, 04, 05, 07 | 09 (§4.4 authorization snapshot and apply-time re-check; the request-delivery port that writes `owf_cancel_request`) |
| 9 | `06-saga-and-compensation` | saga / compensation | 2 | 01, 10, 02, 03, 04, 05, 07, 08 | 09 (the cancel request record's `cancel_reason`, read by `report-outcome`; the request-delivery port at `pre-compensation`) |
| 10 | `09-read-and-authz` | reads / authz | 3/4 | 01–08, 10 | — |

Several of these edges are less obvious than the rest and are stated because they were checked
against the component, operation and table ownership established in `DESIGN.md` §3. `10` needs
only `01` because the definition calls nothing but registered operations under the `01` contract;
every slice needs `10` because its definition fragment is part of the one canonical definition and
its protected operations are ordered by `10`'s fence. `03` needs `02` because the approval step
only runs inside an admitted, version-pinned process instance. `04` needs `03` because the
fulfillment plan is only built once the process has passed (or been made to bypass) the approval
gate. `05` needs `04` because a provisioning intent is derived line-by-line from the frozen plan,
not from the raw order, and every outcome is applied through slice 04's transition function. `07`
needs `03`, `04` and `05` because its operations call slice 03's park port, slice 04's transition
function and slice 05's `binding_reference`. `08` needs `03`, `04`, `05` and `07` because it calls
slice 03's gate-window port, slice 05's deferral port with slice 04's transition function, and
slice 07's manual-task creator in the same unit of work. `06` comes after both: it calls slice
03's closure port, slice 04's completion predicate, slice 05's intent dispatcher, slice 07's
creation port and Incident Recorder, and slice 08's suspension-closure and cancel-authority ports
(`06 §5`). The cancel path reaching `06`'s fence is a
path of the definition, not an edge. `09` depends on `01` through `08` and `10` because it
authorizes every route and every step operation the others declare, and its step-retry route
calls slice 07's resolution-request intake port (`07 §3.2`, D-181); nothing here is separable from
the whole set.

**The cycles, stated.** Eleven pairs depend on each other, and the back-edges above are where each is
broken: `01` with each of `03`, `04`, `05`, `06`, `07`, `08` and `09` (every slice registers its
operations under `01`'s envelope and contract; `01`'s workers and `terminate-instance` call into
05–08, and its `retention-purge` worker deletes from the tables of 03–09, which is an edge by the
table rule above even though no port is called); `06` and `07` (06 calls 07's creation port; 07's retry re-check reads 06's fence row); and
`06`, `07` and `08` each with `09` (09 enumerates their routes and operations; they declare their
routes against 09's catalogue and scope predicate, and 06 and 08 reach 09's cancel request record
through its request-delivery port or by a read). A back-edge double is replaced by the owner's port in
the owner's build step, and 09's conformance test (`09 §3.7` *No permission table: the catalogue conformance check*, `inst-cc-conformance`) is the check
that every route 07 and 08 declared maps to the pair 09 registers.

## Honesty disclosures

Three gaps are stated here plainly rather than left implicit in the documents.

**(a) The phase-1 approval path is inert.** Until the Generic Approval service exists, a stand-in
sits behind the same expectations contract and unconditionally returns "approval not required",
and that stand-in must be recorded as the deciding authority by name on every verdict it produces.
As a direct consequence, multi-party approval gates, escalation, and the approver inbox never fire
in this phase, and two of the six process events — `OrderApprovalRequested` and
`OrderApprovalEscalated` — never fire either. The Generic Approval service has **no specification
anywhere in this repository**; `03-approval-execution.md` designs against its expectations
contract only, not against an implementation.

**(b) Fulfillment's pre-activation abort check cannot be fully evaluated today.** One half of that
check needs the upstream overlap-presence read `SUB-O5`. `SUB-O5` is registered on the upstream
seam map but is **unagreed** — until it lands, that half of the check is unevaluable, and
`04-fulfillment-plan.md` must fail closed on it rather than assume a result.

**(c) The platform path is not buildable until the platform engine exists.** `serverless-runtime`
has **no code and no Temporal plugin today**: its design and ADRs exist, its host, SDK and
`plugins/temporal-plugin/` do not. The canonical definition of `10-process-definition.md` is
therefore **documentation until the platform readiness gate passes** (`01-foundation.md` §3.8):
it has not been parsed, published or executed by the platform, and the platform capabilities it
relies on are upstream asks (`../UPSTREAM_REQS.md` §2.9), not facts. **The fallback property**
holds regardless: the step operations and the gear-owned record of `01`–`09` are unchanged if the
platform slips, and only the sequencing would fall back to code, binding each instance with
`definition_source = code`. That fallback is a stated property of the decomposition, not a plan —
no code sequencer is designed, and none will be unless the gate is declared failed.

## Validation posture

The ten documents of this folder — `01`–`09` and `10-process-definition.md` alike — are
**deliberately outside `cfs` autodetect and traceability**, per
`.cf-studio/config/artifacts.toml`. `cfs` hardcodes its artifact-kind set, and a `DESIGN_SLICE`
kind cannot be registered — this was attempted and reverted on 2026-07-28. The documents follow the
DESIGN template and declare `cpt-cf-bss-orders-workflow-*` ids, but they are **review-audited**
rather than `cfs`-validated, the same posture already used by the pricing, rating and
subscriptions design sets. Concretely: this means the set's coherence claims are
**reviewer-checkable statements, not CI-enforced ones**. Those claims are: that the dependency
edges in the slice map line up with the component, operation and table ownership the documents
declare; that each `cpt-cf-bss-orders-workflow-*` id is minted in exactly one document and
referenced everywhere else; that every table a document specifies appears as a row in
`DESIGN.md`'s table registry naming that document as its specifying one, and that no table's
columns, endpoints or state enums are declared in both places; that every step operation a slice
declares in its §3.3 appears, with the same `protection`, in `09`'s `process_step × execute` value
table and in ADR-0012's protected list; and that the process-event set the documents declare is
the same set `DESIGN.md` names. The YAML fragments of `10-process-definition.md` are likewise
checked by reading against the Serverless Workflow v1.0.0 specification, not by a parser. Nothing
above is enforced — each is a statement a reviewer can check and, until one does, an assertion
this set makes about itself. `design/README.md` itself is likewise excluded from `cfs`
autodetect, so `cfs validate --artifact` reports it unmatched; that is expected, not an error.
