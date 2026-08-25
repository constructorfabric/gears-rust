---
status: accepted
date: 2026-07-23
decision-makers: Constructor Fabric Steering Committee
---

# Registered GTS Instance Mutation, Revision, and Retention Model

**ID**: `cpt-cf-types-registry-adr-registered-instance-revisions`

## Table of Contents

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Terminology](#terminology)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Revision identity and creation](#revision-identity-and-creation)
  - [Instance update validation](#instance-update-validation)
  - [Optimistic concurrency](#optimistic-concurrency)
  - [Retention](#retention)
  - [Content revisions versus registry state](#content-revisions-versus-registry-state)
  - [Resolution and historical access](#resolution-and-historical-access)
  - [Rollback direction](#rollback-direction)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Immutable Instance value; every change requires a new GTS ID](#immutable-instance-value-every-change-requires-a-new-gts-id)
  - [Mutable current value with no retained history](#mutable-current-value-with-no-retained-history)
  - [Mutable logical Instance with retained immutable revisions](#mutable-logical-instance-with-retained-immutable-revisions)
- [More Information](#more-information)
  - [Industry Practice](#industry-practice)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Types Registry stores named, well-known GTS Instances used for platform configuration, discovery, and governed declarations. These are registry entities, not arbitrary runtime domain objects.

ADR-0004 makes a managed registered Instance ID a stable major-only logical identity. Types Registry must therefore decide whether the Instance value is immutable, whether every accepted update creates a retained revision, how concurrent updates are prevented from overwriting each other, and how an Instance revision records the Type Schema definition against which it was validated.

Type Schema compatibility terminology does not directly apply to two Instance values. A replacement value is safe when it remains structurally valid under the applicable Type Schema revision, satisfies ownership and lifecycle policy, and, when P2 Validation Hooks are enabled and a required binding matches, passes owning-gear semantic validation.

## Scope

This ADR decides:

* whether a managed registered GTS Instance is mutable under its stable GTS ID;
* Instance revision identity, immutability, and current-value selection;
* retention of admitted Instance revisions;
* validation against mutable Type Schemas;
* optimistic concurrency for Instance updates;
* the boundary between content revisions and lifecycle or tenant state;
* ordinary and historical resolution behavior.

This ADR does not apply to runtime domain objects stored by owning gears or to Externally Managed registered Instances. It does not define a general-purpose object-history service. External Instance revision, retention, and historical-access semantics belong to the owning Registry Source Plugin under ADR-0002.

## Terminology

| Term | Meaning |
|---|---|
| Logical registered Instance | One well-known managed GTS Instance identified by a stable GTS ID and Registry Reference. |
| Instance revision | One immutable admitted canonical value of the logical registered Instance. |
| Current revision | The Instance revision returned by ordinary resolution. |
| Admission candidate | Proposed canonical Instance content undergoing validation before initial admission or before it can replace the current revision. It is not yet an Instance revision. |
| Candidate status | Per-candidate workflow and outcome state — `pending`, `running`, `succeeded`, `unchanged`, or `failed` under ADR-0012 — distinct from operation progress and logical-entity Lifecycle Status. |
| Conforming Type Schema revision | The exact Type Schema revision used to validate an Instance revision at admission time. |
| Content hash | A digest of canonical Instance content used for idempotency, validation binding, and diagnostics. |

## Decision Drivers

* Well-known configuration and discovery values need stable identities while their values may legitimately change.
* Successive Instance values do not have a general structural backward/forward compatibility relation.
* A mutable conforming Type Schema means historical validation must identify the exact schema revision used.
* Concurrent tenant administrators or gears must not overwrite each other's Instance changes.
* Retained revisions support diagnostics and future rollback but must not turn Types Registry into an arbitrary runtime-object store.
* Lifecycle, admission status, ownership, tenant enablement, and external-source freshness are separate state dimensions from Instance content.

## Considered Options

* Immutable Instance value; every change requires a new GTS ID.
* Mutable current value with no retained history.
* Mutable logical Instance with retained immutable revisions.

## Decision Outcome

Chosen option: a managed registered GTS Instance is a mutable logical entity whose every admitted content value is an immutable retained revision.

### Revision identity and creation

* Initial successful admission atomically creates the logical registered Instance in lifecycle `ACTIVE`, creates Instance revision `1`, and makes it current.
* Each successful content update allocates the next monotonically increasing revision number scoped to the logical Instance.
* Each revision contains at least the logical Instance reference, revision number, canonical content, content hash, creation and admission metadata, conforming Type Schema GTS ID, and exact conforming Type Schema revision.
* Revision numbers are allocated only when admission succeeds. Only a `succeeded` candidate allocates one; a `pending`, `running`, `failed`, or `unchanged` candidate is not an admitted revision and consumes no revision number — `unchanged` because its content already equals the current revision (ADR-0012).
* Revision content, number, hash, and schema-validation provenance are immutable after creation.
* The logical Instance owns a current-revision pointer. Ordinary resolution returns the current revision.
* Re-submitting content equal to the current canonical content is an idempotent no-op and does not allocate a revision.
* Re-admitting content equal to an older non-current revision allocates a new monotonically increasing revision after normal validation.

### Instance update validation

An admission candidate becomes an admitted Instance revision and current only when:

1. its GTS ID and instance envelope are valid, which for the managed profile means the last segment carries no explicit UUID tail (ADR-0001), no major 0, and no minor version. The last two are refused unconditionally and for one argument, which ADR-0015 makes: a marker whose meaning is vacuous for an Instance would mean one thing on a Type Schema and nothing here. An Instance of a minor-versioned Type Schema carries that minor in a *preceding* segment — `gts.A~acme.crm.order.type.v1.2~acme.thing.v1` — so nothing is lost;
2. its referenced logical Type Schema is visible, lifecycle-usable, tenant-available under the applicable registration policy, and not in the unstable profile of ADR-0015 — a schema exempt from evolution checks cannot carry an Instance, because the rule below that forbids a schema revision from becoming current while an affected Instance would break is exactly what that profile exists to remove;
3. the candidate content validates against the current admitted Type Schema revision selected at validation time;
4. every GTS and JSON Schema reference in the candidate is resolvable and valid;
5. ownership, lifecycle, dependency, and alias rules pass;
6. when P2 Validation Hooks are enabled and a required binding matches, every selected owning-gear semantic validator accepts the same candidate content hash and schema revision;
7. optimistic concurrency and schema freshness preconditions still hold at commit time.

Types Registry records the exact conforming Type Schema revision on the admitted Instance revision. If that Type Schema revision changes while Instance validation is in progress, the candidate must be revalidated or fail with a structured stale-baseline conflict. The race does not arise where the conforming Type Schema is minor-bearing, since ADR-0004 makes such an entity immutable and it therefore has no later revision to move to.

When a new Type Schema revision is proposed, ADR-0005 requires all affected registered Instances to remain valid before the schema revision becomes current. Existing admitted Instance revisions remain historical facts validated against their recorded schema revisions; the current Instance revision must also satisfy the new current Type Schema before activation.

There is no general backward, forward, or full compatibility check between successive Instance values. In P2, domain-specific continuity requirements selected by a matching hook binding belong in owning-gear semantic validation.

### Optimistic concurrency

Every managed registered Instance content update must carry the caller-observed `resource_version` of the logical Instance as an explicit precondition, in the form ADR-0012 defines for every candidate: `must_not_exist` for an identifier that was absent at read time, `match_resource_version(v)` for one that was present. The token is the entity's monotonic `resource_version` rather than its Instance revision number, for the reason ADR-0005 records — a lifecycle transition advances the entity without creating a content revision, so a revision-based token cannot detect one.

Types Registry validates the candidate without a long-lived database lock, then atomically inserts the revision and advances the current pointer only if:

* `entity.resource_version` still equals the caller's precondition;
* the conforming Type Schema current revision still equals the revision used for validation;
* other correctness-relevant dependency freshness tokens still match.

The first is the caller's baseline and the other two are internal, and they fail differently. A caller-precondition mismatch is a terminal per-candidate `precondition_failed`: Types Registry neither overwrites a concurrent change nor rebases the update onto it. A conforming schema or dependency that moved during validation causes the worker to revalidate within a bounded retry policy, without weakening the caller's precondition.

Before initial admission there is no public logical registered Instance and no entity Lifecycle Status. A failed initial candidate may remain as an operation or audit artifact, but it does not create a logical entity or tombstone, issue a Registry Reference for domain persistence, or establish a permanent GTS ID reservation. While an update candidate is `pending`, an existing logical Instance retains its current revision and its Lifecycle Status.

### Retention

* Every admitted registered Instance revision is retained for the lifetime of the registry identity, including after logical deletion while Registry References or registered dependents may still exist. As with a Type Schema, resolving the reference is not the whole reason: a gear holding a reference to a deleted well-known Instance may still need its value to retire what depends on it, so the current value survives deletion rather than only its hash (ADR-0013).
* Admitted revisions are never physically removed by a retention period, time-to-live, or background policy. Physical removal happens only through the explicit platform-level purge operation decided by ADR-0013, which is operator-invoked and never automatic.
* Authorization and tenant visibility apply to historical revisions at least as strictly as to the current Instance; historical access must not expose content to a caller who could not access the logical entity.
* Failed candidates may be retained under an operation-artifact policy but are not admitted revisions.

Those terms are unconditional, so whether a given class of content may be held under them is decided before registration and elsewhere: data classification is a cross-cutting platform decision, not one Types Registry owns, and the registry stores what it admits without applying a content policy of its own. A use case whose content cannot be retained on these terms belongs to a different storage owner.

### Content revisions versus registry state

The following change Instance content and therefore create a revision:

* replacing or patching the canonical registered value;
* a future rollback that restores historical content.

The following do not create an Instance content revision by themselves:

* admission of a higher-major Version Successor, which under ADR-0008 changes no other member of the version family;
* tenant enablement or computed availability changes;
* cache or freshness metadata changes;
* external source availability changes.

Those mutations advance the relevant registry state/cache token and create the required operation or audit record.

A `pending` candidate is not a logical Instance Lifecycle Status. The managed logical Instance lifecycle contains `ACTIVE` and `DELETED` in P1 under ADR-0008.

Admitting an internal Instance revision does not change Lifecycle Status, and neither does admitting a higher-major Version Successor (ADR-0008).

### Resolution and historical access

Ordinary registered Instance resolution returns:

* stable GTS ID and Registry Reference;
* current content hash and the freshness validator;
* lifecycle and tenant availability metadata.

As with a Type Schema, this list is the **default field projection** rather than a minimum, and the caller selects content explicitly. The canonical value is therefore not returned unless asked for — which reads oddly for an Instance, whose value is most of what it is, and is nonetheless right: reverse-resolving a batch of stored references to display their identifiers is a common operation that has no use for the values, and paying for them by default would make the cheap case expensive.

Neither the Instance revision number nor its conforming Type Schema revision appears in the contract. As ADR-0005 explains, no P1 operation accepts a revision number, so exposing one creates an unusable handle.

The conforming revision is actively misleading. A caller seeing validation against schema revision 4 beside a later current revision could conclude that the value is outdated. That is false: this ADR and ADR-0005 prevent a schema revision from becoming current if an affected registered Instance would become invalid. The current Instance value is therefore valid against the current schema.

The stored schema revision is the one that admitted the value, and it lives on the immutable Instance revision. It is internal admission provenance, not the value's current standing.

That same argument is why no revision records the revalidation itself. The current standing is the invariant above, so a "last revalidated by" column on current state would restate what activation already guarantees, and it could not serve as retry bookkeeping either: a rolled-back attempt commits nothing, so nothing survives to shorten the retry. A conforming Instance is reached during revalidation the way every other dependent is, through the reverse dependency traversal. Indexing the conforming schema on current state would not merely duplicate that reach, it would compete with it: the commit rechecks the membership of the reverse-impact set under the target's lock, and two mechanisms producing one set must then be reconciled there, on the path whose duration is already the hazard. Should the activation protocol commit progress in stages, that progress belongs to the operation rather than to entity state.

The conforming Type Schema identifier is also absent, for a plainer reason: GTS §11.1 makes it the Instance identifier's chain up to and including the last `~`, so the caller already holds it and the SDK derives it with a method call rather than string parsing.

Domain gears that reference the well-known Instance store its stable Registry Reference under ADR-0001, not the current Instance revision. A domain contract that must reproduce the exact historical value must explicitly store revision provenance under a separate owning-gear requirement.

P1 retains all revisions for internal validation and diagnostics. A public management revision-history API may be phased separately; ordinary resolution never returns an arbitrary historical revision.

### Rollback direction

Instance rollback is not selected for P1 by this ADR. If introduced later, it must run normal validation and optimistic concurrency and create revision `N+1` with content copied from historical revision `K`; it must not move the current pointer backward.

### Consequences

* Registered well-known Instances can evolve without changing their GTS IDs or Registry References.
* Every admitted current value has reproducible schema-validation provenance.
* Instance history retention is unbounded in P1, which is acceptable because runtime domain objects remain out of scope.
* In P2, matching owning-gear semantic validators, not a generic schema-diff algorithm, decide whether a domain-specific Instance value transition is acceptable.
* Caches must validate Instance revision or ETag freshness just as they do for mutable Type Schemas.
* Type Schema updates and Instance updates are coupled through exact schema revision checks but retain separate revision histories.

### Confirmation

This decision is confirmed when:

* successful initial admission atomically creates an `ACTIVE` logical registered Instance and revision `1`, while a content update creates the next immutable revision without changing GTS ID or Registry Reference;
* a pending or rejected initial candidate is never returned as a logical entity, consumes no revision number, and does not permanently reserve the GTS ID;
* a pending or rejected update leaves the existing logical entity lifecycle and current revision unchanged;
* every admitted Instance revision records the exact conforming Type Schema revision;
* a managed registered Instance identifier carrying a minor version in its last segment is rejected even where its conforming Type Schema carries a minor, while an Instance whose conforming Type Schema carries a minor in a preceding segment is admitted normally;
* changed Instance or Type Schema freshness tokens produce a structured conflict before activation;
* a Type Schema update cannot activate while an affected current registered Instance would become invalid;
* P2 hook tests cover initial admission and content revisions of managed registered Instances;
* all admitted Instance revisions remain internally retrievable after later updates and logical deletion;
* ordinary resolution returns the current value's content hash, the freshness validator, and lifecycle and availability metadata, while stable references remain logical — and returns neither the Instance revision number nor the conforming Type Schema revision, since §*Resolution and historical access* keeps both out of the contract.

## Pros and Cons of the Options

### Immutable Instance value; every change requires a new GTS ID

* Good, because exact identity always determines exact content.
* Good, because caches and historical explanation are simple.
* Bad, because configuration and discovery identities churn for ordinary value changes.
* Bad, because aliases or dependents must migrate even when the logical well-known Instance remains the same.

### Mutable current value with no retained history

* Good, because writes and storage are simple.
* Bad, because concurrent updates, diagnostics, and rollback need an external mechanism.
* Bad, because the Type Schema revision used for an older admitted value is lost.
* Bad, because a stable Registry Reference can no longer explain historical registry behavior.

### Mutable logical Instance with retained immutable revisions

* Good, because the public identity remains stable while every admitted value is distinguishable.
* Good, because validation provenance, concurrency, diagnostics, and future rollback are supported.
* Good, because the model aligns with Type Schema revisions without pretending Instance values have schema compatibility semantics.
* Bad, because revision storage is unbounded and nothing after admission can shorten it.
* Bad, because management authorization and historical visibility require explicit policy.

## More Information

### Industry Practice

* [Google AIP-162 Resource Revisions](https://google.aip.dev/162) describes immutable historical snapshots, diff and rollback use cases, and the distinction between a logical resource and its revisions. AIP-162 is a draft and is used as design precedent.
* [Google AIP-154 Resource Freshness Validation](https://google.aip.dev/154) recommends ETags to prevent one concurrent update from overwriting another resource state.
* [Kubernetes API concepts](https://kubernetes.io/docs/reference/using-api/api-concepts/) use `resourceVersion` and conditional update behavior to detect stale resource modifications in a distributed control plane.
* [Google Cloud Pub/Sub schemas](https://docs.cloud.google.com/pubsub/docs/schemas) distinguish a stable schema resource from committed revisions, showing the broader control-plane pattern of stable identity plus revisioned state.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0004**: [0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md](./0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md)
- **ADR-0005**: [0005-cpt-cf-types-registry-adr-type-schema-revisions.md](./0005-cpt-cf-types-registry-adr-type-schema-revisions.md)
- **ADR-0008**: [0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md) — decides that admitting a higher-major Version Successor changes no other member of the family, so it creates no Instance revision anywhere else.
- **ADR-0012**: [0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md](./0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md) — decides the write-path contract the optimistic-concurrency section rests on: `resource_version` preconditions rather than a revision token, per-candidate outcomes, and request-key replay.
- **ADR-0013**: [0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md](./0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md) — decides the only mechanism that physically removes a retained revision, and keeps it out of every retention policy.
- **ADR-0015**: [0015-cpt-cf-types-registry-adr-major-zero-unstable-profile.md](./0015-cpt-cf-types-registry-adr-major-zero-unstable-profile.md) — refuses an Instance of an unstable Type Schema, and refuses major 0 on an Instance identifier, because successive Instance values have no compatibility relation for the profile to exempt.

This decision directly addresses:

* `cpt-cf-types-registry-fr-register-instances` - defines managed registered Instance content-update semantics.
* `cpt-cf-types-registry-fr-gts-validation` - binds each Instance revision to the exact Type Schema revision used for validation.
* `cpt-cf-types-registry-fr-ref-tracking` - protects registered Instance dependencies during schema and Instance updates.
* `cpt-cf-types-registry-fr-cache-freshness-metadata` - provides Instance revision and content hash as result validators.
* `cpt-cf-types-registry-fr-validation-hooks` - delegates domain-specific transition safety to the owning gear.
* `cpt-cf-types-registry-fr-two-phase-init` - defines the per-Instance candidate, initial revision, and publication semantics used by the dependency-aware partial batch admission of ADR-0012.
