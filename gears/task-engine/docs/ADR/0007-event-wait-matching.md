---
status: accepted
date: 2026-10-24
description: "Match incoming events to event wait conditions by type, tenant, and subject first, then by an optional CEL expression that is validated at creation and evaluated under a cost budget enforced inside the evaluator, with per-tenant limits on pending event conditions."
---

# Match Event Wait Conditions with Bounded, Precompiled CEL Expressions

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Type and subject matching only](#option-1-type-and-subject-matching-only)
  - [Option 2: JSONPath / equality filter maps](#option-2-jsonpath--equality-filter-maps)
  - [Option 3: Embedded scripting language](#option-3-embedded-scripting-language)
  - [Option 4: CEL with creation-time validation and evaluation limits](#option-4-cel-with-creation-time-validation-and-evaluation-limits)
  - [Option 5: JSON Logic or JMESPath filters](#option-5-json-logic-or-jmespath-filters)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-event-wait-matching`

## Context and Problem Statement

An event wait condition parks a task until a matching event arrives from the Event Broker. Matching by event type and subject is often not enough: a wait may need a specific field value in the event payload (for example a status or an amount). Expressions are supplied by callers, so they are untrusted input and are evaluated inside the consumer transaction.

We must decide how callers express payload conditions and how the engine evaluates them safely.

## Decision Drivers

* Express payload conditions without custom code.
* Evaluation must be safe against untrusted, expensive, or malicious expressions.
* Invalid expressions must be rejected when the wait is created, not when an event arrives.
* Matching must never cross tenants.
* Evaluation must be fast enough for the consumer hot path.
* One tenant's waits must not slow event matching for other tenants; the consumer is shared.
* A task must never wait for ever on an expression that cannot finish.

## Considered Options

1. **Type and subject matching only** — match only on `event_type` and optional subject id/type.
2. **JSONPath / equality filter maps** — a list of field-path equality checks.
3. **Embedded scripting language** — run caller scripts (for example Lua or JavaScript).
4. **CEL with creation-time validation and evaluation limits** — an optional CEL `match_expression`, parsed and checked at creation, evaluated after the cheap filters under a cost budget and AST limits.
5. **JSON Logic or JMESPath filters** — a JSON-encoded filter language evaluated against the event.

## Decision Outcome

Chosen option: **Option 4 — CEL with creation-time validation and evaluation limits.**

An event wait condition stores `event_type`, an optional `event_subject_id`, an effective `event_subject_type` (explicit, or the event type's single allowed subject type), the `event_consume_filter` kept from the caller's set-time `consume` checks (DESIGN §3.2 *Event wait authorization*), and an optional CEL `match_expression`. The expression is parsed and checked when the wait is created, with the workspace `cel-core` parser and checker; an invalid or over-complex expression is rejected with `invalid_argument`. Size and complexity are bounded by expression length (4 KiB, the Event Broker filter limit), AST node count, and AST depth.

When an event arrives, the consumer selects pending event waits by `event_type`, keeps only those whose `owner_tenant_id` equals the event tenant, applies the subject filters and the consume filter, and evaluates the expression last. The expression therefore only ever sees the `data` of events the caller could consume through its own broker subscription, so a match reveals nothing beyond the caller's rights.

**Evaluation budget.** Each evaluation runs under a fixed cost budget (`cel.max_eval_cost`), charged per node and per byte copied and enforced from **inside** the evaluator, as the workspace `quota-enforcement-engine-cel` runtime does. A wall-clock timeout is not used: it cannot preempt a synchronous evaluation, so it would bound nothing.

**Outcomes** (this resolves the open point of the earlier draft):

* `true` → match; the condition is marked `satisfied` in the consumer transaction (ADR-0004).
* Any other value, or a run-time error (missing field, type mismatch) → no match, nothing recorded. Events of one type often differ in shape, so an error is not evidence of a broken expression.
* Budget exceeded → no match, and the condition's evaluation-failure counter goes up by one; a matching evaluation resets it. After `event_waits.max_eval_failures` consecutive failures (default 10) the condition resolves as if it had timed out, with reason `eval_budget_exceeded`, and its `on_timeout` applies. A task is never left waiting for ever on an expression that cannot finish, and one slow event never fails a wait on its own.

**Bounded work per tenant.** One consumer serves all tenants, so the engine limits pending event conditions per tenant, per tenant and event type, and — tighter — per tenant and event type for conditions without a subject, which every event of the type must be checked against (PRD §5.4). Setting or arming over a limit is refused with `429` `EVENT_WAIT_LIMIT`. With these limits and the per-evaluation budget, the worst-case matching work for one event is fixed by configuration, whatever a tenant does.

### Consequences

* Callers can express payload conditions without custom engine code.
* Expressions can never run unbounded or cause side effects; their cost is bounded by the evaluator itself.
* The matching work one event can cause is bounded by configuration, so one tenant cannot slow event waits for others.
* An expression that keeps running out of budget resolves its condition instead of parking the task for ever.
* The engine and the Event Broker use the same expression language and the same expression-size limit.
* Events from one tenant never match another tenant's wait.
* Most events are discarded by indexed filters before any expression runs.
* The CEL dialect and its limits become part of the public API contract.

### Confirmation

Tests MUST check that:

* invalid and over-complex expressions are rejected at creation;
* evaluation stops when the cost budget is spent and is treated as no match;
* a run-time error is treated as no match and does not count as an evaluation failure;
* after the configured number of consecutive budget failures the condition resolves with reason `eval_budget_exceeded` and applies `on_timeout`;
* setting or arming an event wait over a per-tenant limit returns `429` `EVENT_WAIT_LIMIT` and changes nothing;
* events never match waits of another tenant;
* subject filters and expressions combine correctly.

## Pros and Cons of the Options

### Option 1: Type and subject matching only

No payload conditions.

* Good, because it is trivial and fast.
* Bad, because waits that depend on payload values cannot be expressed; callers would resolve them by hand.

**Rejected.**

### Option 2: JSONPath / equality filter maps

Callers supply `{path: value}` pairs.

* Good, because it is simple and safe.
* Bad, because it cannot express ranges, boolean logic, or membership.
* Bad, because it would grow into an ad-hoc expression language over time.

**Rejected.**

### Option 3: Embedded scripting language

Full scripting inside the engine.

* Good, because anything can be expressed.
* Bad, because sandboxing, termination, and resource limits are hard to guarantee.
* Bad, because it is far more power than matching needs.

**Rejected.**

### Option 4: CEL with creation-time validation and evaluation limits

CEL is a non-Turing-complete, side-effect-free expression language. The engine first narrows candidates by `event_type`, then requires `owner_tenant_id` = event tenant, then applies subject filters, and only then evaluates the expression.

* Good, because CEL is designed for safe evaluation of untrusted expressions.
* Good, because invalid expressions fail at creation.
* Good, because cheap indexed filters run before any expression.
* Bad, because it needs a budgeted evaluator; the workspace already pins `cel-core` and has one in `quota-enforcement-engine-cel`, which should be shared rather than copied.

**Chosen.**

### Option 5: JSON Logic or JMESPath filters

Callers send a structured filter document instead of an expression string.

* Good, because the filter is plain JSON and easy to validate.
* Good, because evaluation is simple and bounded.
* Bad, because typing and functions are weaker than CEL.
* Bad, because the Event Broker already uses CEL for subscription filters (`gts.cf.core.events.filter.v1~cf.core.expression.cel.v1`), so a second language would split the platform.

**Rejected.**

## More Information

Reference: CEL specification <https://github.com/google/cel-spec>. See DESIGN §3.1 EventWait, §3.2 Event Consumer, and §4.5 CEL expression safety.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-wait-event-expression` — CEL match expressions for event waits.
* `cpt-cf-task-engine-fr-wait-event` — event wait matching by type and subject.
* `cpt-cf-task-engine-nfr-security` — untrusted expressions are bounded and side-effect-free.
* `cpt-cf-task-engine-nfr-tenant-isolation` — matching never crosses tenants.
* `cpt-cf-task-engine-usecase-event-wait` — a task resumes on a matching event.
