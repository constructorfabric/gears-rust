---
status: proposed
date: 2026-09-30
description: Validate the whole manifest before any dispatch, then dispatch one section at a time in section-plugin priority order, continue after failures, and leave rollback to section plugins
---
# Validate the Whole Manifest, Then Dispatch in Priority Order


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Validate everything first, then dispatch in priority order, continue after failures, rollback owned by section plugins](#validate-everything-first-then-dispatch-in-priority-order-continue-after-failures-rollback-owned-by-section-plugins)
  - [Interleaved per-section validate-and-dispatch](#interleaved-per-section-validate-and-dispatch)
  - [Stop at the first failure](#stop-at-the-first-failure)
  - [Dispatch in manifest order](#dispatch-in-manifest-order)
  - [Application Manager rollback after a failed dispatch](#application-manager-rollback-after-a-failed-dispatch)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-validate-all-then-dispatch`

## Context and Problem Statement

A manifest carries one section for each section plugin, and the Application Manager holds no domain logic. Section plugins can only be applied one call at a time, with no shared transaction across them. The Application Manager must decide when a bad manifest is stopped, in what order sections reach section plugins, what happens after one section plugin fails, and who undoes partial work. How does the Application Manager keep a bad manifest from half-applying and keep dispatch predictable?

## Decision Drivers

* A manifest that is invalid in any part must change nothing anywhere (`cpt-cf-application-manager-fr-validate-before-dispatch`, `cpt-cf-application-manager-nfr-no-partial-on-invalid`).
* One failing section plugin must not stop the others from receiving their data, and behaviour must stay easy to follow and audit (`cpt-cf-application-manager-fr-ordered-dispatch`).
* Section plugin authors must be able to choose where they sit in the order, and the order must not depend on how a caller wrote the manifest (`cpt-cf-application-manager-fr-section-priority`).
* Only a section plugin knows what undoing its own data means. The Application Manager must stay free of domain logic and of any cross-plugin rollback, which is out of scope (`cpt-cf-application-manager-fr-plugin-owned-semantics`).
* Requests are non-interactive deploy-time operations, so simple sequential behaviour is preferred over latency optimisation (`cpt-cf-application-manager-nfr-bounded-request-time`).

## Considered Options

* Validate everything first, then dispatch in priority order, continue after failures, rollback owned by section plugins
* Interleaved per-section validate-and-dispatch
* Stop at the first failure
* Dispatch in manifest order
* Application Manager rollback after a failed dispatch

## Decision Outcome

Chosen option: "Validate everything first, then dispatch in priority order, continue after failures, rollback owned by section plugins", because it is the only combination that prevents a half-applied install caused by a bad manifest, gives every section plugin its data even when a neighbour fails, keeps the order under the control of section plugin authors, and leaves all undo semantics with the only party that understands them.

The decision has three parts.

**1. Validate first.** On a seed, the whole manifest is checked before any section plugin receives data: the envelope, the required and unknown section keys, and each section against the schema its section plugin serves (see [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md)). All problems are collected and reported with their location. If any part is invalid, nothing is dispatched.

**2. Dispatch sequentially in priority order.** After validation succeeds, sections are dispatched one at a time, in the order of the priority each section plugin announced at startup, lowest value first, not in manifest order (see [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)). Uninstall asks every section plugin to remove the application in the same order. Dispatch continues after a failure or a time-limit overrun, and the outcome of each section is recorded. The Application Manager never cancels or undoes work a section plugin has started.

**3. Rollback owned by section plugins.** The Application Manager performs no rollback. If a section plugin needs to undo, replace or ignore earlier data, that is its own logic, reached through the next seed. Rollback that spans section plugins would need a composite plugin, which is out of scope. A retry is a fresh seed.

How the recorded section outcomes and the resulting outcome category are reported to the caller is decided in [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md). The ordering of audit hand-offs relative to these steps is decided in [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md).

Decision scope: this ADR covers when validation runs relative to dispatch, dispatch and removal order, what happens after a section plugin fails, and who is responsible for rollback. It does not decide how section plugins are discovered ([ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)), where schemas come from ([ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md)), the audit hand-off protocol ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md)), what the registry stores ([ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md)), or the outcome-to-HTTP mapping ([ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md)). Handling of concurrent seeds of the same application id is resolved in [DESIGN §4.2](../DESIGN.md#42-concurrent-requests-for-the-same-application): it is left to the section plugins, with no coordination, the last registry writer wins, and both requests are audited.

### Consequences

* Validation needs every schema before the first dispatch, so a seed touches every section plugin at least once before any state changes. A section plugin that cannot serve its schema blocks the whole seed with a retryable temporarily unavailable outcome. This is accepted in exchange for the no-partial-on-invalid guarantee.
* Section plugin authors choose a priority to place themselves in the order, and must treat a priority change as an intentional change of order. Two section plugins may not share a priority (startup check).
* Section plugins must expect that their neighbours may have failed or succeeded, and must own any compensation. The contract offers no batch, transaction, cancel or undo operation.
* A section plugin that fails or times out does not stop the request, so the worst-case request time is the sum of the time limits of the ordered calls. This is acceptable for deploy-time use and is bounded by the configured time limit.
* After a partial failure the caller retries by seeding again. Section plugins therefore need to tolerate a repeated section in the way they choose (idempotency is theirs).
* Uninstall deletes the registry entry once the uninstall is accepted, whatever the section plugins return, so a failed removal is retried by seeding again and then uninstalling again (see [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md)).

Impact by concern:

* Performance: sequential dispatch and a schema fetch per section plugin per seed cost time proportional to the small, bounded set of section plugins, each call capped by the configured time limit. Accepted for deploy-time traffic.
* Security: the access decision is made before any section plugin is called, and a platform access control failure fails closed as temporarily unavailable.
* Reliability: continuing after failures and bounded calls keep one section plugin from hanging or blocking a request. Partial state is expected and is repaired by re-seeding.
* Data: the Application Manager keeps no per-section state. Only the registry entry is updated, per [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md).
* Integration: adds no new call types. It relies on the section plugin contract having only schema, apply and remove operations.
* Operations: because a failed section does not stop the request, the per-section outcomes show every section plugin that failed in the same request.
* Testing: see Confirmation.
* Compliance: the fixed priority order and the recorded per-section outcomes give reviewers a reproducible account of each request. No personal data is added.
* UX: this is a machine-facing interface with no screens.
* Business: fewer half-installed applications and clearer failure handling for application teams.

### Confirmation

* Tests with test section plugins count dispatches for every invalid-manifest case (bad envelope, missing key, unknown key, schema violation) and assert zero dispatches (`cpt-cf-application-manager-nfr-no-partial-on-invalid`).
* Tests with ordered test section plugins assert that dispatch and uninstall follow priority order irrespective of manifest key order, that dispatch continues after failure and after time-limit overrun, and that no call follows a failure of the same section plugin for the same request.
* Inspection of the section plugin contract confirms there is no rollback, cancel or batch operation.

## Pros and Cons of the Options

### Validate everything first, then dispatch in priority order, continue after failures, rollback owned by section plugins

* Good, because a bad manifest can never leave some section plugins updated and others not.
* Good, because every section plugin receives its data even if another fails, so one faulty section plugin does not block unrelated ones.
* Good, because priority order is stable, announced by section plugins and independent of caller formatting, so behaviour is reproducible and auditable.
* Good, because rollback stays with the only party that knows the domain, and the Application Manager remains free of domain logic.
* Neutral, because dispatch is sequential, so request time is the sum of the calls; this suits non-interactive deploy-time use.
* Bad, because a failed request may still leave some sections applied, and cleaning up is left to re-seeding and to section plugin logic.
* Bad, because every seed needs a schema fetch from every section plugin first, so one unreachable section plugin blocks all seeds.

### Interleaved per-section validate-and-dispatch

For each section in priority order, fetch its schema, validate it, and dispatch it before moving on to the next section.

* Good, because the first section reaches its section plugin sooner, and a section plugin whose schema cannot be served blocks only the sections from it onwards.
* Good, because each schema is fetched right before it is used.
* Bad, because a manifest that is invalid in a later section has already changed the earlier section plugins, against `cpt-cf-application-manager-nfr-no-partial-on-invalid`.
* Bad, because the caller learns about problems in later sections only after earlier sections were applied, instead of getting every problem at once with nothing changed.
* Bad, because invalid manifest stops being a pre-dispatch outcome and becomes mixed with dispatch results, which makes the result of a request harder to state and to audit.

### Stop at the first failure

Dispatch in order and stop when a section plugin fails, leaving later sections undispatched.

* Good, because it limits the number of section plugins touched when something is wrong.
* Good, because it is a simple rule to state.
* Bad, because one faulty or slow section plugin would starve every section plugin after it, so unrelated data would be missing until the fault is fixed.
* Bad, because it makes the state after a failure depend on priority, which section plugin authors choose for other reasons, so the same fault has different reach depending on position.
* Bad, because it does not remove the partial state; the sections before the failure are already applied. It only adds more uncertainty about which sections were reached.

### Dispatch in manifest order

Dispatch sections in the order in which they appear in the submitted document.

* Good, because it needs no priority and is the order the caller wrote.
* Bad, because JSON object key order carries no meaning and may change between tools, so behaviour would vary with formatting.
* Bad, because section plugin authors could not rely on running before or after another section plugin, and could not declare a position.
* Bad, because it would make an order that is meant to be an intentional platform choice a side effect of how each application team writes its manifest.

### Application Manager rollback after a failed dispatch

Track what was applied and undo it when a later section fails.

* Good, because on the surface it looks all-or-nothing to the caller.
* Bad, because undoing needs to know what applying meant, which only the section plugin knows; the Application Manager would need domain logic or a compensating operation in every section plugin.
* Bad, because rollback across section plugins is the job of a composite plugin, which is out of scope, and would need the Application Manager to keep per-section applied state, which it must not hold.
* Bad, because undo can itself fail, leaving the same partial state with more moving parts, and it conflicts with the rule that the Application Manager never cancels or undoes started work.

## More Information

* This decision depends on [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md) for the section plugin set and the priority order, [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md) for schemas fetched on every seed and for required sections, [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-via-plugin.md) for the audit hand-offs that surround dispatch, and [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md) for how the last result is recorded. [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md) reports the section outcomes this dispatch produces.
* Review and supersession: this ADR should be revisited if the platform gains a transaction or compensation facility that section plugins can share, if a composite plugin comes into scope, or if a need for parallel dispatch appears. Any such change would supersede this ADR.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-validate-before-dispatch` — validates the whole manifest before any dispatch; an invalid part means nothing is dispatched.
* `cpt-cf-application-manager-fr-required-sections` — a missing or unknown section key makes the whole manifest invalid, and nothing is dispatched.
* `cpt-cf-application-manager-fr-section-priority` — priority fixes dispatch and removal order, lowest value first.
* `cpt-cf-application-manager-fr-ordered-dispatch` — one-at-a-time dispatch that continues after failures and records each section outcome.
* `cpt-cf-application-manager-fr-plugin-owned-semantics` — the Application Manager never undoes anything after a failed dispatch; rollback is the section plugin's.
* `cpt-cf-application-manager-fr-pass-through` — each section plugin receives only its own section with the envelope.
* `cpt-cf-application-manager-fr-bounded-plugin-calls` — a time-limit overrun during dispatch is an internal failure and dispatch continues.
* `cpt-cf-application-manager-fr-dependency-failures` — fail closed before dispatch; a registry failure after dispatch counts as an internal failure.
* `cpt-cf-application-manager-fr-uninstall` — uninstall removes in the same priority order and continues after failures.
* `cpt-cf-application-manager-fr-retry` — retry is a fresh seed.
* `cpt-cf-application-manager-nfr-no-partial-on-invalid` — no section plugin receives data for an invalid manifest.
* `cpt-cf-application-manager-nfr-bounded-request-time` — worst-case request time is bounded by the ordered calls, each capped by the time limit.
* `cpt-cf-application-manager-usecase-first-seed` — the seed flow of validate, then dispatch.
* `cpt-cf-application-manager-usecase-reseed` — each seed is validated and dispatched on its own.
* `cpt-cf-application-manager-usecase-retry` — recovery from a failed or partly failed seed is a new seed.
* `cpt-cf-application-manager-usecase-uninstall` — ordered removal that continues after failures.
* `cpt-cf-application-manager-actor-section-plugin-author` — chooses a priority and owns rollback and re-seed behaviour.
