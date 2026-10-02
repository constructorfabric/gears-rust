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
     `evaluate-payment-auth-eligibility` precedes `construct-and-freeze-plan`, which precedes
     `begin-fulfillment` (a settled `eligible` of the same round and a settled `frozen`, as
     `design/10-process-definition.md` §4.1 and `design/04-fulfillment-plan.md` §4.8 order them);
     `begin-fulfillment` precedes `dispatch-wave1-create`, which precedes
     `re-check-pre-activation`, `report-spawn-signal` and `dispatch-wave2-activate` in that order;
     each wave is one `call` carrying `lineRefs[]`, and `dispatch-wave2-activate` follows both an
     `evaluate-activation-eligibility` that answered `released` and the expected-fulfillment
     `wait` (the ADR-0004 conjunction);
     `run-cancellation-fence` precedes `compensate-order`, which precedes `report-outcome`;
     `terminate-instance` is the last operation on every terminal path; `start-instance` is the
     first operation of every instance;
  2. every `call` targets an operation registered in `owf_step_operation`, and a `call` to a
     registered Function targets only a `composable` operation;
  3. every `listen` target is within the closed set: the nine Lifecycle triggers, the approval
     decision, the Subscriptions confirmation and failure events, Orders' own two terminal process
     events (allowed, but unused by the canonical definition, whose overdue monitor stops with the
     invocation, `design/10-process-definition.md` §2.2) and the operator signals of `design/10-process-definition.md`
     §3.3 (`cancel-requested`, `reauthorize-requested`, `task-resolution-requested`,
     `unpark-requested`);
  4. bounds nest: per-operation deadline < task retry budget < task timeout < the overdue window
     < the lifetime ceiling (D-02, D-53, D-67), and every `wait` has a bound;
  5. no payload field crosses: every task input and output conforms to the reference schema the
     operation declares (ADR-0013), checked structurally against the declared `input` and `output`;
  6. no `protected` operation is wrapped in a `catch` that swallows its failure: a `catch` around a
     protected operation may only `raise`, call `park`, call `create-manual-task`, or route to the
     compensation path; it may not continue the forward path.
  `design/10-process-definition.md` §2.2 states these six rules as **eight** numbered checks: it
  adds the grammar subset (no `run`, `emit` or `for`; the two start triggers) and the fork-routing
  convention (every competing `fork` followed by a `switch` on `arm`) as rules 7 and 8, which
  restate this ADR's grammar and D-80's routing rule rather than adding a fence (D-67).

  > **Amended 2026-09-26 by D-126**: the rules are restated where a validator could not check
  > them as written (`design/10-process-definition.md` §2.2, §4.1, §4.6).
  > **Rule 1**: `start-instance` is the first operation after `admit-trigger` (`role: start`),
  > which precedes it; the ADR-0004 conjunction is a row of the §4.1 fence table, not only the
  > run-time guard; and "the whole path" is every walk of a routing graph the check enumerates:
  > the routing members (`nextStage`, `stageLoop`, the return members, `arm`) may be written only
  > as literals or copies of one another, so their values form a finite set, and a walk the check
  > cannot decide is refused. The run-time guards stay the backstop they were.
  > **Rule 2**: every endpoint is `$context.stepsBase + "/<operation>"`, with `stepsBase` written
  > only by the definition's `input.from` and compared with the environment's step-surface base.
  > **Rule 3**: the `OrderAmended` filter correlates on `orderId` only.
  > **Rule 4**: the nesting is checked over values the definition holds — each operation's
  > deadline < the task timeout, and every task timeout and `wait` < the literal `P90D` lifetime
  > ceiling; no cumulative backoff is computed, because the DSL gives exponential backoff no
  > multiplier; the overdue window and the SLA classes are per-order policy values, bounded where
  > the policy is validated (`design/07-manual-tasks.md` §4.8 item 8).
  > **Rule 6**: a `catch` around a protected operation either carries only `retry` — exhaustion
  > faults the invocation, which the instance liveness pass raises as the `invocation-dead` task
  > (D-105, D-114); a `catch` that only `raise`s is the same — or routes to one of the named
  > failure routes of `design/10-process-definition.md` §4.6: a wave's line tasks,
  > `compensate-order`'s next pass, `reflect-verdict`'s order task, the start path's supersession
  > retry. It may not route a protected failure to `park` (`design/08-hold-and-cancel.md` §4.7
  > item 7). That a retry-only `catch` re-raises once its limit is spent is Q-11 (vi). The four
  > destinations of *Consequences* are accordingly a fault of the invocation, a manual task, the
  > compensation path's next pass and the supersession wait.

  > **Amended 2026-09-26 by D-135 and D-136**: **Rule 1** also requires the `p1` composables on
  > their paths, which stay `composable` (22 `protected`, 13 `composable` are unchanged): on an
  > `unobtainable` verdict, `park` and a park loop calling `arm-park-escalation` with a route to
  > `raise-overdue-escalation` (`park`); on a pending-approval reflection, `open-gates` before
  > `record-decision` and a gate wait carrying the decision `listen` and the `escalate-gate` fire
  > and probe, with a route to `raise-overdue-escalation` (`approval-outage`); after every manual
  > task, a wait carrying its resolution `listen` and SLA check through `resolve-manual-task`. The
  > check tracks four pinned members — `verdict`, `reflected`, `policy`, `forceTask` — written
  > only as copies of their operation's output or as literals, so a `switch` over the seller's
  > pinned partial-failure policy is decided, and `remediate` must reach `create-manual-task`
  > before any terminal outcome (`design/10-process-definition.md` §2.2 rule 1, §4.1). Rule 8
  > adds the shared arms every stage `fork` carries. **Rule 2**: a `call` to a registered
  > Function is no longer allowed anywhere, so the "`composable` only" clause is withdrawn.
  > Each slice's constraints are split into items enforced through a rule and
  > canonical-definition guidance that the behavioural gate asserts (§4.7).

  > **Amended 2026-09-26 by D-142 and D-143**: the check tracks a fifth pinned member,
  > `beginResult`, a copy of `begin-fulfillment`'s answer, and refuses a version on which any walk
  > reaches a wave dispatch without `beginResult = in-fulfillment`. Rule 8 also forbids a plain
  > `wait` inside a stage, so every stage wait carries the shared arms
  > (`design/10-process-definition.md` §2.2 rules 1 and 8, §4.1).

  > **Amended 2026-09-26 by D-144**: the routing members are the full list of rule 1 —
  > `nextStage`, `stageLoop`, `returnStage`, `taskReturnStage`, `taskReturnLoop`,
  > `ceilingReturnStage`, `ceilingReturnLoop`, `heldStage`, `heldLoop` and `arm` — and each may be
  > written as a literal, a copy of another routing member, or an `if … then … else` over routing
  > members that yields one of those, as the canonical `ceilingEntry` does; the D-126 block above
  > named only the first two forms. The check tracks ten pinned members: the five above, plus
  > `released` (a copy of `evaluate-activation-eligibility`'s answer), and `spawned`, `planFailed`,
  > `preAdmitted` and `failureScope`, each written only as a literal in the task one case of a
  > `switch` over an operation's closed enum routes to. The ADR-0004 conjunction of Rule 1 is
  > checked as a walk to `dispatch-wave2-activate` that passes the pinned `released = true`
  > (`design/10-process-definition.md` §2.2 rule 1, §4.1).

  > **Amended 2026-09-26 by D-161**: the routing members add `ceilingReturnBack`, the interrupted
  > stage's `returnStage` that `ceilingEntry` records and `backToProcess` restores. The D-144 block
  > above stated the pinned-member writes more broadly than rule 1 and the canonical allow; they
  > are rule 1's forms: `spawned`, `planFailed` and `preAdmitted` are boolean literals, each
  > `false` where it is initialised or consumed (`preAdmitted` also in every arm that writes
  > `lifecycleEventId`) and `true` only in the task one case of a `switch` over an operation's
  > closed enum routes to; `failureScope` is one of the literals `line`, `plan` and `order` in the
  > task that enters the failure stage, whatever case routes there
  > (`design/10-process-definition.md` §2.2 rule 1).

  > **Amended 2026-09-26 by D-162**: Rule 4 has one computed sum. A task under the `gate` timeout,
  > a term of the escalation bound, carries the `gate` retry policy, whose backoff is `constant`,
  > and the check computes attempts × the largest `deadline_ms` + (attempts − 1) × (delay + jitter
  > maximum) < the `gate` timeout — 55 s < 60 s in the canonical
  > (`design/10-process-definition.md` §2.2 rule 4).

  > **Amended 2026-09-28 by D-190**: **Rule 6**'s named failure routes no longer include
  > `reflect-verdict`'s order task, which the D-126 block above lists. A Lifecycle refusal of the
  > reflection is the settled answer `refused`, and the definition routes it on the output to that
  > task, so no `catch` around `reflect-verdict` names a 400 and a genuine 400 of it faults the
  > invocation (`design/10-process-definition.md` §3.6 (a), §4.6).

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
  `raise-overdue-escalation`. The list is **22 `protected` and 13 `composable`** operations,
  35 in all, and matches the `protection` field each slice's §3.3 declares (slice 03: only
  `obtain-verdict`, `reflect-verdict` and `record-decision` protected; slice 05: `reread-draft-liveness`,
  `rebuild-wave1` and `reconcile-intent` composable; slice 07: only `create-manual-task`
  protected). A `protected` operation may be ordered by a definition; it may never be omitted or
  replaced.
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

  > **Amended 2026-09-26 by D-105 and D-106**: `run-cancellation-fence` also refuses without a
  > recorded cause for its trigger, and every step call after `start-instance` must carry the
  > instance's bound invocation (D-106). An instance whose invocation ends without
  > `terminate-instance` is found by the sweep's instance liveness pass, not by
  > `settle-from-lookup`, and raised as an `invocation-dead` manual task (D-105).
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

  > **Amended 2026-09-26 by D-138**: the platform-operator role is held by one publisher, the
  > Orders definition publish job, a pipeline job of its own that builds and deploys nothing. A
  > merge to `definitions/` needs its code owners (the Orders definition owners) and a second
  > reviewer. The job runs the rules, then the **behavioural gate** — the candidate published
  > in a non-production environment and driven down each path (a)–(f) against the real step
  > surface — then publishes it and deprecates the version it replaces. It never archives or
  > deletes, rolls back by publishing the last good document as a new version, and re-points both
  > trigger bindings on a major bump. Which version an event trigger starts, and a tenant-scoped
  > activation, are the ask `…-upreq-serverless-runtime-trigger-version-selection`
  > (`design/10-process-definition.md` §4.2).

  > **Amended 2026-09-26 by D-158 and D-159**: the rollback's document carries the new version in
  > its own `document.version`, since D-137 refuses a document whose `version` differs from the
  > pinned one; the forward rollback is kept even though the registry allows
  > `deprecated → disabled → active`, because that path disables a version while instances pinned
  > to it are live. A failing behavioural gate rolls its environment back the same way, and the
  > first environment is published once. The job also compares every literal tick with its
  > guidance value, and refuses a candidate whose `wave1` timeout is not below every effective
  > overdue window of the environment (`design/10-process-definition.md` §4.2).
