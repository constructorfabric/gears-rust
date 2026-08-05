---
status: accepted
date: 2026-07-27
decision-makers: Constructor Fabric Steering Committee
---

# Platform-Level Purge of Deleted Registry State

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [One operation](#one-operation)
  - [Everything that records the identity](#everything-that-records-the-identity)
  - [What guards purge](#what-guards-purge)
  - [Preconditions and shape](#preconditions-and-shape)
  - [Non-goals: personal-data erasure](#non-goals-personal-data-erasure)
  - [The exception to identifier non-rebinding](#the-exception-to-identifier-non-rebinding)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A deployment mode in which ordinary deletion is physical and identifiers are reusable](#a-deployment-mode-in-which-ordinary-deletion-is-physical-and-identifiers-are-reusable)
  - [A retention period after which deleted entities are physically removed](#a-retention-period-after-which-deleted-entities-are-physically-removed)
  - [An explicit platform-level purge, split into content removal and identity removal](#an-explicit-platform-level-purge-split-into-content-removal-and-identity-removal)
  - [One explicit platform-level purge](#one-explicit-platform-level-purge)
- [More Information](#more-information)
  - [Industry Practice](#industry-practice)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-types-registry-adr-platform-purge-of-deleted-entities`

## Context and Problem Statement

Deletion in Types Registry is logical and terminal. `DELETED` is final in P1, admitted content revisions are retained, no retention policy ever removes them, and ADR-0001 reserves the GTS Identifier permanently so it can never be rebound to a new logical entity.

Those properties are correct for production. They also make an ordinary development mistake unrecoverable. A developer who registers a schema under a mistyped identifier, or registers one shape and then needs an incompatible one, has burned that identifier for the lifetime of the deployment. Iterating on a schema before it has any consumers is exactly the case the production guarantees were not written for.

The obvious fix — a deployment mode in which deletion is physical and identifiers are reusable — was considered and rejected during design discussion, for a reason worth recording: it makes the development environment stop being a rehearsal of production. Reverse resolution of a deleted entity's Registry Reference, rejection of re-registration under a deleted identifier, and tombstone retention are precisely the behaviours most likely to harbour bugs, and a mode that changes ordinary deletion hides all three.

Recovering a burned identifier is the whole of the problem. A second driver suggests itself — retained revisions might hold personal data in descriptions, examples, or enum values, which would argue for splitting the mechanism in two to serve it. §Non-goals records why that driver is not taken.

## Scope

This ADR decides:

* whether physical removal of registry state exists, and through what mechanism;
* what it removes, and what has to be removed with it for an identifier to be genuinely free;
* what guards it, given that no check can establish its safety;
* the delivery shape, authority, and audit of the operation;
* the single exception this creates to the identifier non-rebinding guarantee.

This ADR does not decide deletion preconditions or lifecycle transitions (ADR-0008, PRD `cpt-cf-types-registry-fr-lifecycle`), the write-path contract the operation uses (ADR-0012), the retired Source Claim reservations of ADR-0011, or the platform-wide data classification policy that determines what counts as personal data.

## Decision Drivers

* A development environment must exercise the same deletion semantics as production, or it stops being evidence about production.
* Releasing an identifier for reuse is a data-corruption primitive, not a storage optimization: deterministic derivation gives the reused identifier the same Registry Reference, so any domain row still holding it silently rebinds to an unrelated entity.
* No check available to Types Registry can establish that no domain row holds a given Registry Reference. Safety cannot be proven, only bounded.
* Physical removal must never be automatic. A background policy that quietly discards registry state is the failure mode retention rules exist to prevent.
* A mechanism that only partly achieves what its name promises is worse than none, because it invites reliance it cannot support.

## Considered Options

* A deployment mode in which ordinary deletion is physical and identifiers are reusable.
* A retention period after which deleted entities are physically removed.
* An explicit platform-level purge operation, separated into content removal and identity removal.
* One explicit platform-level purge operation that removes both.

## Decision Outcome

Chosen option: **one explicit, operator-invoked platform-level purge that physically removes an entity's records and releases its GTS Identifier.**

Ordinary deletion is unchanged everywhere. Every deployment, including development, exercises the same logical deletion, the same tombstone retention, and the same identifier reservation. What differs between deployments is whether one privileged operation is available at all, not how deletion behaves.

### One operation

Purge removes the records of a `DELETED` entity and releases its identifier for registration as a new logical entity. It is the operation development needs, and the one that can corrupt data.

The alternative is to split it in two, adding a content purge that removes retained revisions while keeping the identity tombstone, on the ground that it is production-safe and could serve a personal-data erasure obligation. §Non-goals rejects that half, and with it the argument for a split: what remains is one act with one risk profile and one guard.

The tombstone, the mapping, and any other durable record of the identity are removed in **one transaction**. A partial removal that leaves a mapping behind would let the identifier be re-registered while a stale record still points at the old entity.

Before purge, a deleted entity is still readable by an exact key and reports itself deleted and unavailable, which is how a gear holding a stored reference distinguishes a retired contract from an unknown one. Purge removes that distinction: between purge and re-registration the old Registry Reference resolves to nothing, indistinguishable from a reference that was never issued. After re-registration of the same identifier it resolves again, to the new logical entity, because deterministic derivation makes identifier and reference the same fact — the reference cannot be retired while the identifier is reusable. That is the rebinding hazard this operation carries, and it is the reason it is disabled by default rather than something a stronger removal rule could eliminate.

### Everything that records the identity

"Any other durable record of the identity" is not rhetorical, and the enumeration matters because a survivor does not merely leave litter — it silently changes what re-registration means. Beyond the entity row and the Registry Reference mapping, two records name the identity and are easy to overlook.

The **version-family record** binds the family key to an ownership scope. If it outlives its last member, the released name stays bound to the previous owner and, under ADR-0004's kind-exclusive family key, to the previous entity kind. A new registrant would be refused by a family that has nobody left to belong to. Purge therefore removes a family record once it is empty.

The **operation history** records the identifier on every candidate item that named it. Leaving those behind does not block re-registration, but after one the history holds items for two unrelated logical entities under a single identifier string. Purge removes the items naming the purged identifiers; the operation rows themselves survive, and one whose subject is partly gone is an honest record rather than a corrupted one.

### What guards purge

Nothing can prove purge is safe. Types Registry cannot see domain rows, so it cannot establish that no gear still holds the Registry Reference. A grace period proves nothing either — a domain row can be older than any interval.

The guard is therefore deployment policy: whether the operation is available at all. It is disabled by default and enabled deliberately, which in practice means enabled in development and scratch environments and left off in production except for a specific, planned migration.

This is a narrower divergence than the rejected deployment mode, and the difference is the point. Only the availability of one maintenance operation varies. Every ordinary code path — admission, resolution, deletion, reverse resolution of a deleted reference — is identical in every environment, so production scenarios remain testable on a development stand.

Where purge is enabled, it is documented as unsafe whenever domain data may hold the reference. In a development environment nothing holds it, which is what makes the operation reasonable there and not elsewhere.

### Preconditions and shape

Purge requires the entity to be `DELETED`, and re-evaluates the deletion preconditions at execution time: no registered dependent may exist. Under ADR-0011 every dependent is a Managed Entity, so that re-evaluation reads managed storage alone and reaches no plugin.

**Purge is synchronous and creates no operation.** It runs to completion inside the request and returns its report in the response. This is the one mutation that does not use the asynchronous write path of ADR-0012, and the reasons that made that path mandatory for registration and deletion are all absent here. P2 Validation Hooks do not apply to purge, so its duration is bounded by local database work rather than by a counterparty — which is what made an unbounded operation necessary elsewhere. Its work is a scan and a delete over managed storage, with no GTS resolution, no compatibility checking, and no plugin call, since ADR-0011 leaves every dependent local. And it has no caller to keep a stable contract for: it is an operator-invoked platform-plane job, disabled by default, not a gear-facing API whose response shape has to survive P2.

Three things follow, and each is a subtraction rather than a special case. There is **no `Idempotency-Key` and no replay record**: re-running a purge of the same pattern finds the already-released identifiers absent and reports them as not matched, so the operation is naturally repeatable and needs no stored request identity to make it so. There is **no per-candidate row**, because the caller names no candidates — the pattern is expanded by a scan and the outcome per identifier is in the response, not in storage. And there is **nothing for a later purge to erase**: purge still removes the operation items naming the identifiers it releases, so a re-registration cannot leave a history in which one identifier string spans two logical entities, and the question of whether it might delete its own record does not arise, because it has none.

The audit trail is therefore the job's own record rather than a registry row. What such a record must contain, who may read it, and how long it is kept is PRD open question 2, which covers registry mutations generally and is not settled by this decision.

It is delivered as a platform maintenance job rather than as tenant-facing API surface. That follows from what it is: a non-tenant-scoped platform operation authenticated on the platform plane with `PlatformSecurityContext` rather than a propagated tenant context, batch-shaped, and potentially wide in scope. A job also gives the operation a natural place for the property that matters most in practice — a **dry run** that reports exactly which identifiers would be released before anything is removed, broken down by owner, since one pattern can cross tenant boundaries.

The job takes a **GTS pattern**, which is what makes it usable and also what keeps referential integrity intact. A registered Instance's identifier begins with the identifier of the Type Schema it conforms to, so any prefix pattern that selects a schema necessarily selects every Instance that could pin one of its revisions — a structural property of the chained identifier, not a coincidence. The job removes matched Instances before matched Type Schemas, and the pins never obstruct it. An exact identifier carrying no wildcard selects only itself; there the job **MUST** verify that no Instance still pins the target and refuse with those Instances listed rather than failing on a constraint.

A pattern selects candidates; it does not waive preconditions. The job reports how many entities matched, how many were eligible, why each of the rest was skipped, and — for the dry run — the owner of every identifier it would release. All of that is in the response, computed while the entities are still in hand, which is what a synchronous shape buys: a dry run removes nothing, so the owner of a matched entity is a read away rather than something that has to be recorded before the entity disappears.

That dry run is a facility of this job rather than an application of the general dry-run mode of ADR-0012, since purge does not travel that path. What it keeps from that mode is the property that made it worth having: it is a **mode of the same code**, running the identical check sequence and stopping before the removal, so the report cannot drift from what the real purge would do. It needs no request-identity rule to stay distinct from a real purge, because there is no stored request to be replayed. And it proves nothing about a purge invoked later, since an entity's eligibility can change in between.

Purging a Registry Source Plugin removes its Source Claims with it. No extra ordering is needed: deleting the plugin Instance — a precondition of purging it — already retired those claims and released the foreign key by which they pinned its revisions. The job deletes the claim rows and bumps the routing generation in the same transaction, so cached routing and live federated cursors observe the change.

Purge never runs on a schedule, on a timer, or as a consequence of any retention rule. Every execution is an explicit act with an operator behind it.

### Non-goals: personal-data erasure

Types Registry is a registry of type contracts. What it stores is schema documents, well-known configuration values, and platform control-plane declarations, authored by developers about contracts rather than collected about people. Personal data in that content is already prohibited by ADR-0006 and by the DESIGN constraint on sensitive content. This decision declines to build a mechanism for the prohibited case, and the reasoning is worth recording because the opposite choice is the tempting one.

A content-only purge would not discharge an erasure obligation. It would require the entity to be `DELETED`, so it could not reach the likeliest occurrence — content in a live, in-use type, where removal would first mean deleting the type and could only succeed if nothing depended on it. It would not touch identifiers, which can themselves carry a name and which live in the entity record, the family key, and the operation history. And removing a row does not remove it from write-ahead logs, from pages awaiting vacuum, or from backups, so the live dataset is the only thing any such operation reaches.

A mechanism that satisfies none of those on its own, while being named for the obligation, invites reliance it cannot support. The honest position is the one taken here: the registry holds contracts, personal data in them is prohibited rather than managed, and P1 offers no erasure path. That statement is auditable and puts the obligation where it can actually be met — in the prohibition and in review — instead of in a job that would appear to discharge it.

The consequence is stated plainly rather than hidden. Retention is unbounded by policy, and in a production deployment, where purge is disabled, admitted content cannot be removed at all. The prohibition is therefore not a guideline but the only control, and it is load-bearing in proportion.

### The exception to identifier non-rebinding

ADR-0001 guarantees that a logically deleted GTS Identifier cannot be rebound to a new logical entity. Purge is the single, named exception to that guarantee, and it is the reason the guarantee is stated as a property of ordinary operation rather than of the storage layer.

Retired Source Claim reservations from ADR-0011 are released by the same exception. A Registry Source Plugin is a registered Instance, so purging it removes its reservations along with its identity, and the claimed identifier space becomes registrable again. Placing those reservations out of scope would carve out an identical hazard for different treatment, since a released claim space rebinds a persisted reference exactly as a released identifier does. ADR-0011 offers no runtime takeover operation, so purge is the only in-product way to reuse a reserved space, and it is the wider of the two available: it releases the space to whoever asks next, including a managed registration. The narrower one is outside the product — a migration that retargets the claim rows to a named successor, leaving the space reserved throughout.

### Consequences

* Development iteration is delete, purge, re-register, with production-identical semantics at every step. That is the trade this decision makes deliberately. The job may batch the first two over a pattern — deleting in dependency order and then purging — which is sequencing rather than new semantics: each step keeps its own preconditions and produces its own operation record, so the development stand still rehearses production.
* Retained content is unremovable in a production deployment, because the one operation that removes it also releases the identifier and is therefore disabled there. This is a deliberate outcome, and §Non-goals states what it rests on.
* Retention of the admitted revisions of ADR-0005 and ADR-0006 is unbounded by policy and bounded only by explicit purge.
* The identifier non-rebinding guarantee of ADR-0001 has exactly one exception, and this operation is it.
* A deployment must expose whether purge is enabled, so that an operator can tell before invoking it.
* Enabling purge in production is a decision an operator can make. The documentation must state plainly that it can silently rebind persisted domain references, because no runtime check will say so.
* Referential integrity between a registered Instance revision and the Type Schema revision that validated it survives purge without weakening, because the pattern that selects a schema also selects its Instances. Had the job taken a list of exact identifiers instead, those foreign keys would have had to be dropped.

### Confirmation

This decision is confirmed when:

* ordinary deletion behaves identically with purge enabled and disabled, and a development deployment reproduces production reverse resolution, re-registration rejection, and tombstone behaviour;
* a deleted entity is absent from discovery, search, and query assistance, and is returned by an exact read — by GTS Identifier or by Registry Reference alike — marked deleted and unavailable, so that a gear holding a stored reference can tell *deleted* from *never existed*;
* purge removes the entity record, its revisions, and the forward and reverse mapping in one transaction, after which the old Registry Reference resolves to nothing and is indistinguishable from an unissued reference;
* purge removes the version-family record once its last member is gone, and a subsequent registration of the released identifier under a different owner, or of the other entity kind, succeeds;
* purging a Registry Source Plugin removes its retired Source Claims, after which a Managed Entity can be registered in the space they reserved, while deleting that plugin without purging it leaves the reservations in force;
* purge removes the operation items naming the purged identifiers, so a later re-registration cannot produce a history in which one identifier string spans two logical entities;
* a purge returns its report in the response and creates no operation, no operation item, and no outbox message; re-running it over the same pattern reports the already-released identifiers as unmatched rather than failing, so repeatability needs no stored request identity;
* registration of a new logical entity under a purged identifier succeeds and resolves under the same Registry Reference as the purged one;
* purge rejects an entity that is not `DELETED`, and one that still has a registered dependent, with the precondition re-evaluated from managed storage while every plugin is unreachable;
* a pattern that selects a Type Schema also selects every Instance conforming to it, Instances are removed before schemas, and no foreign key obstructs the job; an exact identifier still pinned by an Instance is refused with those Instances listed;
* the job reports matched, eligible, and skipped counts with a reason per skipped entity, and a dry run reports the identifiers that would be released, broken down by owner, and removes nothing;
* purge is unavailable, and reported as unavailable, in a deployment where it is not enabled;
* no scheduled task, retention sweep, or background process removes **admitted content or identity** — a revision, an entity, a tombstone, a version family, or a Source Claim reservation — in any deployment. The one scheduled removal the platform does operate, the operation-retention sweep of DESIGN §3.2, is bounded to operations that no revision points at, so it releases no identifier and can rebind nothing.

## Pros and Cons of the Options

### A deployment mode in which ordinary deletion is physical and identifiers are reusable

* Good, because development iteration is a single operation with no extra step.
* Good, because it needs no new API surface at all.
* Bad, because the development environment stops rehearsing production. Reverse resolution of a deleted reference, rejection of re-registration, and tombstone retention are never exercised — and those are the behaviours most likely to be wrong.
* Bad, because the divergence is broad and implicit: every deletion behaves differently, rather than one operation being additionally available.

### A retention period after which deleted entities are physically removed

* Good, because it requires no operator action and bounds storage growth automatically.
* Bad, because a background process that releases identifiers would rebind persisted domain references with no human in the loop and no event anyone observes.
* Bad, because elapsed time is unrelated to whether a reference is still held; the interval would be a guess presented as a guarantee.
* Bad, because it makes registry state disappear silently, which is the outcome retention rules exist to prevent.

### An explicit platform-level purge, split into content removal and identity removal

* Good, because the safe half would be usable in production while only the dangerous half is gated.
* Bad, because the safe half serves only an erasure obligation this registry does not have (§Non-goals), so it is machinery for a case that should not arise.
* Bad, because as specified it would not even serve that case: requiring `DELETED` puts it out of reach of the likeliest occurrence, content in a live, in-use type, whose removal would first require deleting the type.
* Bad, because it doubles the operation surface, the guards, and the vocabulary for one act.

### One explicit platform-level purge

* Good, because ordinary deletion is identical in every deployment, so a development stand remains evidence about production.
* Good, because every removal has an operator, an audit record, and an available dry run.
* Good, because one act has one risk profile and one guard, and nothing has to explain which half an operator wants.
* Bad, because development iteration costs an extra step.
* Bad, because its safety rests on deployment policy and operator judgement rather than on anything the registry can verify.
* Bad, because a production deployment has no way to remove retained content at all, which is acceptable only while the content is contracts.

## More Information

### Industry Practice

* [Confluent Schema Registry](https://docs.confluent.io/platform/current/schema-registry/schema-deletion-guidelines.html) separates a soft delete, which keeps the version recoverable and its identifier reserved, from a hard delete, which is explicit, permanent, and documented as usable only when no consumer depends on the schema. The same two-stage shape, with the second stage guarded by judgement rather than by a check.
* [crates.io](https://doc.rust-lang.org/cargo/commands/cargo-yank.html) never releases a published name for reuse, accepting permanent reservation as the price of reference integrity — the position Types Registry holds by default and departs from only under an explicitly enabled operation.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0005**: [0005-cpt-cf-types-registry-adr-type-schema-revisions.md](./0005-cpt-cf-types-registry-adr-type-schema-revisions.md)
- **ADR-0006**: [0006-cpt-cf-types-registry-adr-registered-instance-revisions.md](./0006-cpt-cf-types-registry-adr-registered-instance-revisions.md)
- **ADR-0011**: [0011-cpt-cf-types-registry-adr-managed-external-boundary.md](./0011-cpt-cf-types-registry-adr-managed-external-boundary.md)
- **ADR-0012**: [0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md](./0012-cpt-cf-types-registry-adr-write-path-admission-protocol.md) — decides the write path this job uses, and the general dry-run mode of which the dry run described here is one application.

This decision directly addresses:

* `cpt-cf-types-registry-fr-lifecycle` - supplies the only mechanism that physically removes registry state, and keeps it out of any retention policy.
* `cpt-cf-types-registry-fr-id-resolution` - names the single exception to the identifier non-rebinding guarantee, and bounds it to one local transaction.
* `cpt-cf-types-registry-fr-register-schemas`, `cpt-cf-types-registry-fr-register-instances` - bound the retention of admitted content: unbounded by policy, removable only by this operation, and therefore unremovable wherever it is disabled. §Non-goals records why no erasure path is offered instead, and why naming one here would invite reliance it could not support.
