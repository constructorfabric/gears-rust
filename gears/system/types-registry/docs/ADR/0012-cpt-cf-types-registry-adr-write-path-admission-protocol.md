---
status: accepted
date: 2026-07-26
---

# Write Path and Admission Protocol

**ID**: `cpt-cf-types-registry-adr-write-path-admission-protocol`

## Table of Contents

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Actual mutations are always asynchronous](#actual-mutations-are-always-asynchronous)
  - [Request identity, content equality, and optimistic locking are distinct](#request-identity-content-equality-and-optimistic-locking-are-distinct)
  - [Dry run is a mode of the operation, not an operation of its own](#dry-run-is-a-mode-of-the-operation-not-an-operation-of-its-own)
  - [The public operation has one status and per-candidate results](#the-public-operation-has-one-status-and-per-candidate-results)
  - [Batches use dependency-aware partial admission](#batches-use-dependency-aware-partial-admission)
  - [Startup is caller-side reconciliation, not a global barrier](#startup-is-caller-side-reconciliation-not-a-global-barrier)
  - [Control-plane records are registry entities with built-in validators](#control-plane-records-are-registry-entities-with-built-in-validators)
- [Consequences](#consequences)
- [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Always synchronous](#always-synchronous)
  - [Deadline-based inline completion](#deadline-based-inline-completion)
  - [Synchronous no-op plus asynchronous mutation](#synchronous-no-op-plus-asynchronous-mutation)
  - [Always asynchronous with no synchronous no-op path](#always-asynchronous-with-no-synchronous-no-op-path)
  - [All-or-nothing batch](#all-or-nothing-batch)
  - [Unconstrained best effort](#unconstrained-best-effort)
  - [Dependency-aware partial admission](#dependency-aware-partial-admission)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Registration must support a bounded batch because one candidate may depend on another candidate that does not exist yet. P1 has only local validation, but P2 semantic hooks may outlive an HTTP request, so the first public contract must already support durable asynchronous execution.

Three forms of concurrency have to remain separate:

* a network retry must recover the original request rather than execute it twice;
* a caller must not overwrite a logical entity that changed after the caller read it;
* dependencies may change while expensive resolution and compatibility checks run outside a database transaction.

The common platform-gear case adds a startup constraint. A gear knows the definitions it desires, but Types Registry does not know the complete platform registration set and must not wait for one. The gear therefore needs a read/reconcile/write workflow and gates only its own readiness.

ADR-0002 also leaves Registry Source Plugin configuration as either a Managed Entity or platform configuration. That choice belongs here because it determines whether routing invariants pass through the ordinary admission protocol.

## Scope

This ADR decides:

* the acceptance and asynchronous operation response contract, and whether any acceptance may be synchronous;
* immutable request-key replay and content no-op semantics;
* per-entity optimistic preconditions;
* dependency-aware partial batch admission;
* caller-side startup reconciliation;
* whether control-plane records are registry entities and how P1 validates them.

Route paths, DTO field layout, operation retention, and table definitions belong to DESIGN. Revision identity and retention remain in ADR-0005 and ADR-0006, compatibility in ADR-0003, and ownership and authority in ADR-0009.

## Decision Drivers

* Enabling P2 semantic hooks must not break the client contract.
* A retry after a timeout must recover the same operation or completed response.
* Request identity must not change meaning when registry state changes later.
* A stale writer must not overwrite a newer revision.
* Expensive GTS checks must not hold a database transaction open.
* Independent valid candidates should not fail only because another branch in the same batch is invalid.
* Interdependent candidates, including reference cycles, must be registrable without exposing an inconsistent intermediate state.
* Types Registry must not depend on a global startup census.
* P1 safety must not require P2 hooks.

## Considered Options

For execution:

* always synchronous;
* deadline-based inline completion with an operation fallback;
* synchronous no-op detection and always-asynchronous execution for actual mutations;
* always-asynchronous execution with no synchronous no-op path.

For concurrency:

* content equality alone;
* request idempotency alone;
* request idempotency plus an explicit entity resource-version precondition.

For batch failure:

* all-or-nothing;
* unconstrained best effort;
* dependency-aware partial admission with atomic dependency groups.

For startup:

* a global staged-registration barrier;
* caller-side read, reconcile, conditional registration, retry, and readiness gating.

## Decision Outcome

### Actual mutations are always asynchronous

A new registration request has exactly one successful acceptance result: `202 Accepted` with an operation UUID. The server never returns an admission result inline, whether because P1 validation happens to be fast or because the batch turns out to change nothing.

A synchronous `200 OK / unchanged` acceptance for an all-equal batch is not offered either, and the reason is not economy of response shapes. The state such a path would optimize is unreachable for a caller that honours its own preconditions: `must_not_exist` yields `precondition_failed` once the entity exists, and `match_resource_version(v)` fails once the content has moved — and content can only have become equal by someone writing it, which advances the version past `v`. An all-equal batch therefore means the caller read current state, found equality, and submitted anyway. That is a caller defect, not a workflow, and a caller that reconciled sends no POST at all; the optimization belongs there, where it costs nothing, and §Startup places it there.

`unchanged` is therefore a **guarantee** rather than a path: a redundant submission creates no revision and no `resource_version` increment. Both the operation status and the per-candidate status carry the value for that reason.

The acceptance transaction stores the operation and its candidates and enqueues the operation UUID through the ToolKit transactional outbox. Request identity and workflow state are one record. A separate request record would exist only to hold a synchronous no-op receipt, and with no such path there is no request without an operation. The operation carries the scoped `Idempotency-Key`, its fingerprint, the plane, and the principal. The outbox owns leases, retry, duplicate delivery, and dead letters; the operation owns client-visible progress. A worker performs checks outside a long transaction and commits bounded admission units in short transactions.

This keeps one mutation contract when P2 hooks arrive. Hooks increase operation duration but do not introduce a new response shape — and now no successful acceptance has a second shape to begin with.

### Request identity, content equality, and optimistic locking are distinct

`Idempotency-Key` is mandatory for a registration batch and is scoped to the caller's plane, owning tenant, **and requesting principal**. The principal participates so that one subject's key cannot return another subject's response — and with it another subject's Registry References and resource versions — inside one tenant. Its stored fingerprint covers at least the canonical request body, operation kind, ownership scope, the dry-run mode below, and every candidate precondition.

A repeat with the same scoped key and fingerprint returns the immutable stored operation without re-evaluating current registry state:

* a non-terminal operation returns `202` and the same operation UUID;
* a terminal operation returns `200` and the stored terminal operation.

The same key with a different fingerprint is `409 Conflict`. A completed request never turns back into a new mutation because another request later changed its entities. A new reconciliation cycle uses a new key.

Content equality answers a different question: whether an admitted candidate creates a revision. Equal canonical authored content creates no revision and does not increment `resource_version`; the item status is `unchanged`. Effective resolved artifacts are not part of this comparison because dependency movement may change them without changing authored content.

Optimistic locking answers whether the caller's read is still current. Every candidate carries exactly one precondition:

* `must_not_exist` for an identifier that was absent during reconciliation;
* `match_resource_version(version)` for an existing entity.

The worker enforces the precondition in the commit transaction. A mismatch is a terminal per-item `precondition_failed`; Types Registry does not silently rebase the update. This token is the logical entity's monotonic `resource_version`, not the authored revision number, because lifecycle and other correctness-relevant changes must also invalidate a stale write.

Dependency freshness is internal concurrency control. Validation records the dependency revision vector. If the target precondition still holds but a dependency changes before commit, the worker revalidates within a bounded retry policy. This internal retry does not weaken the caller's target precondition.

### Dry run is a mode of the operation, not an operation of its own

Every mutation kind accepts a dry-run request: it runs the complete check sequence and commits nothing — no logical entity, no revision, no current-pointer move, no `resource_version` advance, no lifecycle transition, no removal. Per-candidate outcomes and diagnostics are what the real operation would have produced against the state observed during the run.

It is a mode rather than a separate validation operation for one reason. A separate operation is a second implementation of the ordered check sequence, and the two drift; a drifted check that passes before deployment and fails at admission is the exact failure a pre-deployment gate exists to prevent. As a mode it is the same code path with the commit suppressed, so drift is not merely discouraged but unrepresentable. It is consequently orthogonal to `kind` rather than three further values in it.

The mode participates in the request fingerprint. Without that, a dry run and the real submission that follows it collapse under one `Idempotency-Key`: the second request replays the first's stored operation and never executes. That is a silent lost write, and it is the reason the fingerprint list above names the mode explicitly.

The acceptance shape does not change. A dry run returns `202` with an operation UUID and is polled like any other, which is not uniformity for its own sake: when P2 hooks exist a dry run **must** invoke every hook the real operation would, or it stops predicting admission precisely where the stakes are highest — and hook duration is unbounded. Giving the mode a synchronous contract in P1 would therefore mean withdrawing it in P2, which is the client-contract break this ADR exists to avoid.

A dry run is not a guarantee of admission and must not be presented as one. Its verdict is relative to the state it observed: a target's `resource_version` may advance, a dependency may admit a new revision, or the entity may be deleted before the real submission. There is also one state in which the check is wanted and admission is impossible by construction — ADR-0003 freezes a logical entity whose revision chain spans a semantic change of the compatibility relation, and requires the check against it to remain answerable together with the unproven-chain state.

The purge dry run of ADR-0013 is this mode applied to `kind = purge`, not a separate facility.

### The public operation has one status and per-candidate results

An operation has one status: `pending`, `running`, `succeeded`, `unchanged`, `partially_succeeded`, `failed`, `cancelled`, or `expired`. Progress and outcome are one field rather than two. Splitting them spreads a tagged union across two fields, because an outcome only ever exists under one progress value; that makes illegal combinations representable and requires a constraint to forbid them. One enumeration makes them unrepresentable, and it matches how a candidate result is modelled.

Each exact candidate GTS Identifier has one result with status `pending`, `running`, `succeeded`, `unchanged`, `failed`, `blocked`, or `cancelled`. `unchanged` is preferred to `not_modified` and `already_registered`: it applies to both create and update attempts and does not overload HTTP `304 Not Modified`. A terminal success also returns the Registry Reference, revision number, and resulting resource version. A failure returns a structured reason.

There is no separate public Admission Status vocabulary and no pending logical entity. An operation is the request-progress resource; an entity exists only after an admission unit commits.

### Batches use dependency-aware partial admission

A batch is one durable operation on one plane, but it is not one all-or-nothing database transaction.

The candidate dependency graph is resolved with the submitted candidates as an overlay. A reference to another candidate never silently falls back to that identifier's previously committed revision. The graph is condensed into strongly connected components and processed in topological order:

* one acyclic candidate is one admission unit;
* one cyclic component is one atomic admission unit;
* independent units that pass all checks commit even if another branch fails;
* a dependent whose selected in-batch dependency fails becomes `blocked`;
* failure of one cyclic member rejects or blocks the whole cyclic unit.

This is called **dependency-aware partial admission**, not best effort. “Best effort” does not state dependency ordering, overlay resolution, or cycle atomicity.

The batch cannot mix ownership scopes. All candidates are tenant-owned by the one tenant context, or all are global under platform authority. The version-family row remains the single ownership authority, so concurrent first registration cannot make one family global and tenant-owned or assign it to two tenants.

### Startup is caller-side reconciliation, not a global barrier

On startup, a domain or platform gear:

1. batch-reads the current snapshots for its desired exact GTS Identifiers;
2. compares canonical authored content locally;
3. skips the POST when every desired definition is already current;
4. submits only missing or differing candidates with preconditions derived from the read;
5. polls the operation and gates its own readiness on the required per-candidate outcomes;
6. performs a fresh read and a new reconciliation cycle after a precondition failure or a dependency-not-yet-registered failure.

Types Registry becomes ready when its own storage and workers are ready. It does not know how many registrants exist, wait for a staged set, or coordinate readiness across gears.

The SDK provides this workflow as a convenience helper while retaining lower-level batch-get, submit, and get-operation methods. A helper invocation generates one key and reuses it across transport retries and polling. Crash-resumable callers may persist and supply the key.

### Control-plane records are registry entities with built-in validators

Registry Source Plugin configuration and, in P2, Validation Hook declarations are global Managed registered Instances of platform-defined GTS Types. They pass through the same revisions, concurrency, operation, and audit protocol.

Types Registry provides a closed in-process validator for these platform-defined control-plane types. It is not registered or extensible and is distinct from P2 semantic hooks. It enforces Source Claim non-overlap, retired-claim reservations, capability requirements, and the prohibition on tenant-scoped control-plane instances. Their base schemas are trusted platform seeds and do not depend on user definitions.

## Consequences

* Real registration work always costs at least acceptance plus operation polling; the SDK hides that loop for callers that want a terminal result.
* Every accepted POST costs an operation, its candidate rows, an outbox message, and a poll. The startup helper avoids the POST entirely, so no correct path pays this.
* Request identity needs no storage of its own: the operation is the receipt, so replay and conflict semantics cost one unique constraint rather than a second table and a second insert per write.
* The content-equality rule is implemented once, in the worker, instead of also as a whole-batch predicate under a row lock in the request handler.
* Clients distinguish request retry from a new reconciliation attempt. Reusing a completed key does not ask the server to compare against today's state.
* Every update candidate carries an explicit resource version, and every create candidate explicitly requires absence.
* Batch outcomes are deterministic with respect to dependency units, but a mixed success/failure result is valid.
* Types Registry leaves the platform startup critical path; each registrant owns its retry and readiness.
* The worker must be safe under outbox redelivery and lease expiry.

## Confirmation

This decision is confirmed when:

* every accepted request returns `202` with an operation UUID without waiting for admission, and no successful acceptance has any other shape;
* an all-equal request is accepted as an ordinary operation whose candidates all terminate `unchanged`, and no storage path produces a request record without an operation;
* a matching key replay returns the same operation even after another request changes current state;
* the same scoped key with another fingerprint is rejected;
* two callers on different planes may use the same key value without collision, and so may two principals in one tenant;
* an update whose target resource version changed fails with `precondition_failed` and creates no revision;
* a dependency-only race is revalidated without bypassing the target precondition;
* equal authored content creates no revision and does not advance resource version;
* independent valid candidates commit when another dependency branch fails;
* a failed in-batch dependency blocks its dependants;
* a cyclic dependency component commits atomically or not at all;
* concurrent first-family admission leaves exactly one owner;
* a gear with current definitions performs only the batch read and reports `UpToDate`;
* a gear with stale definitions submits a conditional batch, polls it, and gates only its own readiness;
* Types Registry reaches ready state before any domain gear registers definitions;
* tenant-scoped control-plane registration is rejected and Source Claim invariants are enforced without P2 hooks;
* a dry run of each mutation kind reports the same per-candidate outcomes and diagnostics as the real operation while leaving every entity, revision, current pointer, resource version, and lifecycle status untouched;
* a dry run and a real submission carrying the same scoped key are treated as different requests, so the real submission executes rather than replaying the dry run;
* a dry run against a frozen logical entity returns the compatibility verdict together with the unproven-chain state, in a case where no real submission could be accepted at all.

## Pros and Cons of the Options

### Always synchronous

* Good, because P1 callers receive a terminal result in one request.
* Bad, because P2 hooks and wide dependent revalidation may outlive request deadlines.
* Bad, because a later switch to operations would break every client.

### Deadline-based inline completion

* Good, because short mutations sometimes finish in one round trip.
* Bad, because response shape depends on load and timing rather than semantics.
* Bad, because it couples the HTTP request lifetime to worker execution and complicates duplicate execution.

### Synchronous no-op plus asynchronous mutation

* Good, because response shape depends on semantics rather than on load or timing.
* Good, because a redundant raw POST terminates in one round trip.
* Bad, because strict replay of the no-op requires a separate durable receipt, and therefore a second table holding the optional half of a one-to-one relation.
* Bad, because the whole-batch equality predicate duplicates, under a row lock in the request handler, a rule the worker already applies per candidate.
* Bad, because the state it optimizes is unreachable for a caller that honours its own preconditions.

### Always asynchronous with no synchronous no-op path

* Good, because no successful acceptance has a second shape, so the response depends on nothing at all.
* Good, because request identity and workflow state collapse into one record, sparing a table, a foreign key, an insert per write, and a `disposition` discriminator.
* Good, because the content-equality rule has exactly one implementation and no row-locked snapshot read remains on the acceptance path.
* Bad, because a redundant submission now costs an operation, its rows, an outbox message, and a poll.
* Bad, because `unchanged` appears in both vocabularies while being unreachable for a correct caller, which reads as dead vocabulary until explained.

### All-or-nothing batch

* Good, because it has a simple outcome.
* Bad, because an unrelated invalid candidate prevents valid independent registrations.
* Bad, because expensive validation encourages an excessively large transaction or repeated work.

### Unconstrained best effort

* Good, because independent candidates may succeed.
* Bad, because the term does not define dependency fallback, ordering, or cycle safety.

### Dependency-aware partial admission

* Good, because it preserves independent progress and explicitly protects dependency units.
* Bad, because clients must inspect per-GTS-ID results and handle partial success.

## More Information

* [Google AIP-151 Long-running operations](https://google.aip.dev/151) describes a durable operation resource for work that may outlive a request.
* [Stripe idempotent requests](https://docs.stripe.com/api/idempotent_requests) treats the key as request identity and replays the original result.
* [Kubernetes resource versions](https://kubernetes.io/docs/reference/using-api/api-concepts/#resource-versions) illustrate an opaque concurrency token distinct from resource content.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **Database reference schema**: [../database.sql](../database.sql)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0003**: [0003-cpt-cf-types-registry-adr-type-schema-evolution-compatibility.md](./0003-cpt-cf-types-registry-adr-type-schema-evolution-compatibility.md)
- **ADR-0005**: [0005-cpt-cf-types-registry-adr-type-schema-revisions.md](./0005-cpt-cf-types-registry-adr-type-schema-revisions.md)
- **ADR-0006**: [0006-cpt-cf-types-registry-adr-registered-instance-revisions.md](./0006-cpt-cf-types-registry-adr-registered-instance-revisions.md)
- **ADR-0009**: [0009-cpt-cf-types-registry-adr-tenant-ownership-visibility-authority.md](./0009-cpt-cf-types-registry-adr-tenant-ownership-visibility-authority.md)
- **ADR-0011**: [0011-cpt-cf-types-registry-adr-managed-external-boundary.md](./0011-cpt-cf-types-registry-adr-managed-external-boundary.md)

This decision directly addresses:

* `cpt-cf-types-registry-fr-register-schemas`, `cpt-cf-types-registry-fr-register-instances`;
* `cpt-cf-types-registry-fr-dry-run`;
* `cpt-cf-types-registry-fr-two-phase-init`;
* `cpt-cf-types-registry-fr-registry-source-routing`;
* `cpt-cf-types-registry-fr-validation-hooks`;
* `cpt-cf-types-registry-interface-sdk`, `cpt-cf-types-registry-interface-rest`.