* **Audit of publishes.** Every publish must be attributable to an actor, a version and a
  validation result. The platform's audit of definition changes is unaddressed (NEXT_ADR_SCOPE.md
  line 26, BR-034) and is raised as an upstream ask; until it lands, the CI conformance run over
  the canonical definitions and the registry's version listing (DESIGN.md line 857, *list
  versions*) are the evidence, and `owf_definition_binding.published_by` records, per instance,
  who published the version it was bound to.

  > **Amended 2026-09-26 by D-137**: the registry carries no publisher per version (its callable
  > entities have `owner`, `created_at` and `updated_at`), so `published_by` is nullable and stays
  > null until the hook ask's publish audit reports one; the evidence meanwhile is the publish
  > job's run and the version listing. `start-instance` records `definition_id` and
  > `definition_version` from the platform's invocation record (`function_id`,
  > `function_version`), never from the document's self-declared `version`, which it only
  > compares; a mismatch is `definition-not-bound`. The hook ask also asks, as an interim, that
  > only the publish job's identity may publish `order_process`.

### Consequences

* `owf_step_operation` is the source of truth for rule 2 and rule 5: it is loaded at startup from the compiled registry, has no runtime write path and is audited on load, exactly as the former permission table was; a definition can only reference what a release registered.
* The CI conformance test becomes a release gate for this gear and a publish gate for the definitions: a change to a `protected` operation's order constraints fails the test until the canonical definitions are updated in the same change.
* The `protected` list turns ADR-0004's conjunction rule, ADR-0005's fence-before-compensation order and ADR-0007's park exits into checkable statements about a document rather than conventions about code (those ADRs carry dated amendments saying so).
* Operators lose nothing they had: `retry-step` remains an operator operation; the platform's `retry` and `replay` control actions (DESIGN.md lines 888–889) act on the invocation and are not a way around a protected operation's precondition, since the re-invoked operation re-checks it.
* Because a `catch` around a protected operation may not continue the forward path, every failure of a protected operation has one of four destinations — `raise`, park, manual task or compensation — which is the same closed outcome set ADR-0009 and ADR-0005 already require.
* A definition that passes validation can still be operationally wrong (a wait too short for a seller's approval practice, an escalation arm that pages the wrong queue); the fence bounds safety, not suitability. Suitability remains a publish-time review concern and, once the platform provides it, a BR-122 governance concern. **Amended 2026-09-26 by D-138**: suitability is also exercised before production by the publish job's behavioural gate, a scenario run of the candidate per path; a window a seller's practice needs is a seller-policy value, not a definition value (D-134).

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
a manual task (**amended 2026-09-26 by D-105**: by the sweep's instance liveness pass, as an
`invocation-dead` manual task); and a startup test that `owf_step_operation` is loaded from the compiled registry,
audited on load and rejects any runtime write. **Amended 2026-09-26 by D-135, D-137 and D-138**:
the mutation corpus adds, per `p1` composable, a mutant with it dropped from its required path, a
mutant whose policy `switch` tests a literal instead of the pinned `policy`, one stage `fork`
without its hold arm and one `call` to a Function; a test asserts that `start-instance` records
the invocation record's `function_version` and refuses a document whose `version` differs; and
the publish job's behavioural gate must pass before any production publish.

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
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — D-02, D-53, D-67, D-68, D-105, D-106, D-126, D-135, D-136, D-137, D-138, D-142, D-143, D-144, D-158, D-159, D-161, D-162, Q-10
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
