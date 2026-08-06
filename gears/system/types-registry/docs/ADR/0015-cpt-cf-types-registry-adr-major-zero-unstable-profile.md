---
status: accepted
date: 2026-08-04
decision-makers: Constructor Fabric Steering Committee
---

# Unstable Major-Zero Profile for Managed Type Schemas

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [What major 0 exempts, and what it does not](#what-major-0-exempts-and-what-it-does-not)
  - [The quarantine rule](#the-quarantine-rule)
  - [Why the identifier is the right carrier](#why-the-identifier-is-the-right-carrier)
  - [Registered Instances](#registered-instances)
  - [Nothing is stored](#nothing-is-stored)
  - [Graduation is an ordinary registration](#graduation-is-an-ordinary-registration)
  - [What the platform does not promise](#what-the-platform-does-not-promise)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep one enforced mode and iterate with new majors](#keep-one-enforced-mode-and-iterate-with-new-majors)
  - [Keep one enforced mode and iterate with delete, purge, re-register](#keep-one-enforced-mode-and-iterate-with-delete-purge-re-register)
  - [A configurable compatibility mode per family or namespace](#a-configurable-compatibility-mode-per-family-or-namespace)
  - [A stored stability flag on the entity](#a-stored-stability-flag-on-the-entity)
  - [An unstable profile keyed on major 0 in the identifier](#an-unstable-profile-keyed-on-major-0-in-the-identifier)
- [More Information](#more-information)
  - [Industry Practice](#industry-practice)
  - [Relationship to the GTS specification](#relationship-to-the-gts-specification)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-types-registry-adr-major-zero-unstable-profile`

## Context and Problem Statement

ADR-0003 enforces `BACKWARD` compatibility on every managed Type Schema and rejects a candidate whose compatibility the implementation cannot establish. That is the correct rule for a published contract, and the wrong one for a contract still being designed.

A type under active development changes shape, and the platform offers an author two ways to do it. A new major identity is one: it churns the identifier, issues a new Registry Reference, and leaves the abandoned major `ACTIVE`, because ADR-0008 defers deprecation past P1 and nothing retires a major automatically. The purge of ADR-0013 is the other: it is disabled in production by default, and it works by *releasing the identifier*, so by construction it cannot serve a type that anything already references — releasing the name is precisely what rebinds a stored reference to an unrelated entity.

Neither serves the case this decision is about: a type whose shape is not settled while consumers already exist. In that state the author knows the change is breaking, accepts responsibility for it, and needs the identifier and the reference to survive it.

The GTS grammar already admits major 0 — `major = "0" | positive-integer`, and §10 uses `v0` in its own wildcard examples — and the managed identity profile has never said anything about it. A `v0` identifier is therefore registrable today and carries exactly the same enforcement as any other major. The question this ADR answers is not whether major 0 may be used, but whether it should mean something.

## Scope

This ADR decides:

* whether major 0 marks a distinct evolution profile for managed Type Schemas;
* which admission checks that profile exempts, and which it leaves in force;
* how an unstable entity is prevented from weakening a stable one through a floating reference;
* whether registered Instances participate;
* whether the profile is stored, and how an entity leaves it;
* what the platform does and does not promise for an unstable entity.

This ADR does not decide the enforced mode itself or the comparison baseline (ADR-0003), managed identity semantics (ADR-0001, ADR-0004), version-family lifecycle (ADR-0008), the dialect profile (ADR-0014), or the write-path protocol through which the checks run (ADR-0012). It does not change anything for Externally Managed Entities, whose evolution rules their source owns (ADR-0002).

## Decision Drivers

* A type under active development must be able to change shape while its GTS Identifier and Registry Reference stay stable, because those are what consumers have already persisted.
* Purge releases the identifier and so cannot serve a referenced type; a new major forces every consumer to re-point on every reshape rather than once.
* A managed `$ref` floats to the current revision (ADR-0004), so the guarantee a Type Schema offers is the weakest guarantee in its resolution closure. Any relaxation must be prevented from leaking upward.
* Responsibility must be localizable. An owner accepting risk for its own type must not be able to transfer it silently to an owner who accepted nothing.
* ADR-0003 examined and rejected configurable compatibility modes for five reasons. A new relaxation must not reintroduce them.
* Whatever marks the relaxation must be legible to a consumer without a second lookup, since a consumer that cannot see the risk cannot accept it.
* The check must be decidable at admission from state Types Registry owns (`cpt-cf-types-registry-principle-local-authority`).

## Considered Options

* Keep one enforced mode and iterate with new majors.
* Keep one enforced mode and iterate with delete, purge, re-register.
* A configurable compatibility mode per version family or per identifier namespace.
* A stored stability flag on the entity.
* An unstable profile keyed on major 0 in the identifier.

## Decision Outcome

Chosen option: **an unstable profile keyed on major 0 in the identifier.** A managed Type Schema whose own last segment carries major 0 evolves without the enforced compatibility check of ADR-0003, and no entity outside that profile may depend on one.

### What major 0 exempts, and what it does not

The exemption is narrow and is stated as a list so that nothing is waived by implication.

**Exempted.** Type Schema Evolution Compatibility. A content revision of a v0 Type Schema is admitted whatever its relation to the current revision: narrowing, widening, incomparable, or undecidable. ADR-0003's fail-closed rule for an unprovable verdict does not apply, because there is no verdict to establish.

The freeze state machine of ADR-0003 does not apply either. It exists to protect a whole-history guarantee across a semantic change of the compatibility relation, and a v0 entity carries no such guarantee, so there is nothing to freeze and nothing for the repair pass to revalidate. A rules-change repair **MUST** skip v0 entities rather than fail them.

**Not exempted, and each for its own reason.**

* **Type Derivation Compatibility.** `Valid(derived) ⊆ Valid(base)` is a property of the identifier chain, not of evolution — the specification separates the two relations precisely so they can be reasoned about apart (§4.1 against §4.2). Waiving it would make a chained identifier state a substitutability that does not hold, which is a lie in the one place GTS encodes meaning in the string itself.
* **Dependent revalidation.** ADR-0005 requires every affected registered dependent to remain valid before a new revision becomes current, and that rule stands. It is consequently still possible for a v0 reshape to be refused: if a v0 derived type would stop satisfying its base, the base's reshape fails. The remedy is to fix or delete the dependent, not to weaken the chain.
* **The dialect profile.** ADR-0014 pins Draft-07 and pins it for the life of the logical entity. A v0 entity is not exempt: the dialect governs what `Valid(S)` even means, and the derivation check above still needs both sides evaluated under one semantics.
* **Everything else on the admission path.** GTS validity and the managed identity profile, reference resolvability, deletion safety, ownership, registration authority, and the write-path preconditions of ADR-0012 all apply unchanged. An unstable type is unstable in its shape, not in its governance.

### The quarantine rule

**A managed entity whose own last segment carries a major of 1 or higher MUST NOT reference or derive from an entity whose last segment carries major 0.** The prohibition is on a **direct** edge — a `$ref` target, an `x-gts-ref` target, or the immediate derivation base. The corresponding property of the whole resolution closure follows by induction rather than needing to be checked: an entity that carried the exemption transitively would itself have had to be admitted holding a direct edge to an unstable target, which this rule refuses. That is the same shape of argument ADR-0003 uses to reach a whole-history guarantee from a candidate-versus-current check.

Its base case is a **precondition rather than a theorem**, and the difference is worth stating because the induction rests on it. The GTS grammar has always admitted major 0 (§2.1), and the managed identity profile said nothing about it until now, so a registry that has been running can in principle hold a v0 entity — and a stable entity that already references one. Rejecting only new edges would then leave the quarantine asserted but not established. Enabling the rule therefore requires the base case to hold, established by a **preflight scan**: one pass over `dependency` joined to `entity.gts_id`, looking for a subject whose own last segment carries a major of 1 or higher and a target whose carries 0. It is the same comparison admission performs, run once. A deployment where the scan is empty — which is every deployment that has admitted only stable majors, and the expected case for a first release under this profile — satisfies the base case and needs nothing further. A deployment where it is not empty must resolve the offending edges before the rule is enabled, since no grandfathering is offered: an exempt edge left in place is exactly the leak the rule exists to prevent.

Without it the profile is not a profile but a leak. A `$ref` floats to the current revision, so a stable `customer.v1~` referencing an unstable `address.v0~` has its own accepted-instance set redefined whenever the address author reshapes — and, critically, **with no revision of `customer.v1~` at all**: §1.1 of DESIGN records that a current-state projection is recomputed when a floating dependency advances without producing an authored revision here. ADR-0005's dependent revalidation does not catch this, because it establishes that the dependent remains *valid*, not that it remains compatible with what it accepted yesterday. The owner of `customer.v1~` would lose a guarantee it made to its own consumers through an act of a different owner. That is the opposite of localizing responsibility.

The rule costs no new machinery and no registry state at all. A candidate's direct references are its own identifier chain plus the `$ref` and `x-gts-ref` targets in the submitted document, and a major version is readable from each of those identifiers, so the check is a static property of what the caller sent — the same standing ADR-0014's dialect check has, and the same place in the admission sequence. ADR-0011 is what keeps it well-posed: the boundary is closed, so every target named in a managed document is a Managed Entity whose identifier the platform is entitled to interpret.

The relation is one-way and that is deliberate. An unstable type **MAY** build on a stable one — weaker on stronger is sound, and it is the normal case, since a new type under development usually derives from a published base. Only stronger-on-weaker is refused.

### Why the identifier is the right carrier

ADR-0003 considered making the compatibility mode configurable and rejected it. Four of its five objections do not reach this decision, and the difference in every case is that the profile lives in the identifier rather than in stored policy.

* *"It would require new state."* It requires none. The profile is a substring of a column the registry already holds.
* *"The mode would be effectively immutable once chosen, and selected before the consumers who care about it exist."* It is immutable here too, but it is not hidden: it is in the identifier a consumer holds, reads in logs, and stores as a reference. And it is escapable — graduation to v1 is an ordinary registration.
* *"It introduces a second prefix-policy system over the identifier space that Source Claims already partition."* There is no policy and no prefix system. There is one field of one segment.
* *"A managed `$ref` floats, so the effective mode of a closure has to be computed, pinned at admission, and rechecked whenever any member changes."* The quarantine rule replaces that computation with a prohibition, and the prohibition cannot drift: stability is a property of an identifier, an identifier never changes, and a v0 entity never becomes a v1 entity — `v1` is a different entity in the same family. A closure that was uniform at admission stays uniform for the life of its members.

The fifth objection — *"derivation crosses ownership: a tenant deriving from a platform base cannot hold its own type to a stronger guarantee than the base whose mode another owner controls"* — is answered by the quarantine rule rather than dodged: the situation it describes is unrepresentable, because a stable derived type cannot have an unstable base.

### Registered Instances

The profile is defined for Type Schemas, and two consequences follow for Instances.

**Major 0 carries no meaning on a registered Instance identifier and MUST be refused there.** ADR-0006 establishes that successive Instance values have no compatibility relation at all, so there is nothing for the profile to exempt — an Instance value is already free to change. Admitting a marker that means "unenforced evolution" onto an entity whose evolution was never enforced would leave the marker meaning two things, and a reader could no longer conclude anything from seeing it. The restriction is one comparison at admission and joins the two narrowings the managed profile already carries: no minor version (ADR-0004) and no explicit UUID tail (ADR-0001).

**A registered Instance MUST NOT conform to a v0 Type Schema.** ADR-0006 forbids a schema revision from becoming current while an affected registered Instance would cease to be valid. Applied to a v0 schema that rule would restore exactly the block this decision exists to remove; waived, it would leave admitted Instances failing validation against their own current schema while `instance.validated_type_schema_revision_no` records a revalidation that no longer holds. Refusing the combination is the only option that leaves both records truthful. The cost is real and is accepted: a control-plane type and its Instances cannot be developed together under the unstable profile, and such a type is published at v1 from the start.

### Nothing is stored

Types Registry **MUST NOT** store the profile as a column. It is derivable from `entity.gts_id`, which the registry retains in full, so a stored copy would be a second authority over a derived fact and would need an invariant to keep the two from diverging — which `cpt-cf-types-registry-principle-derive-not-store` prohibits. This is the same reasoning ADR-0014 applied to the declared dialect, and it has the same consequence: `database.sql` does not change.

### Graduation is an ordinary registration

An unstable type becomes stable by being registered at v1. No operation, no transition, and no migration is required, because the existing model already covers it: `family(gts.cf.b.c.d.v0~)` and `family(gts.cf.b.c.d.v1~)` are the same family key under ADR-0004, ADR-0008 permits several members of one family to be `ACTIVE` simultaneously and in any order, and ADR-0009 fixes one owner for the whole family through the family record. The v0 member stays `ACTIVE` until its owner deletes it.

Graduating a type that is used as a **base** is expensive, and the cost belongs to ADR-0004 rather than to this decision. A family key holds every preceding segment exactly as written, so `A.v0~B.v1~` and `A.v1~B.v1~` are different families: moving a base out of the unstable profile orphans everything derived from it, which must be re-registered under new identifiers and receives new Registry References. The unstable profile is therefore appropriate for leaf types and expensive for bases, and authoring guidance must say so.

### What the platform does not promise

Three limits are stated here so that they are not discovered later.

**A v0 type gives no protection to runtime data held by other gears.** Types Registry cannot see domain objects, and the reshape of a v0 schema may leave live rows no longer conforming to it. This introduces no new class of hazard: `cpt-cf-types-registry-fr-lifecycle` already permits an `ACTIVE` type to be deleted while live domain data conforms to it, and records that as a stated P1 limitation rather than a guarantee. Deleting a contract under live data is strictly more destructive than reshaping it. What changes is only that the risk is now legible in the identifier.

**Nothing prevents a production consumer from depending on a v0 type.** The marker is a convention backed by review, not an invariant backed by a check. P1 has no lever to make it one: managed tenant enablement is deferred to P2 by `cpt-cf-types-registry-fr-tenant-enablement`, and the grant model of `cpt-cf-types-registry-fr-registration-authority` governs writing rather than reading. A deployment that wants the marker enforced on the read path needs a capability P1 does not have.

**Discovery cannot currently exclude unstable types.** A GTS wildcard has no negation and `GET /entities` has no stability filter, so a catalogue view that wants published contracts only cannot express it. Adding the filter later is additive; it is recorded as an open question rather than built now.

### Consequences

* An author with an unsettled contract keeps one identifier and one Registry Reference across any number of breaking reshapes, and pays a single re-point at graduation instead of one per reshape.
* Admission acquires one comparison over the candidate's direct references, placed beside ADR-0014's dialect check. It reads a wider edge set — `x-gts-ref` targets are quarantined here and excluded from the dialect check, which never inlines them — and a cheaper one, since a major is readable from an identifier and no target document has to be loaded.
* The managed identity profile acquires a third narrowing: no minor version, no explicit UUID tail, and no major 0 on a registered Instance identifier.
* A v0 reshape can still be refused, by dependent revalidation, when a derived type would stop satisfying its base. This is derivation compatibility doing its job and is not a defect of the profile.
* The rules-change repair pass of ADR-0003 skips v0 entities, which also makes it cheaper.
* No storage change, no migration, and no new operation.
* The compatibility contract gains one value in each of two enumerations: an unenforced compatibility mode, and an unenforced chain state, since a v0 entity is neither proven nor frozen.
* A control-plane type and its registered Instances cannot be co-developed under the profile.
* Introducing the profile is additive to everything already admitted: every existing managed entity has a major of at least 1, so every existing closure satisfies the quarantine rule by construction.

### Confirmation

This decision is confirmed when:

* a content revision of a v0 Type Schema that narrows, widens, or is incomparable to the current revision is admitted **when the non-exempt checks pass**, and the equivalent revision of a v1 Type Schema is rejected;
* the preflight scan for a stable subject holding a direct edge to a v0 target reports empty before the quarantine rule is enabled, and reports the offending edges where one exists;
* a v0 candidate whose compatibility the implementation reports as `Unknown` is admitted rather than failed closed;
* a v0 derived Type Schema that violates its base chain is rejected, proving derivation compatibility is not waived;
* a v0 Type Schema candidate declaring a dialect other than Draft-07 is rejected, proving ADR-0014 is not waived;
* admission rejects a v1 Type Schema carrying a `$ref` or `x-gts-ref` to a v0 target, and rejects a v1 identifier deriving from a v0 base, with diagnostics naming the offending member of the closure;
* admission accepts a v0 Type Schema that references and derives from v1 targets, proving the quarantine relation is one-way;
* a v0 reshape that would leave a v0 derived type no longer satisfying its base is refused by dependent revalidation;
* registration of a managed registered Instance whose own last segment carries major 0 is rejected;
* registration of a registered Instance conforming to a v0 Type Schema is rejected;
* no column of Types Registry storage holds a stability value, and no migration accompanies this decision;
* registering v1 of a family whose v0 member is `ACTIVE` succeeds, leaves the v0 member `ACTIVE`, and is refused for any owner other than the family's;
* a read result reports the unenforced compatibility mode and the unenforced chain state for a v0 entity, and never a bare compatibility verdict;
* the ADR-0003 repair pass after a semantic change of the compatibility relation skips v0 entities rather than freezing or failing them;
* an Externally Managed Entity whose source serves a v0 identifier is resolved and returned without any of these rules being applied to it.

## Pros and Cons of the Options

### Keep one enforced mode and iterate with new majors

* Good, because every managed entity carries one guarantee and a consumer never has to read the identifier to know which.
* Good, because it needs no decision at all.
* Bad, because every breaking reshape issues a new identifier and a new Registry Reference, so every consumer re-points on every iteration rather than once.
* Bad, because ADR-0008 defers deprecation past P1 and nothing retires a major, so an iterated type accumulates `ACTIVE` majors that are deletable only while nothing depends on them.

### Keep one enforced mode and iterate with delete, purge, re-register

* Good, because ADR-0013 already built it and it needs nothing new.
* Good, because the development stand keeps rehearsing production exactly, which is that ADR's central argument.
* Bad, because purge releases the identifier, which is a data-corruption primitive by ADR-0013's own description: deterministic derivation reproduces the reference and any row still holding it silently rebinds. It therefore cannot serve a type anything already references, which is the case this decision is about.
* Bad, because purge is disabled in production by default, so the relief is unavailable in the environment where consumers are most likely to already exist.

### A configurable compatibility mode per family or namespace

Examined and rejected by ADR-0003; retained here because the chosen option occupies the same design space.

* Good, because it would let a contract that genuinely needs `FULL` ask for it without imposing it platform-wide.
* Bad, for the five reasons ADR-0003 records: the configurable set could only be `BACKWARD` and `FULL`, the effective mode of a floating reference closure would have to be computed and rechecked, derivation crosses ownership, the mode would be effectively immutable but invisible, and a namespace-attached mode would be a second prefix-policy system over the identifier space.
* Bad, because it needs durable per-family policy state, which the chosen option does not.

### A stored stability flag on the entity

A boolean or enumeration column on `entity`, set at initial admission and immutable thereafter.

* Good, because it separates the concept from the version number, so a type could be marked unstable at any major.
* Good, because discovery could filter on it directly, which the chosen option cannot express.
* Bad, because it is invisible where it matters most. A consumer holds an identifier and a Registry Reference; under this option neither says anything, so the risk can only be learned by a lookup that a consumer has no reason to perform.
* Bad, because it stores a fact that would otherwise be derivable, which `cpt-cf-types-registry-principle-derive-not-store` prohibits, and it needs a migration and an immutability invariant that the chosen option gets from the identifier for free.
* Bad, because it reintroduces the computed-closure problem: with stability orthogonal to the identifier, a closure's composition is no longer fixed by construction and would have to be established and maintained.

### An unstable profile keyed on major 0 in the identifier

* Good, because the risk is legible in the value every consumer already holds, so accepting it is an informed act rather than an assumption.
* Good, because it needs no storage, no migration, no new operation, and no new state, and graduation falls out of the existing version-family model.
* Good, because the quarantine rule makes responsibility genuinely local: an unstable type can harm only entities whose owners also opted in.
* Good, because it is additive — every entity admitted before it has a major of at least 1, so no existing closure has to be re-examined.
* Good, because it follows the convention every developer already knows from SemVer, Go modules, Cargo, and Kubernetes alpha versions, so it needs no explaining to the actors who will use it.
* Bad, because it consumes a major number for a meaning, so a genuine `v0` in the ordinary sense is no longer available. In practice the two coincide.
* Bad, because it cannot be applied to a settled type that later needs one breaking change: that case still needs a new major.
* Bad, because graduating a v0 **base** orphans everything derived from it, so the profile is materially cheaper for leaves than for bases.
* Bad, because nothing on the read path prevents production consumption; the marker is backed by review rather than by a check.

## More Information

### Industry Practice

Marking a not-yet-settled contract by a convention in its version string, rather than by policy state held beside it, is one of the most widely adopted conventions in software distribution. It appears in two spellings.

**Major zero, literally.**

* [Semantic Versioning 2.0.0](https://semver.org/#spec-item-4) is the normative source: major version zero "is for initial development", "anything MAY change at any time", and "the public API SHOULD NOT be considered stable". This decision adopts that meaning unchanged.
* [Go modules](https://go.dev/doc/modules/version-numbers) classify `v0.x.x` as "in development and unstable" and state that such a release carries no backward-compatibility guarantee. Go also shows the one place our model is stricter than the convention: a module keeps the same import path from `v0` through `v1` and only acquires a major suffix at `v2`, so graduating out of the unstable range costs a Go consumer nothing. Here `v0~` and `v1~` are different identifiers and therefore different Registry References, so graduation costs one re-point. The consolation is the one this ADR was written for — one re-point instead of one per reshape.
* [Cargo](https://doc.rust-lang.org/cargo/reference/semver.html) encodes the convention in the resolver itself: a `0.x` requirement is treated as compatible only within that minor, because the ecosystem assumes breaking changes land there. The signal is honoured by tooling with no registry check enforcing it — which is precisely the standing this decision gives it.

**The same idea under a different spelling.**

* [Kubernetes API versioning](https://kubernetes.io/docs/reference/using-api/#api-versioning) carries the stability level in the version string itself — `v1alpha1` — and defines it as software that "may contain bugs", whose support "may be dropped at any time without notice", whose API "may change in incompatible ways in a later software release without notice", and which is "recommended for use only in short-lived testing clusters". This is the closest analogue to what is decided here, because the marker travels inside the identifier a client uses rather than sitting in configuration beside it. Kubernetes additionally disables alpha APIs by default and requires them to be enabled explicitly in the API server — an enforcement lever P1 does not have, and the reason the corresponding limitation is stated plainly above rather than implied away.
* [Google AIP-181](https://google.aip.dev/181) makes stability an explicit, named property of an API component, with `alpha` describing something that "undergoes rapid iteration with a known set of users" and is expressed in the version string as `v1alpha`. It is the same choice of carrier, from the same body of guidance this gear already draws on for resource revisions and freshness validation.
* [Azure Resource Manager](https://learn.microsoft.com/en-us/azure/azure-resource-manager/management/resource-providers-and-types) uses dated API versions with a `-preview` suffix, again in the string a caller sends, with no compatibility guarantee and an independent retirement schedule.

**And the closest product category does not do it at all.** Confluent Schema Registry, AWS Glue Schema Registry, Google Pub/Sub Schemas, and Azure Event Hubs Schema Registry have no analogue: compatibility is configured per subject as policy — the option ADR-0003 examined and rejected — and none encodes stability in the identifier a consumer stores. The convention is therefore borrowed from package and API versioning rather than from schema registries, and that is a deliberate choice rather than an oversight: those registries govern message schemas resolved at write time by a producer, whereas a GTS Type is also depended upon by other Types through a floating reference.

That last difference is why one part of this decision has **no** precedent in any of the systems above. In every one of them the risk is accepted by a consumer choosing to depend on an unstable artifact, and nothing prevents a *stable* artifact from depending on an unstable one — Cargo, Go, and npm all permit a `1.0` crate or module to depend on a `0.x` one, leaving the resulting exposure to the author's judgement. Consumer opt-in is not sufficient here, because a floating `$ref` transfers the exposure to an owner who never opted in and never sees a verdict. The quarantine rule is the platform's own addition.

### Relationship to the GTS specification

The specification permits this without amendment. §2.1 admits major 0 in the grammar and §10 uses it in its own examples. §4.2 leaves the publication shape for successive definitions to the implementation, and §6 item 6 makes the enforced Type Schema Evolution Compatibility mode — and the publication policy around it — implementation-defined, noting in §4.3 that "systems may enforce different modes for different identifier namespaces". Enforcing no mode for one major is inside that latitude.

The specification attaches no meaning to major 0 itself. This decision therefore adds a platform convention rather than reading one out of the specification, in the same way ADR-0004 narrowed the managed profile by forbidding minor versions and ADR-0001 by forbidding an explicit UUID tail. Should the convention prove valuable beyond this platform, it is a candidate for a specification change request rather than something to assume other GTS implementations share.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md) — the Registry Reference this profile exists to keep stable, and the precedent for narrowing the managed identity profile.
- **ADR-0003**: [0003-cpt-cf-types-registry-adr-type-schema-evolution-compatibility.md](./0003-cpt-cf-types-registry-adr-type-schema-evolution-compatibility.md) — the enforced mode this profile exempts, and the rejected configurable-mode option this decision is measured against.
- **ADR-0004**: [0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md](./0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md) — the version-family key that makes graduation an ordinary registration, and the base-graduation cost.
- **ADR-0005**: [0005-cpt-cf-types-registry-adr-type-schema-revisions.md](./0005-cpt-cf-types-registry-adr-type-schema-revisions.md) — dependent revalidation, which is not exempted and remains the one way a v0 reshape can be refused.
- **ADR-0006**: [0006-cpt-cf-types-registry-adr-registered-instance-revisions.md](./0006-cpt-cf-types-registry-adr-registered-instance-revisions.md) — why registered Instances neither carry the profile nor conform to a type that does.
- **ADR-0008**: [0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md) — several members of a family may be `ACTIVE`, which is what lets v0 and v1 coexist during graduation.
- **ADR-0011**: [0011-cpt-cf-types-registry-adr-managed-external-boundary.md](./0011-cpt-cf-types-registry-adr-managed-external-boundary.md) — the closed boundary that makes the quarantine check decidable from local state.
- **ADR-0013**: [0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md](./0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md) — the alternative this decision complements rather than replaces.
- **ADR-0014**: [0014-cpt-cf-types-registry-adr-managed-type-schema-dialect-profile.md](./0014-cpt-cf-types-registry-adr-managed-type-schema-dialect-profile.md) — the closure check this one sits beside, and the precedent for not persisting a derivable profile.

This decision directly addresses:

* `cpt-cf-types-registry-fr-validate-schema-compat` - exempts major 0 from the enforced mode and bounds the exemption with the quarantine rule.
* `cpt-cf-types-registry-fr-gts-validation` - adds major 0 to the managed identity profile for Type Schemas and refuses it on registered Instance identifiers.
* `cpt-cf-types-registry-fr-register-schemas` - widens the admissible evolution of a managed Type Schema whose major is 0.
* `cpt-cf-types-registry-fr-register-instances` - refuses a registered Instance conforming to a v0 Type Schema.
* `cpt-cf-types-registry-fr-ref-tracking` - a dependency edge to a v0 target is admissible only from a v0 subject.
* `cpt-cf-types-registry-fr-lifecycle` - graduation adds a family member and changes the status of none.
