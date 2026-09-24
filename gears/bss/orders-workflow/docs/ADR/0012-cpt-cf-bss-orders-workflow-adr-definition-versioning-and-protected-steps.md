---
status: accepted
date: 2026-09-24
decision-makers: BSS Orders team (Architecture)
---
# ADR-0012: Definition Versions Are Pinned Per Instance And Fenced By Validation Over A Protected-Step List


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [No fence](#no-fence)
  - [Review-only fence](#review-only-fence)
  - [Validation-hook fence (chosen)](#validation-hook-fence-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps`
## Context and Problem Statement

ADR-0011 makes the order process flow a versioned platform workflow definition. That gives a
platform operator the power to publish a definition that orders this gear's step operations — and,
unless something prevents it, a definition that omits `run-cancellation-fence` before
`compensate-order`, wraps `begin-fulfillment` in a `catch` that swallows its refusal, calls
`dispatch-wave2-activate` before every wave-1 draft is confirmed, listens for an event outside the
nine Lifecycle triggers, or passes a resolved total as a task input. The platform validates a
definition against the Serverless Workflow schema at submission
([serverless-runtime ADR-0003](../../../../serverless-runtime/docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md)
line 78) and runs plugin-specific validation through the plugin trait's registration-validation
hook ([DESIGN.md §3.2](../../../../serverless-runtime/docs/DESIGN.md#32-component-model) lines 762
and 792); neither knows what an order process must contain. The platform pins an in-flight
invocation to the callable version it started under (DESIGN.md line 614, BR-029), and PRD §5.2
(line 227) puts operator migration of a running instance out of scope. The platform lists
publishing governance (review/approval of definition changes) and an audit event schema for
definition changes as unaddressed
([NEXT_ADR_SCOPE.md](../../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) lines 26 and 48,
BR-034 and BR-122).

What fence keeps a published definition inside the process this gear's PRD requires, who may
publish, how is a publish evidenced, and how is an instance bound to the version it started under?

## Decision Drivers

* The adjustability contract of ADR-0011 must be real: a `composable` operation may be reordered, inserted or dropped by a publish. The fence must not require an Orders release for those changes.
* The PRD's `p1` invariants — fail-closed verdict (ADR-0007), two-wave barrier (ADR-0004), cancellation fence before compensation (ADR-0005), closed trigger set (PRD §6.1), no commercial data in engine history (ADR-0013), nesting of bounds (D-02, D-53) — are properties of the *path*, so the fence must be able to see the whole path, not one task.
* A protected operation that is omitted or wrapped so that its failure is swallowed is a silent bypass of a `p1` guard; the fence must fail closed, at publish where possible and at run time regardless.
* Instances are pinned (PRD §5.2); a version with bound instances must stay resolvable for as long as this gear's audit can cite it (`cpt-cf-bss-orders-workflow-nfr-owf-retention`), and the platform's function lifecycle hard-deletes after a retention period (DESIGN.md line 610).
* Every publish is a security-relevant change to a `p1` order-taking process and must be attributable; the platform's audit of definition CRUD is not yet specified (NEXT_ADR_SCOPE.md line 26).
* Human review alone does not scale to the change rate the adjustability contract exists for and cannot check a nesting inequality or a schema reliably.

## Considered Options

* **No fence** — any schema-valid definition may be published; Orders trusts the publisher and its operations refuse at run time whatever they can detect
* **Review-only fence** — a human four-eyes review at publish (the platform's BR-122 governance once it exists) with a checklist derived from this ADR; no machine check
* **Validation-hook fence (chosen)** — machine-checked rules over the whole definition, run by a pre-publish validation hook the platform registry calls and by a CI test over the canonical definitions, layered over run-time precondition guards inside the `protected` operations

## Decision Outcome

Chosen option: **validation-hook fence**, because it is the only option that checks the path
rather than one task, fails closed before a definition can be executed, and still lets a
`composable` change publish without an Orders release. Concretely:

* **Validation rules.** A definition version is publishable only if all of the following hold,
  checked over the whole definition:
  1. every `protected` operation of the path appears exactly where its order constraints require:
     `admit-trigger` precedes every other operation on a trigger arm; `obtain-verdict` precedes
     `reflect-verdict`; `record-decision` precedes any activation of the approval outcome;
     `construct-and-freeze-plan` precedes `evaluate-payment-auth-eligibility`, which precedes
     `re-check-pre-activation` and `begin-fulfillment`; `dispatch-wave1-create` for every task
     precedes `dispatch-wave2-activate` for any task, and `dispatch-wave2-activate` follows both
     the wave-1 join and the expected-fulfillment `wait` (the ADR-0004 conjunction);
     `run-cancellation-fence` precedes `compensate-order`, which precedes `report-outcome`;
     `terminate-instance` is the last operation on every terminal path; `start-instance` is the
     first operation of every instance;
  2. every `call` targets an operation registered in `owf_step_operation`, and a `call` to a
     registered Function targets only a `composable` operation;
  3. every `listen` target is within the closed set: the nine Lifecycle triggers, the approval
     decision, the Subscriptions confirmation and failure events;
  4. bounds nest: per-task timeout < task retry budget < the enclosing `wait`/deadline < the
     overdue window < the lifetime ceiling (D-02, D-53), and every `wait` has a bound;
  5. no payload field crosses: every task input and output conforms to the reference schema the
     operation declares (ADR-0013), checked structurally against the declared `input` and `output`;
  6. no `protected` operation is wrapped in a `catch` that swallows its failure: a `catch` around a
     protected operation may only `raise`, call `park`, call `create-manual-task`, or route to the
     compensation path; it may not continue the forward path.
  The `protected` list is closed and fixed here; adding to it or removing from it is an Orders
  release and an amendment of this ADR: `start-instance`, `settle-from-lookup` (sweep-only),
  `terminate-instance`, `admit-trigger`, `terminate-on-terminal-event`, `obtain-verdict`,
  `reflect-verdict`, `record-decision`, `construct-and-freeze-plan`,
  `evaluate-payment-auth-eligibility`, `re-check-pre-activation`, `begin-fulfillment`,
  `dispatch-wave1-create`, `dispatch-wave2-activate`, `report-spawn-signal`,
  `run-cancellation-fence`, `compensate-order`, `report-outcome`, `create-manual-task`,
  `apply-hold`, `apply-resume`, `authorize-cancel`. Every other registered operation is
  `composable`: `retry-step` (operator), `park`, `unpark`, `open-gates`, `arm-park-escalation`,
  `escalate-gate`, `evaluate-activation-eligibility`, `reread-draft-liveness`, `rebuild-wave1`,
  `reconcile-intent` (sweep), `resolve-manual-task`, `verify-override`,
  `raise-overdue-escalation`. A `protected` operation may be ordered by a definition; it may never
  be omitted or replaced.
* **Two enforcement points, one rule set.** The rules run (a) in a validation hook the platform
  registry calls before a definition version is published — a consumer-registered pre-publish
  hook is not in the platform surface today (only the plugin's registration-validation hook is,
  DESIGN.md line 762) and is raised as an upstream ask — and (b) in a CI conformance test in this
  gear's repository over the canonical definitions that `design/10-process-definition.md` names.
  Until (a) exists, (b) plus the publish role below is the fence at publish time.
* **Run-time guards inside protected operations, regardless of the fence.** Every `protected`
  operation checks its own precondition in this gear's record before acting and refuses with a
  catalogue reason if it does not hold — `dispatch-wave2-activate` refuses without a frozen plan
  and every task at `draft_created`; `compensate-order` refuses without a recorded cancellation
  fence; `terminate-instance` refuses without a reported outcome or a recorded terminal event. A
  definition that bypasses the static fence therefore fails closed at the first protected
  operation it misorders, and an instance whose invocation ends without `terminate-instance` is
  found by the reconciliation sweep (`settle-from-lookup`) and raised as a manual task.
* **Pinning.** `start-instance` writes `owf_definition_binding` (`correlation_id`,
  `definition_id`, `definition_version`, `definition_source`, `pinned_at`, `published_by`,
  `resource_tenant_id`) in the same transaction that creates the instance; the instance runs to
  termination on that version. There is **no migration** of a running instance (PRD §5.2). A
  definition version is never archived or deleted while any `owf_definition_binding` row
  references it, and remains queryable for this gear's audit retention afterwards; because the
  platform's `archived → deleted` transition runs on the platform's retention clock (DESIGN.md
  lines 609–610), this is an upstream ask rather than an assumption.
* **Publish roles.** The publish role is the **platform operator** today, authorized by the
  platform on the Function Registry API (DESIGN.md line 847). A seller-scoped role that may
  publish a fragment of the definition — for example a seller's own escalation arms — is
  registered as Q-10 and is not granted by this decision.
* **Audit of publishes.** Every publish must be attributable to an actor, a version and a
  validation result. The platform's audit of definition changes is unaddressed (NEXT_ADR_SCOPE.md
  line 26, BR-034) and is raised as an upstream ask; until it lands, the CI conformance run over
  the canonical definitions and the registry's version listing (DESIGN.md line 857, *list
  versions*) are the evidence, and `owf_definition_binding.published_by` records, per instance,
  who published the version it was bound to.

### Consequences

* `owf_step_operation` is the source of truth for rule 2 and rule 5: it is loaded at startup from the compiled registry, has no runtime write path and is audited on load, exactly as the former permission table was; a definition can only reference what a release registered.
* The CI conformance test becomes a release gate for this gear and a publish gate for the definitions: a change to a `protected` operation's order constraints fails the test until the canonical definitions are updated in the same change.
* The `protected` list turns ADR-0004's conjunction rule, ADR-0005's fence-before-compensation order and ADR-0007's park exits into checkable statements about a document rather than conventions about code (those ADRs carry dated amendments saying so).
* Operators lose nothing they had: `retry-step` remains an operator operation; the platform's `retry` and `replay` control actions (DESIGN.md lines 888–889) act on the invocation and are not a way around a protected operation's precondition, since the re-invoked operation re-checks it.
* Because a `catch` around a protected operation may not continue the forward path, every failure of a protected operation has one of four destinations — `raise`, park, manual task or compensation — which is the same closed outcome set ADR-0009 and ADR-0005 already require.
* A definition that passes validation can still be operationally wrong (a wait too short for a seller's approval practice, an escalation arm that pages the wrong queue); the fence bounds safety, not suitability. Suitability remains a publish-time review concern and, once the platform provides it, a BR-122 governance concern.

### Confirmation

Verified by: the CI conformance test running rules 1–6 over every canonical definition and over a
mutation corpus — each canonical definition with one protected operation removed, one reordered,
one wrapped in a forward-continuing `catch`, one `listen` outside the closed set, one non-nesting
bound and one payload field in a task input — asserting every mutant is rejected with the rule
named; a test that `start-instance` writes `owf_definition_binding` in the instance-creating
transaction and refuses a second binding for the same `correlation_id`; a test that an instance
bound to version *n* is driven to termination by version *n* after *n+1* is published and that no
Orders code path reads any version but the bound one; a test per protected operation that its
precondition guard refuses with the catalogue reason when its record precondition does not hold; a
test that an invocation ending without `terminate-instance` is surfaced by `settle-from-lookup` as
a manual task; and a startup test that `owf_step_operation` is loaded from the compiled registry,
audited on load and rejects any runtime write.

## Pros and Cons of the Options

### No fence

* Good, because nothing stands between an operator and a publish; the adjustability contract is maximal.
* Bad, because a schema-valid definition can omit a `p1` guard, and the run-time guards only catch what a protected operation can see from its own record — an omitted operation is not called and cannot refuse.
* Bad, because it makes every platform operator a de-facto author of this gear's `p1` invariants, with no evidence trail.

### Review-only fence

* Good, because a reviewer can judge suitability, which no rule can.
* Bad, because a human cannot reliably check a nesting inequality, a reference schema or the presence of twenty-two protected operations in order across a long document; the checks that matter most are the mechanical ones.
* Bad, because the platform's review/approval governance does not exist yet (NEXT_ADR_SCOPE.md line 48), so the fence would be procedural until it does, with no artifact a test can assert.

### Validation-hook fence (chosen)

* Good, because the rules see the whole path and fail closed before execution, and the same rules run in CI so a canonical definition cannot drift from the operations it calls.
* Good, because `composable` changes publish freely: the rules constrain the protected skeleton and the boundary, not the arms an operator is meant to adjust.
* Neutral, because the hook is an upstream ask; until it lands the fence at publish time is CI plus a restricted publish role, with the run-time guards underneath.
* Bad, because a rule set is a contract that must be maintained alongside the operations: every change to a protected operation's order constraints is a change to the validator and to the canonical definitions.

## More Information

This decision refines ADR-0011 and is the home of the `protected` list; `design/10-process-definition.md`
carries the canonical definitions and the rule text in normative form, and each slice's §3.3
declares its operations' `protection`. The register entries carrying it are in `DECISIONS.md`
from D-65 onward; Q-10 is opened here. The platform's function lifecycle (`draft → active →
deprecated → archived → deleted`, DESIGN.md lines 586–610) and its versioning model (line 614)
apply by reference.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — §5.2 (no migration), §6.1 (definition version recorded on the
  instance and in the audit trail; closed trigger set), §6.3, §6.4, §7.1
- **DESIGN**: [DESIGN.md](../DESIGN.md) §2.2, §4.2, §4.7;
  [`design/10-process-definition.md`](../design/10-process-definition.md);
  [`design/01-foundation.md`](../design/01-foundation.md) §3.7 (`owf_definition_binding`,
  `owf_step_operation`)
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — D-02, D-53, Q-10
- **Upstream asks**: [`UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) — serverless-runtime section
  (pre-publish validation hook, publish audit, version retention while bound)
- **Platform**: serverless-runtime [DESIGN.md](../../../../serverless-runtime/docs/DESIGN.md)
  §3.1 (function lifecycle, versioning), §3.2 (Function Registry), §3.3;
  [NEXT_ADR_SCOPE.md](../../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` — the definition version an instance started with is recorded in `owf_definition_binding` and the instance executes to completion under it; no operator migration exists
* `cpt-cf-bss-orders-workflow-fr-owf-start-contract` — `listen` targets are validated against the closed trigger set, so a definition cannot widen the start contract
* `cpt-cf-bss-orders-workflow-fr-owf-fulfillment-plan` and `cpt-cf-bss-orders-workflow-fr-owf-provisioning-intent` — the plan-freeze and two-wave order constraints are validation rules over the protected operations
* `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution` — fence-before-compensation-before-outcome is a validation rule, and `compensate-order` guards it at run time
* `cpt-cf-bss-orders-workflow-nfr-owf-retention` — a bound definition version must remain resolvable for this gear's audit retention (upstream ask)
* `cpt-cf-bss-orders-workflow-adr-two-wave-activation-barrier`, `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`, `cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park` — their ordering invariants become rules 1 and 6
