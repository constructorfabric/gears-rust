---
status: accepted
date: 2026-07-26
decision-makers: Constructor Fabric Steering Committee
---

# Managed Version-Family Lifecycle

**ID**: `cpt-cf-types-registry-adr-managed-version-family-lifecycle`

## Table of Contents

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Several members may be ACTIVE](#several-members-may-be-active)
  - [The newest member is neither stored nor computed](#the-newest-member-is-neither-stored-nor-computed)
  - [What the family record retains](#what-the-family-record-retains)
  - [Deprecation is deferred](#deprecation-is-deferred)
  - [External Registry Sources](#external-registry-sources)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [One usable member per family](#one-usable-member-per-family)
  - [Several usable members per family](#several-usable-members-per-family)
- [More Information](#more-information)
  - [Industry Practice](#industry-practice)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

ADR-0004 makes a major-only managed GTS Identifier a mutable logical entity, a minor-bearing one immutable, and defines what a managed version family is: the canonical identifier with the whole version of its last segment removed, every preceding segment held exactly. It does not decide what happens to the members of such a family over time — whether more than one may be used at once, how a consumer learns which is newest, and what the family record has to hold in order to enforce any of it.

The obvious answer to all three is one stored value: at most one member of a family is `ACTIVE`, and publishing a higher major atomically demotes the previous member to `DEPRECATED`. That is the rule this ADR weighs and does not adopt, because the single value it stores answers two unrelated questions:

* **Which member is newest?** A fact about the family, derivable from the identifiers of its members at any moment.
* **Does the owner want consumers to stop adopting a member?** An intent, which only the owner can express and which no amount of registry state can infer.

Conflating them has consequences that reach well past the status field. Keeping the derived value truthful requires supporting rules that serve nothing else — deleting the newest member has to be forbidden while older members claim an active successor, and admitting a major lower than the current one has to be rejected. Enforcing "at most one active" requires a family-scoped serialization point, because two successor admissions in one family need not touch the same predecessor row and entity-level optimistic concurrency therefore cannot see the conflict.

The question this ADR settles is how many members of a family may be `ACTIVE` at once. What happens to deprecation follows from that answer rather than driving it.

## Scope

This ADR decides:

* whether more than one member of a managed version family may be `ACTIVE` at once;
* whether the registry states which member is newest, and what discovery must offer instead;
* what the version-family record must hold, and whether it needs its own concurrency control;
* the managed Lifecycle Status vocabulary for P1, and how external source assertions map onto it;
* whether P1 has a deprecation concept for Managed Entities, and in what form it returns if it does not.

This ADR does not decide the version-family definition, minor-version policy, or reference and derivation rules, which belong to ADR-0004. It does not decide content revision identity and retention (ADR-0005, ADR-0006), deletion preconditions and dependency safety, the tenant ownership, visibility, and availability model, or storage schema, which belongs to DESIGN.

## Decision Drivers

* A consumer holds a Registry Reference for one exact identifier (ADR-0001) and never negotiates a version at request time, so no operation selects among family members on the consumer's behalf.
* Lifecycle state must not assert something the registry cannot keep true without additional rules.
* A signal that is derivable should not be stored, because stored copies of derivable facts require invariants, serialization, and repair paths.
* Owner intent cannot be derived, and a value that carries no intent cannot substitute for one.
* P1 should not ship a transition for which no consumer has been named.
* An External Registry Source owns its own lifecycle rules and cannot be required to adopt the platform's.

## Considered Options

* **One usable member per family.** At most one member is `ACTIVE`; the others carry a status derived from version succession.
* **Several usable members per family.** Any number of members may be `ACTIVE`, and the registry makes no statement about which is newest.

## Decision Outcome

Chosen option: **several members of a version family may be `ACTIVE` at once, and the family record retains nothing but its owner scope.** The registry does not nominate a newest member — version ordering is already carried by the identifiers. Managed deprecation is not built in P1; when it arrives it will be authored rather than derived.

### Several members may be ACTIVE

The managed Lifecycle Status of an admitted logical entity in P1 is `ACTIVE` or `DELETED`.

* Initial admission creates the entity `ACTIVE` with content revision `1`.
* Admitting a content revision does not change Lifecycle Status.
* Admitting a higher-major Version Successor changes nothing about any other member of the family. Publishing `v2~` leaves `v1~` `ACTIVE`.
* Several members of one family may be `ACTIVE` simultaneously, in any combination of majors and, where ADR-0004's minors are in use, of minors.
* **Majors may be admitted in any order.** A family has no current-member pointer, so nothing can move backwards and out-of-order admission of a major needs no special rule.
* **Minors may not.** ADR-0004 requires the minors of a major to be contiguous and to open at `M.0`, so a new minor is admissible only while its immediate predecessor is. That is not an exception to the rule above but a consequence of a different fact: succession between majors carries no guarantee — a new major is how an incompatible change is published — while succession between minors carries the backward-compatibility statement of ADR-0003, and a guarantee stated along a sequence needs the sequence to have an order. Where a major carries no minors the distinction is invisible, since it has exactly one member.
* Deletion is unchanged and remains governed by its own preconditions: an `ACTIVE` member may transition directly to terminal `DELETED`. Deleting any member has no effect on the status of the others.
* P1 has no managed deprecation, undeprecation, or restore transition.

`ACTIVE` is a lifecycle status and not a statement that a tenant may use the entity. Those are different dimensions: ADR-0010 computes Tenant Availability State separately, per tenant, from lifecycle, visibility, and the semantic dependency closure, so an `ACTIVE` member can still be `UNAVAILABLE` for a given tenant — and two tenants can receive different verdicts for one member. Where the options below and the discussion above say "usable", they are naming the question this ADR was asked; the answer it gives is about `ACTIVE`, and each tenant evaluates usability independently.

This is what the platform's consumers actually do. A consumer holds a reference to one exact identifier and is not offered a choice among majors, so nothing in the registry needs to nominate one member as the one to use.

### The newest member is neither stored nor computed

A derived replacement for the stored status suggests itself: rather than recording `DEPRECATED`, resolution and discovery would report whether an entity is the highest major among the non-deleted members of its family, computed at request time.

That replacement is not adopted, because the argument against the stored status applies to it unchanged. `DEPRECATED` is rejected here partly because it restates version ordering that the identifiers already encode — and a computed newest-member flag restates exactly the same thing. A caller that can enumerate a family reads the ordering from the members' identifiers by inspection; the registry would be saving a comparison, not supplying information.

Applying this ADR's own standard settles the rest. Authored deprecation is deferred below because P1 has no named consumer for it. The derived property has no named consumer either: no use case, actor, or requirement in the PRD reads it, and by the first decision driver above a consumer holding one exact identifier does not negotiate versions at request time. Building it would put a second family query on the resolution path to serve nobody.

What P1 does need is smaller and has an obvious consumer: **discovery must be able to enumerate the members of a version family.** A pattern query cannot express that on its own, because a GTS wildcard is greedy across the chain separator and so also captures types derived from a family member. The family key gives exact membership as a discovery filter.

The thesis of this decision — an ordering fact is computed, an intent is declared — is thereby carried to its conclusion. The ordering fact is already present in the data the caller receives, so the registry neither stores it nor computes it.

### What the family record retains

Without the one-usable-member invariant nothing has to be **demoted atomically with anything else**, so the family-scoped compare-and-swap that invariant would have required is unnecessary. That is narrower than saying family admissions do not serialize, and the difference matters: the family row is already locked for the ownership check of ADR-0009, and ADR-0004 reads the members under that same lock for kind exclusivity. What this decision removes is a compare-and-swap over a *status* that every membership change would have had to update — not the lock itself.

A family-scoped record still exists, in a much reduced form. It binds a family key to its owner scope, which makes `owner_scope(version_successor) == owner_scope(version_family_root)` enforceable by a uniqueness constraint and an ordinary read. Identifier reservation stays where ADR-0001 already put it, in per-identifier tombstones that survive deletion.

### Deprecation is deferred

P1 has no named consumer for owner-authored deprecation: the requirement was introduced as machinery for the one-usable-member invariant, not in response to a stated need. It is therefore not built.

Nor is it scheduled for P2. The absence of a named consumer is not a P1 accident that the next phase resolves, and none of the P2 capabilities — Aliases, casting, Validation Hooks, tenant enablement — needs it or is blocked by it. Deprecation is deferred until a consumer names it, and PRD open question 1 stays open until then rather than being carried as scheduled work.

Deferring it is safe precisely because a deprecated entity would behave identically to an active one — it would remain resolvable, discoverable, tenant-available, and valid as a reference target, and would only discourage new adoption. Nothing in P1 depends on its absence, so introducing it later is additive and requires no migration of P1 state.

When it is introduced it will be **authored**: an explicit operation on one member by an authorized owner, carrying a reason and optionally a sunset date, reversible, and independent of version succession in both directions. Publishing a successor will not deprecate anything, and a member with no successor at all will be deprecable — which is precisely the case the derived model could not express. Whether it lands as a third Lifecycle Status or as an annotation orthogonal to lifecycle is left to that decision; the evidence surveyed below points to an annotation.

### External Registry Sources

`DEPRECATED` is deferred out of P1 on the external side too, so the P1 Lifecycle Status vocabulary is `ACTIVE` or `DELETED` for every entity the platform exposes.

External-only deprecation has no stronger case. Managed deprecation is deferred because it has no named consumer; a source assertion has none either, and the status is advisory. As stated above, a deprecated entity behaves exactly like an active one in P1.

Retaining the value would make every consumer handle a third case that:

* no managed entity can hold;
* changes no behavior;
* appears only when an external source is configured;
* cannot be tested against managed entities.

The source assertion is therefore not exposed as a P1 lifecycle value.

Two rules for the federation contract follow, and ADR-0002 states them there:

* a source **may** assert deprecation, and Types Registry exposes such an entity as `ACTIVE`. That is a statement of the P1 truth rather than an approximation of it: deprecation discourages new adoption and changes nothing else, and the entity is genuinely usable. The platform invents no third status to carry the assertion and does not require a source to identify a Version Successor for one, because under this decision nothing causes deprecation except its owner's intent;
* the assertion is not relayed to consumers in P1, and the federation contract must say so plainly so that a vendor learns it before integrating. This is the one real cost of the deferral, and it is accepted rather than hidden.

Introducing the status later remains additive on both sides: no P1 state has to be reinterpreted, because no P1 entity carries it.

### Consequences

* There is no one-usable-member invariant, and therefore no compare-and-swap over a member status, no deletion-ordering rule to protect it, and no rejection rule for out-of-order admission **of a major**. None of the three would describe anything a user wants; each exists only to keep a derived value truthful. What does remain is the family-row lock that ADR-0009 takes for ownership and ADR-0004 reads under, and the contiguous-minor rule that rejects a gapped or out-of-order **minor** — neither of which this decision introduces or removes.
* No family-wide compare-and-swap over a member status sits on a write path that every gear uses at startup. Concurrent admissions into one family still serialize on the family row, which the ownership check of ADR-0009 requires independently of anything decided here.
* Discovery must be able to enumerate a version family exactly, which a greedy GTS wildcard cannot do on its own. This is the whole of what the stored status would have signalled, and it is a filter rather than a computed field.
* No consumer observes `DEPRECATED` in P1 from any origin, so SDK and REST contracts model a two-value Lifecycle Status and no consumer carries a branch it cannot exercise. The cost is that a source-asserted deprecation is dropped rather than relayed, which the federation contract must disclose.
* Family membership can grow without bound in principle: nothing forces an owner to delete old majors. Which majors remain supported becomes an owner declaration rather than a registry invariant, and P1 has no place to record that declaration. This is the cost of deferring authored deprecation and is accepted.
* **ADR-0004's minors raise the rate at which that cost accrues, without changing its nature.** A minor exists to be pinned, so dependents pin to it and deletion is refused while they do. This does not reopen the decision — deferral rested on there being no named consumer for the signal, and a minor-bearing family supplies none either — but it is the clearest pressure yet on PRD open question 1.
* Deleting a member of a family with several `ACTIVE` members is an ordinary deletion. There is no ordering constraint between members.
* Introducing authored deprecation later requires no migration, because no P1 state has to be reinterpreted.

### Confirmation

This decision is confirmed when:

* publishing a higher-major Version Successor leaves every other member of the family `ACTIVE` and changes no other entity's state;
* several majors of one family are simultaneously `ACTIVE`, resolvable, and valid reference targets;
* members admitted out of ascending major order are all admitted successfully, and admission order affects nothing;
* minors of one major admitted out of ascending order or over a gap are rejected, and a major opened at anything but `M.0` is rejected, while majors admitted out of order in the same family continue to be accepted — proving the two rules are distinguished rather than conflated;
* concurrent admissions into one family require no family-scoped compare-and-swap over a member status, and contend only on the ownership lock that ADR-0009 imposes regardless;
* discovery enumerates exactly the members of a family, excluding types derived from any of them, and no response field states which member is newest;
* registering a family member under an owner scope different from the family's is rejected by the family record;
* no P1 operation exposes deprecate, undeprecate, or restore for a Managed Entity;
* no entity of any origin is returned with Lifecycle Status `DEPRECATED` in P1, and the exposed vocabulary has exactly two values;
* an externally managed entity whose source asserts deprecation is exposed as `ACTIVE`, remains resolvable and a valid reference target, and the platform requires no Version Successor to be identified for the assertion.

## Pros and Cons of the Options

### One usable member per family

Publishing a higher major atomically demotes the previous member to `DEPRECATED`. At most one member is `ACTIVE`.

* Good, because the signal is automatic: an owner cannot forget to mark the older contract, and the registry can state with certainty which member is current.
* Good, because "which member should I build against" has a stored answer that needs no computation.
* Bad, because the stored value carries no owner intent. It restates version ordering, which the identifiers already encode, and cannot express "stop using this" for a member that has no successor.
* Bad, because the derived value must be kept truthful by additional rules that serve nothing else: deleting the newest member has to be forbidden while older members claim an active successor, and admitting a lower major than the current one has to be rejected.
* Bad, because the invariant cannot be enforced by entity-level optimistic concurrency — two successor admissions in one family need not touch the same predecessor row — so it requires a family-scoped serialization point with compare-and-swap on every membership change.
* Bad, because no surveyed system restricts usability to one version per family, and none derives deprecation from the publication of a successor.

### Several usable members per family

* Good, because each signal is left to the mechanism suited to it: an ordering fact stays in the identifiers that already carry it, and an intent is declared by its owner.
* Good, because it removes an invariant, a serialization point, and two rules that existed only to protect them.
* Good, because it matches the shape every surveyed system uses.
* Good, because deferring authored deprecation is safe: a deprecated entity would behave identically to an active one, so adding it later is additive.
* Bad, because P1 has no way for an owner to discourage a contract, and no registry-enforced statement of which majors remain supported.
* Bad, because an owner who publishes many majors leaves consumers with nothing but version ordering as guidance until authored deprecation exists.

## More Information

### Industry Practice

Two claims carry the decision, and the surveyed systems agree on both.

**Several versions are usable at once, by design.** [Kubernetes CustomResourceDefinition](https://kubernetes.io/docs/tasks/extend-kubernetes/custom-resources/custom-resource-definition-versioning/) serves multiple versions concurrently and states that "it is perfectly safe for some clients to use the old version while others use the new version". Note where exactly-one *is* required there: for `storage`, of which "there must be exactly one version marked as storage=true" — a mechanical necessity, never a restriction on usability. Azure Resource Manager and Stripe likewise keep many dated API versions usable concurrently and announce retirement out of band.

**Deprecation is authored, never derived from publishing a successor.** [Kubernetes CRD](https://kubernetes.io/docs/reference/kubernetes-api/extend-resources/custom-resource-definition-v1/) gives each version a `deprecated` boolean and an optional `deprecationWarning`, both written by the CRD author and separate from `served`, which controls reachability. [npm](https://docs.npmjs.com/cli/v10/commands/npm-deprecate) requires an explicit command carrying a message, permits only the package owner to issue it, and makes it reversible by passing an empty string; publishing a newer version deprecates nothing. OpenAPI and Protocol Buffers both express deprecation as an authored annotation on the element.

Confluent Schema Registry and AWS Glue Schema Registry are worth noting for what they lack: no deprecation concept at all. A subject simply keeps a version history, and clients read the ordering from it. Both do expose a distinguished latest version, but they can, because a subject is a single mutable sequence a producer resolves against at write time. A GTS family is not that: a consumer holds a reference to one exact identifier and never asks the registry to pick a member for it, which is why the same affordance would serve nobody here.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0004**: [0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md](./0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md)
- **ADR-0005**: [0005-cpt-cf-types-registry-adr-type-schema-revisions.md](./0005-cpt-cf-types-registry-adr-type-schema-revisions.md)
- **ADR-0006**: [0006-cpt-cf-types-registry-adr-registered-instance-revisions.md](./0006-cpt-cf-types-registry-adr-registered-instance-revisions.md)

This decision directly addresses:

* `cpt-cf-types-registry-fr-lifecycle` - allows several `ACTIVE` members per family, defines the P1 managed lifecycle vocabulary, removes automatic deprecation, and defers authored deprecation past P1.
* `cpt-cf-types-registry-fr-minor-version-profile` - bounds the any-order admission freedom to majors, leaving the contiguity rule for minors to that requirement, and admits the minors of one major as simultaneously `ACTIVE` members.
* `cpt-cf-types-registry-fr-id-resolution` - introduces no `latest` or newest-member resolution mode; exact resolution stays literal per ADR-0004.
* `cpt-cf-types-registry-fr-ref-tracking` - every non-deleted member of a family remains a valid reference target.
* `cpt-cf-types-registry-fr-externally-managed-entities` - maps source-asserted deprecation without requiring a Version Successor or imposing the managed model on a source.
* `cpt-cf-types-registry-nfr-multi-pod-correctness` - removes the family-scoped serialization point that the previous invariant required.
