---
status: accepted
date: 2026-09-10
decision-makers: BSS Orders team (Architecture)
---

# ADR-0002: Nine Slices Numbered By Build Order, Not PRD Section


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Nine slices numbered by PRD section order](#nine-slices-numbered-by-prd-section-order)
  - [Nine slices numbered by implementation build order (chosen)](#nine-slices-numbered-by-implementation-build-order-chosen)
  - [A single combined design document](#a-single-combined-design-document)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-workflow-adr-slice-decomposition`
## Context and Problem Statement

Orders Workflow spans trigger binding, approval execution, fulfillment planning, provisioning-intent issuance, saga/compensation, manual tasks, hold/cancel handling, and read/authorization concerns — each with its own upstream dependency, its own guard set, and different implementation risk. A single combined design document would interleave unrelated normative content with no natural review boundary, and a document numbered after PRD sections would not tell an implementer what to build first. How should the design be decomposed into documents, and what should the numbering mean?

## Decision Drivers

* The correctness core (the shared process engine: durable state, audit, idempotency, saga bookkeeping established in ADR-0001) must be reviewable and buildable before any capability that depends on it.
* Two capability areas depend on unresolved upstream asks (Generic Approval spec; the Subscriptions asks `SUB-O1` and `SUB-O5`, registered-and-unagreed, and `SUB-O11`–`SUB-O14`, this gear's own renumbered asks, UNASKED — see `UPSTREAM_REQS.md` §2.1); their design must be isolatable so an upstream change is a boundary change, not a rewrite of unrelated slices.
* Implementation needs an explicit dependency order: trigger binding and the shared engine must exist before approval execution can start a process, which must exist before fulfillment planning can act on an approved order.
* Sibling BSS gears (`orders-lifecycle`, `pricing`, `rating`, `subscriptions`) already use an index-plus-slices document shape; divergence costs review effort.
* A slug numbered by PRD section (§6.1, §6.2, ...) would not communicate build sequence, since PRD sections group requirements by topic, not by what must exist first at runtime.

## Considered Options

* Nine slice documents numbered by PRD section order, mirroring the requirements table
* Nine slice documents numbered by implementation build order, with `01-foundation` as the shared process engine every other slice depends on
* A single combined design document with internal headings but no per-capability document boundary

## Decision Outcome

Chosen option: **nine slice documents numbered by implementation build order**, because build order is the information an implementer and a reviewer both need first, and PRD section order does not carry it. The nine slices are: `01-foundation` (the shared process engine: durable process state, audit log, saga/compensation bookkeeping, idempotency registry — the substrate ADR-0001 establishes as gear-owned); `02-triggers-and-start`; `03-approval-execution`; `04-fulfillment-plan`; `05-provisioning-intents`; `06-saga-and-compensation`; `07-manual-tasks`; `08-hold-and-cancel`; `09-read-and-authz`. Each later slice depends only on `01-foundation` and, where stated, on an earlier-numbered slice — never on a later one. This ADR is the build-order authority: `design/README.md` cites these nine numbers and titles verbatim rather than re-deriving them.

### Consequences

* `01-foundation` must be complete and stable — its guard/state/audit contract frozen — before `02` through `09` can be implemented against it without rework.
* A slice author touches one file for their capability; a reviewer reads one file per capability rather than a single interleaved document.
* Any new capability that needs a new process-engine primitive (a new state, a new saga step type) is a `01-foundation` change, mirroring the same non-negotiable-chokepoint shape as `orders-lifecycle` ADR-0001; slices `02`-`09` may not silently extend the engine's contract.
* The two upstream-dependent slices (`03-approval-execution` on the Generic Approval spec; `05-provisioning-intents` on the Subscriptions upstream asks) are isolated to their own documents, so an upstream change is scoped to one file's revision rather than forcing a renumbering or a rewrite of unrelated slices.
* `design/README.md` and any other document that lists these nine slices must cite the numbers and titles from this ADR rather than re-deriving them, so a future renumbering is a single-ADR change rather than a multi-document hunt.
* The numbering communicates build sequence, not review priority or PRD traceability; a reader who wants requirement traceability must still consult the PRD IDs cited in each slice, not infer them from the slice number.

> **Amended 2026-09-24 by ADR-0011** (`cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition`):
> the decomposition is now **foundation + process definition + seven step-operation slices + the
> read/control slice**. `design/10-process-definition.md` is added and holds the platform workflow
> definition — grammar subset, canonical versions, validation rules (ADR-0012) and the binding
> contract; slices `02`–`08` each declare their step operations in §3.3 instead of handlers and
> timers; `09` declares no step operations — its control operations map to the platform
> invocation API (`POST /api/serverless-runtime/v1/invocations`, `…:control`) plus this gear's own
> reads. File names and ids are unchanged; `10` is numbered by availability, not by build order,
> and the number is historical. The rule "a slice depends only on an earlier-numbered slice" is
> restated as "a slice depends only on a slice earlier in the build order below". The build order
> `design/README.md` cites is:

| Build order | Doc | Role after ADR-0011 | Depends on |
|---|---|---|---|
| 1 | `10-process-definition` | the definition: protected path, grammar subset, canonical versions, validation rules, binding | — (fixes what every operation slice implements against) |
| 2 | `01-foundation` | the process record, `owf_step_operation` registry, `owf_definition_binding`, foundation operations (`start-instance`, `settle-from-lookup`, `retry-step`, `park`, `unpark`, `terminate-instance`), the three workers | 10 |
| 3 | `02-triggers-and-start` | `admit-trigger`, `terminate-on-terminal-event` | 10, 01 |
| 4 | `03-approval-execution` | `obtain-verdict`, `reflect-verdict`, `open-gates`, `record-decision`, `arm-park-escalation`, `escalate-gate` | 10, 01, 02 |
| 5 | `04-fulfillment-plan` | `construct-and-freeze-plan`, `evaluate-payment-auth-eligibility`, `re-check-pre-activation`, `evaluate-activation-eligibility`, `begin-fulfillment` | 10, 01, 02, 03 |
| 6 | `05-provisioning-intents` | `dispatch-wave1-create`, `dispatch-wave2-activate`, `reread-draft-liveness`, `rebuild-wave1`, `reconcile-intent`, `report-spawn-signal` | 10, 01, 04 |
| 7 | `06-saga-and-compensation` | `run-cancellation-fence`, `compensate-order`, `report-outcome` | 10, 01, 05 |
| 8 | `07-manual-tasks` | `create-manual-task`, `resolve-manual-task`, `verify-override`, `raise-overdue-escalation` | 10, 01, 05, 06 |
| 9 | `08-hold-and-cancel` | `apply-hold`, `apply-resume`, `authorize-cancel` | 10, 01, 02, 03, 05, 06 |
| 10 | `09-read-and-authz` | reads, the PDP catalogue including `process_step × execute`, control operations over the platform invocation API; no step operations | 10, 01–08 |

> **Amended 2026-09-26 by D-139** (`DECISIONS.md`): the table above ordered `10` before `01`
> and `06` before the `07` and `08` ports it calls, and hid four cycles. The build order is now
> **01, 10, 02, 03, 04, 05, 07, 08, 06, 09**, derived from in-process calls, port calls and table
> reads, never from the definition's calls: `01` first, because the definition calls nothing but
> operations registered under its contract; `07` and `08` before `06`, because `06` calls slice
> 07's creation port and Incident Recorder and slice 08's suspension-closure and cancel-authority
> ports; `08` after `07`, whose manual-task creator it calls. The edges that still point later
> — `07` reading `06`'s fence row, and `06`, `07` and `08` using `09`'s catalogue, scope predicate,
> cancel request record and request-delivery port — are stated as **back-edges**: the earlier slice is built against a double of the
> later slice's port and integrated when the owner lands. The rule "a slice depends only on a
> slice earlier in the build order" holds for every edge except a stated back-edge.
> `design/README.md` carries the table and the back-edges; the slice file names and ids are
> unchanged.
>
> **Amended 2026-09-26 by D-139 (as amended)**: the four cycles above are `06`↔`07`, `06`↔`09`,
> `07`↔`09` and `08`↔`09` (the block first said three), and the foundation adds four more, because its workers and `terminate-instance` call slices `05`, `06`, `07` and `08`
> in-process (`design/01-foundation.md` §3.3, §3.8). Those calls are `01`'s back-edges, built
> against doubles like the others, so eight pairs are stated in `design/README.md`.
>
> **Amended 2026-09-26 by D-139 (second amendment)**: the foundation's `retention-purge` worker
> deletes rows from the tables of slices `03` through `09` (`design/01-foundation.md` §3.8), and
> a table write is an edge (`design/README.md` *What an edge is*), so `01` also has back-edges to
> `03`, `04` and `09`, and the stated cycles are eleven: `01` with each of `03`–`09`, and the four
> above.

### Confirmation

Confirmed by a documentation-structure check that `design/README.md`'s slice table matches this ADR's nine slugs and order exactly, and by a dependency-direction review confirming no slice document declares a dependency on a higher-numbered slice.

> **Amended 2026-09-26 by D-139**: the structure check compares `design/README.md`'s table with
> **ten** documents (the nine slices and `10-process-definition`) in the build order of the block
> above, and the dependency-direction review confirms that no document declares a dependency on
> a document later in that order **except a back-edge the README states**, each of which must be
> built against a double of the later document's port.

## Pros and Cons of the Options

### Nine slices numbered by PRD section order

* Good, because a reader moving from PRD to design finds a familiar section-to-section mapping.
* Bad, because PRD sections group by topic (state ownership, approval, fulfillment, resilience, hold/cancel, authorization) not by what must be built first, so the numbering would not answer "what do I build before what."
* Bad, because PRD section boundaries do not align cleanly with runtime/document boundaries (e.g., §6.1 spans both foundation and trigger concerns), forcing an awkward document split.

### Nine slices numbered by implementation build order (chosen)

* Good, because the numbering directly answers the sequencing question an implementer and a phase-runner both need.
* Good, because it isolates upstream-dependent slices (`03`, `05`) from the shared engine and from each other.
* Good, because it matches the shape sibling BSS gears already use, so reviewers already know how to navigate it.
* Neutral, because it requires a one-time mapping exercise from PRD requirement IDs into the slice that owns them, recorded per slice rather than inferred from the number.
* Bad, because a slice number alone does not indicate PRD traceability; each slice must carry its own requirement-ID references explicitly.

### A single combined design document

* Good, because there is only one file to keep internally consistent.
* Bad, because it produces one very large interleaved document with no natural review boundary, mirroring the exact problem the sibling `orders-lifecycle` gear's ADR-0002 rejected for the same reason.
* Bad, because the two upstream-dependent capability areas cannot be isolated from the rest of the design, so an upstream spec change forces a diff across unrelated content.

## More Information

This decomposition takes the same shape as `orders-lifecycle` ADR-0002 (foundation-plus-handlers), adapted from "one engine plus seven capability handlers around a state machine" to "one shared process engine plus eight process-capability slices around it." The count differs (nine here vs. seven there) because this gear's process-orchestration surface splits triggers-and-start from approval execution as separate build phases, which the sibling gear's state-machine surface does not need to.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — §6 (all subsections), §13
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — D-139 (the build order and its back-edges)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-bss-orders-workflow-fr-owf-boundary-binding` — the trigger-and-start contract is scoped to its own slice (`02-triggers-and-start`) rather than spread across the document set
* `cpt-cf-bss-orders-workflow-fr-owf-fulfillment-plan` — fulfillment planning is isolated in `04-fulfillment-plan`, depending only on `01-foundation` and the approval outcome from `03-approval-execution`
* `cpt-cf-bss-orders-workflow-fr-owf-compensation-declaration` — saga/compensation is a dedicated slice (`06-saga-and-compensation`) rather than duplicated logic inside fulfillment or provisioning slices
* `cpt-cf-bss-orders-workflow-fr-owf-authorization` — read and authorization concerns are isolated in `09-read-and-authz`, the slice with the widest caller surface and the narrowest write surface
