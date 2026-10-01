---
status: proposed
date: 2026-09-30
description: Exactly one audit plugin receives a request hand-off before dispatch and a final-result hand-off after the outcome, each attempted once, and the audit plugin retries until recorded
---
# Attempt Each Hand-Off Exactly Once


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Exactly one audit plugin, two hand-offs each attempted once, retries owned by the audit plugin](#exactly-one-audit-plugin-two-hand-offs-each-attempted-once-retries-owned-by-the-audit-plugin)
  - [A gear-owned audit table](#a-gear-owned-audit-table)
  - [A single audit call after dispatch](#a-single-audit-call-after-dispatch)
  - [Application Manager-side audit retries](#application-manager-side-audit-retries)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-audit-via-plugin`

## Context and Problem Statement

Every seed and uninstall request that reaches the system must leave a trace of what was sent, by whom, and what each section plugin did, including requests that end before dispatch. The trace holds full manifests and caller subjects, so it is sensitive. Who stores it, who guarantees it is recorded, and how many times the Application Manager hands it over are open design questions.

How should the Application Manager produce the audit trail without owning audit storage, without a heavier seeding path, and without gaps or duplicates?

## Decision Drivers

* Completeness: every seed and uninstall request that reaches the system is traced, including not permitted, not found, invalid manifest, and temporarily unavailable ones (`cpt-cf-application-manager-nfr-audit-completeness`).
* No change without a trace: nothing is dispatched unless the request was handed to audit first (`cpt-cf-application-manager-fr-audit-ordering`).
* Ownership: the Application Manager owns only its registry. Audit storage, retention, cleanup, and exposure belong to one audit plugin, and audit reads are not offered by the gear (`cpt-cf-application-manager-fr-audit-plugin`).
* Sensitivity: audit events carry full manifests, so exposure must be off by default and controlled by the audit plugin (`cpt-cf-application-manager-fr-audit-exposure`).
* Simplicity and no duplicates: exactly one party owns retries (`cpt-cf-application-manager-fr-audit-single-handoff`).
* Availability: reads must survive an audit outage (`cpt-cf-application-manager-nfr-availability`).
* Auditability of the trail itself: a final result must be attachable to the request it closes, even when the outcome is decided without dispatch.

## Considered Options

* Exactly one audit plugin, two hand-offs each attempted once, retries owned by the audit plugin
* A gear-owned audit table
* A single audit call after dispatch
* Application Manager-side audit retries

## Decision Outcome

Chosen option: "Exactly one audit plugin, two hand-offs each attempted once, retries owned by the audit plugin", because it is the only option that gives a trace before any change, gives every request that reaches the system an audit event, keeps audit data out of the Application Manager, and leaves exactly one party responsible for durability.

The decision has these parts:

* **One audit plugin.** The Application Manager resolves the audit plugin through the platform plugin model (scoped registration by GTS id) at startup. Exactly one audit plugin must be registered: startup fails when none is available and when more than one is registered, with the same strictness as duplicate section keys. There is no priority-based choice among audit plugins; the single registered instance is used. Unlike section plugins, which all take part in fan-out ([ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)), the audit trail is served by that single plugin, so there is one trail.
* **Request hand-off, made before any dispatch.** It is attempted for every request that reaches the system, including one already decided as temporarily unavailable, not permitted, or not found. Nothing is dispatched unless the audit plugin accepted it. If it is not accepted, the request ends as temporarily unavailable, or keeps an already decided category, and the failure is logged.
* **Final-result hand-off.** It is attempted once for every request whose request hand-off was accepted, after the outcome is decided, including requests that end without dispatch. If it fails, the caller still gets the result and the failure is logged.
* **No retries by the Application Manager.** The audit plugin is obliged to retry internally until every hand-off it accepted is recorded in the request's audit event, and to keep a completed event unaltered.
* **Plugin-owned data.** Storage, retention, cleanup, exposure, and erasure are the audit plugin's. The Application Manager has no audit read API. Requests the platform rejects before they reach the system (request protection such as body limits, content-type checks and throttling, and authentication, scope and license checks) are logged by the platform, not audited. A request that reaches a replica before it is ready is not exempt: it counts as an attempted request hand-off that was not accepted, is logged, and ends as temporarily unavailable.
* **Reads stay available.** Listing and section-key discovery make no audit call, and schema reads make no audit call either, so an audit outage does not affect them.

The only case with no audit event, for a request that reaches the system, is a request whose request hand-off the audit plugin cannot accept (platform-rejected requests are outside this scope). A request hand-off that timed out counts as not accepted, but the audit plugin may already have recorded it (see Consequences).

Decision scope: this ADR covers which audit plugin is used, when each of the two hand-offs is attempted, who retries, and who owns audit data. It does not decide the rank of the audit-unavailable outcome, which `cpt-cf-application-manager-fr-outcome-precedence` sets, its HTTP status ([ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md)), dispatch order ([ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md)), or what the registry stores ([ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md)).

### Consequences

* The audit plugin contract must state these obligations (`cpt-cf-application-manager-contract-audit-plugin`): retry until recorded, immutability, documentation, erasure, default-off exposure, marking an event that never receives its final result as "final result unknown" (the audit plugin decides when), and treating a request hand-off that may have timed out on the Application Manager's side as possibly recorded, so that events recorded from such hand-offs are identifiable. Audit-plugin authors own a contract conformance test that proves them.
* The Application Manager's half of the guarantee is testable by counting: attempted request hand-offs equal the seed and uninstall requests that reach the system, and attempted final-result hand-offs equal accepted request hand-offs.
* An audit outage blocks seeding and uninstall for requests that would otherwise proceed, because the request hand-off gates dispatch. This is deliberate: an untraced change is worse than a retryable refusal. HTTP status mapping for this outcome is set in [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md), and its rank among other categories follows `cpt-cf-application-manager-fr-outcome-precedence`: after access control, registry storage, permission, and not-found decisions, and before invalid manifest.
* Plugin calls to the audit plugin, including hand-offs, share the one configurable time limit (`cpt-cf-application-manager-fr-bounded-plugin-calls`). A timed-out request hand-off counts as not accepted.
* Accepted consequence, open event: an event whose request hand-off was accepted but whose final-result hand-off fails, or never comes because the system stops between the two hand-offs, stays open without its final result. The Application Manager does not retry or repair it; the audit plugin marks it as "final result unknown".
* Accepted consequence, orphan event: a request hand-off that timed out counts as not accepted, so nothing is dispatched and the caller gets no audit id, but the audit plugin may already have recorded it. The result is an orphan event whose audit id the caller never saw; the audit plugin keeps such events identifiable.
* The audit id is returned to the caller only when the request hand-off was accepted, and is stored in the registry entry of the seeded application ([ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md)).
* Because the Application Manager never retries, an accepted hand-off is safe from duplicates only if the audit plugin is reliable; swapping the audit plugin changes durability and exposure, which the security considerations of the [DESIGN](../DESIGN.md) call out.
* The audit event content (identity, subject, application, validation result, per-section outcomes with schema identity, overall result, full manifest) is decided by the Application Manager and passed as-is. Section outcomes come from the ordered dispatch of [ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md), and the schema identity from the per-seed fetch of [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md).

Impact by concern:

* Performance: a bounded extra plugin call at the start and end of each request, acceptable for infrequent deploy-time requests.
* Security: follows from plugin-owned, default-off exposure and unaltered events.
* Reliability: an audit outage blocks seeding and uninstall but not reads; durability of recorded events depends on the audit plugin.
* Data: the gear stores no audit data; storage, retention, cleanup, exposure, and erasure are the audit plugin's.
* Integration: one more plugin contract (`cpt-cf-application-manager-contract-audit-plugin`), resolved through the platform plugin model.
* Operations: failed hand-offs need visible signals (`cpt-cf-application-manager-nfr-operational-signals`), because a failed final-result hand-off does not change the caller's response.
* Testing: see Confirmation.
* Compliance: follows from plugin-owned, default-off exposure and unaltered events. Regulatory obligations for stored content belong to the audit plugin.
* UX: limited to clear, retryable refusals when audit is down.
* Business: environments choose their own audit plugin; the cost is that seeding waits while the audit plugin is down.

### Confirmation

* Automated tests with a test audit plugin that counts hand-offs, refuses them, and exceeds the time limit, asserting no dispatch when the request hand-off is not accepted, and no missing or duplicate hand-off in any outcome category.
* Startup tests: no audit plugin registered (fails), more than one audit plugin registered (fails), exactly one registered (starts).
* A contract conformance test, owned by audit-plugin authors, for retry-until-recorded, immutability, marking an event that never receives its final result as "final result unknown", and keeping events recorded from a request hand-off that timed out on the Application Manager's side identifiable.
* Tests that keep the audit plugin unavailable while listing and discovery succeed.
* Inspection that the public surface offers no operation that returns audit data and that the gear has no audit storage of its own.
* Review of record: the [DESIGN](../DESIGN.md) matches this record.

## Pros and Cons of the Options

### Exactly one audit plugin, two hand-offs each attempted once, retries owned by the audit plugin

The Application Manager hands the request over before dispatch and the final result after the outcome, each attempted once. The audit plugin records and retries.

* Good, because a trace exists before any change happens.
* Good, because rejected and unavailable requests are traced too, with no special case.
* Good, because the final result closes the event for requests that never dispatch.
* Good, because one party owns retries, so there are no duplicate events and the seeding path stays simple.
* Good, because storage, retention, and exposure of sensitive manifests stay outside the gear, and the plugin can be swapped per environment.
* Neutral, because the durability guarantee is split: the Application Manager guarantees the hand-offs, and the audit plugin guarantees recording.
* Bad, because an audit outage blocks seeding and uninstall.
* Bad, because a failed final-result hand-off, or a process stop between the two hand-offs, leaves an event open without its final result, which the audit plugin can only mark as "final result unknown".
* Bad, because a timed-out request hand-off can leave an orphan event whose audit id the caller never sees.

### A gear-owned audit table

The Application Manager writes its own audit rows, as some other gears do with a gear-owned sink.

* Good, because the write can be made in the same storage transaction as the registry, and no extra plugin is needed.
* Bad, because the gear would own storage, retention, cleanup, exposure, and erasure of full manifests, which contradicts the small-registry scope and `cpt-cf-application-manager-fr-audit-plugin`.
* Bad, because environments could not choose their own audit backend or compliance handling.
* Bad, because a gear-owned table would tempt an audit read API, widening the surface and the leak risk for other section plugins' data.

### A single audit call after dispatch

The Application Manager makes one audit call once the outcome is known.

* Good, because it is the simplest sequence, with one call and the complete event at once.
* Bad, because changes can be dispatched with no trace at all if the call fails or the process stops between dispatch and the call. The chosen option shares the process-stop exposure only for the final result, because its request is already recorded before dispatch.
* Bad, because there is no audit id before dispatch, so it cannot gate dispatch or be returned when the request ends early.
* Bad, because a failure of this call cannot be told apart from a lost event without a request-level record, weakening completeness.

### Application Manager-side audit retries

The Application Manager retries failed hand-offs until they are accepted.

* Good, because a transient audit fault would rarely surface to the caller.
* Bad, because retries lengthen requests and conflict with the bounded time limit on every plugin call.
* Bad, because the Application Manager would need to hold state or a queue to survive restarts, or accept loss, and it is meant to be stateless apart from the registry.
* Bad, because retries after an ambiguous failure can create duplicate events, while the audit plugin is better placed to make recording idempotent.
* Bad, because two parties would then share responsibility for durability.

## More Information

* Concurrent seeds for the same application id do not change this decision. Each request has its own hand-offs and audit event either way. The registry side is resolved in [DESIGN §4.2](../DESIGN.md#42-concurrent-requests-for-the-same-application): the last registry writer wins.
* Review this ADR if the platform offers a durable, transactional outbox for plugin calls, if an environment needs more than one audit sink, or if audit completeness requirements change. Supersession would be recorded by a new ADR.
* Related decisions: [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md), [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md), [ADR 0003](./0003-cpt-cf-application-manager-adr-validate-all-then-dispatch.md), [ADR 0005](./0005-cpt-cf-application-manager-adr-minimal-application-registry.md), [ADR 0006](./0006-cpt-cf-application-manager-adr-outcome-http-mapping.md).
* Platform plugin model background: [ClientHub and plugins](../../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-audit-event` — defines the content passed through both hand-offs for every request that reaches the system.
* `cpt-cf-application-manager-fr-audit-ordering` — request hand-off gates dispatch; final-result hand-off closes every accepted request.
* `cpt-cf-application-manager-fr-audit-single-handoff` — each hand-off is attempted exactly once and never retried by the system.
* `cpt-cf-application-manager-fr-audit-plugin` — exactly one audit plugin owns storage, retention, cleanup, and exposure; no audit reads.
* `cpt-cf-application-manager-fr-audit-exposure` — manifest bodies unexposed by default, enforced through the contract.
* `cpt-cf-application-manager-fr-bounded-plugin-calls` — hand-offs share the configurable time limit.
* `cpt-cf-application-manager-fr-outcome-precedence` — ranks the audit-unavailable outcome among the categories.
* `cpt-cf-application-manager-fr-startup-checks` — startup fails when no audit plugin is available or more than one is registered.
* `cpt-cf-application-manager-nfr-audit-completeness` — counting thresholds for hand-offs and logged failures.
* `cpt-cf-application-manager-nfr-availability` — listing and discovery stay available during an audit outage.
* `cpt-cf-application-manager-nfr-operational-signals` — failed hand-offs raise an operator-visible signal.
* `cpt-cf-application-manager-contract-audit-plugin` — retry, immutability, documentation, exposure, "final result unknown" and possibly-recorded obligations of the audit plugin.
* `cpt-cf-application-manager-actor-audit-plugin` — receives the hand-offs and records the events.
* `cpt-cf-application-manager-actor-audit-plugin-author` — owns the contract obligations and conformance test.
* `cpt-cf-application-manager-actor-security-reviewer` — reviews completeness and protection of audit data.
* `cpt-cf-application-manager-usecase-audit-recording` — the reliable audit recording scenario this decision realizes.
