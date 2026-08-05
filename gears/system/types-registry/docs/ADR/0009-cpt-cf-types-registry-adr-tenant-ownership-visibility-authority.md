---
status: accepted
date: 2026-07-26
decision-makers: Constructor Fabric Steering Committee
---

# Tenant Ownership, Visibility, and Management Authority

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Scope](#scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Ownership scopes](#ownership-scopes)
  - [Visibility is directed down the tenant tree](#visibility-is-directed-down-the-tenant-tree)
  - [Version-family ownership versus derivation](#version-family-ownership-versus-derivation)
  - [Identifier uniqueness and the disclosure boundary](#identifier-uniqueness-and-the-disclosure-boundary)
  - [A shared namespace is not a hole in tenant isolation](#a-shared-namespace-is-not-a-hole-in-tenant-isolation)
  - [The contract exposes whether the caller owns an entity, not who does](#the-contract-exposes-whether-the-caller-owns-an-entity-not-who-does)
  - [Deletion blocked by an invisible dependent](#deletion-blocked-by-an-invisible-dependent)
  - [Ownership is immutable, and a mistake is repaired by purge](#ownership-is-immutable-and-a-mistake-is-repaired-by-purge)
  - [Authorization matrix](#authorization-matrix)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Directed subtree visibility](#directed-subtree-visibility)
  - [Strict isolation](#strict-isolation)
  - [Full-tree visibility](#full-tree-visibility)
- [More Information](#more-information)
  - [Industry Practice](#industry-practice)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-types-registry-adr-tenant-ownership-visibility-authority`

## Context and Problem Statement

Types Registry holds platform-owned type contracts and tenant-owned ones in one global GTS identifier namespace. It must decide who owns a registry entity, who can see it, who can change it, and what happens when those answers conflict.

The conflict is not hypothetical. GTS identifiers are globally unique, so two tenants cannot hold the same identifier — but rejecting the second registration tells the second tenant that the identifier is taken, which is a disclosure across a visibility boundary. Symmetrically, a tenant may be unable to delete its own contract because something it cannot see depends on it.

This ADR settles ownership, visibility, and management authority. Whether a visible entity is currently usable is a separate computation, decided by ADR-0010.

## Scope

This ADR decides:

* the ownership scopes a registry entity can have;
* who can see a tenant-owned entity, and in which direction that relation runs;
* how ownership of a version family relates to ownership of types derived from it;
* how the global uniqueness of GTS identifiers is reconciled with the rule against disclosing entities outside their visible scope;
* what happens when deletion is blocked by a dependent the deleting owner cannot see;
* whether a mistaken owner assignment can be corrected;
* the authorization matrix for registry operations.

This ADR does not decide tenant availability evaluation or tenant enablement state (ADR-0010), lifecycle transitions (ADR-0008), identity and reference semantics (ADR-0001, ADR-0004), or the ownership semantics of Externally Managed Entities, which remain source-owned under ADR-0002.

## Decision Drivers

* A reseller or vendor tenant must be able to publish one contract that its sub-tenants use, without duplicating it per sub-tenant.
* GTS identifiers are globally unique, so "the same contract for several tenants" cannot be expressed by copying it under different identifiers.
* Platform tenant-tree barriers protect descendant data from ancestor access; a contract-visibility rule must not be mistaken for a hole in them.
* Visibility must not imply authority: seeing a contract is not permission to change it.
* An owner assignment made in error must be repairable, because identifier reservation makes delete-and-recreate impossible.
* Cross-boundary disclosure must be bounded and deliberate rather than incidental.

## Considered Options

For visibility of tenant-owned entities:

* Directed subtree visibility: the owner and all its descendants.
* Strict isolation: the owning tenant only.
* Full-tree visibility: the owner, its ancestors, and its descendants.

## Decision Outcome

Chosen option: **two ownership scopes, with tenant-owned entities visible to the owning tenant and all of its descendants, and management authority never implied by visibility.**

### Ownership scopes

A Managed Entity has exactly one ownership scope:

* **Global** — platform-owned. Potentially visible to every tenant.
* **Tenant-owned** — owned by one tenant, recorded as `owner_tenant_id`.

An Externally Managed Entity carries the same two scopes, asserted by its owning Registry Source Plugin rather than recorded here. The division of labour is that the source states a **flat** fact — this entity is platform-wide, or it belongs to tenant X — and Types Registry expands it into the directed descendant relation below. That split matches what each side can actually know: a plugin already receives a tenant identity on every request, because ADR-0002 requires it to return tenant enablement state for one, but it need not know the tenant hierarchy, and under this arrangement it does not have to.

The scope is mandatory in a plugin response, so there is no default to get wrong in either direction; an absent assertion, or one naming a tenant the platform does not know, is an `INVALID_SOURCE_RESPONSE` rather than a silently invisible or silently universal entity.

Letting a source assert an owner does not offend `cpt-cf-types-registry-principle-local-authority`, though it looks as if it might. A plugin asserts only about content it already owns in full, so a wrong assertion discloses the vendor's own type to the wrong tenant — a vendor error about vendor data, not a breach of platform isolation, and no third party's data is reachable through it. What the platform does not delegate is the relation: visibility, authorization, and the availability verdict are computed here from what the source returns.

The scope is also **narrower in meaning** than its managed counterpart, and the shape being identical makes saying so necessary. Managed ownership does four jobs — it drives visibility, scopes SecureORM, confers write authority, and answers the "mine" discovery filter. For an externally managed entity only the first and the last apply: nothing is stored, and no write path to an external entity exists or is planned, so the assertion confers no authority over anything.

### Visibility is directed down the tenant tree

A tenant-owned entity is visible to its owning tenant and to every descendant of that tenant. It is not visible to ancestors, siblings, or unrelated tenants. The relation Types Registry evaluates is `is_descendant(requesting_tenant, owner_tenant)`, inclusive of the owner itself.

Strict isolation was rejected because it cannot express the case the platform actually has. A reseller tenant that publishes a contract for its sub-tenants would have to register a separate entity per sub-tenant, and global identifier uniqueness makes that impossible: the same contract cannot exist twice, so each sub-tenant would need a different identifier for what is semantically one type. Every downstream mechanism — derivation chains, Concrete Reference Sets, compatibility — would then treat them as unrelated types.

Full-tree visibility was rejected because it inverts the direction that matters. An ancestor gains nothing from seeing contracts its descendants invented, and exposing them upward leaks the descendant's business structure.

Tenant-tree barriers are not weakened by this rule. Barriers protect descendant **data** from ancestor access; contract visibility flows in the opposite direction, from ancestor to descendant. Types Registry therefore does not apply barrier filtering to the descendant-reads-ancestor relation. Authorization still governs whether a given caller may perform a given operation.

### Version-family ownership versus derivation

Two relations must not be confused.

**Version succession stays with the family.** Every member of a version family has the same ownership scope as the family root: `owner_scope(version_successor) == owner_scope(version_family_root)`. A descendant that uses an ancestor's `Customer.v1~` cannot publish `Customer.v2~`. Only the family owner extends its own family, which is what makes a published contract a contract rather than a shared mutable name.

**Derivation creates a new family with its own owner.** A tenant may derive a new type from any visible type, including one owned by an ancestor or by the platform. The derived type starts a new version family owned by the deriving tenant, which controls its successors and gains no authority over the base or the base's family.

This asymmetry is the point: descendants extend the type system by deriving, not by editing what an ancestor published.

### Identifier uniqueness and the disclosure boundary

Registration of an identifier already held elsewhere must fail, and the failure necessarily tells the caller that the identifier is unavailable. Types Registry accepts that disclosure and bounds it.

A GTS identifier is a name in a global, vendor-structured namespace, not a secret. Every globally unique namespace leaks name existence at registration time — DNS, npm, and crates.io all do — and the structure of `gts.<vendor>.<package>.<namespace>...` already implies which names a given vendor is likely to hold. What must not leak is anything beyond the name.

Therefore:

* a registration conflict returns only that the identifier is unavailable. It **MUST NOT** disclose the owner, the ownership scope, the content, the lifecycle status, or whether the holder is a Managed Entity, a Source Claim, or a deleted-identifier reservation;
* the conflict response is identical whether the existing holder is visible to the caller or not, so it cannot be used to probe scope;
* discovery, search, exact resolution, batch resolution, and query assistance disclose nothing about entities outside the caller's visible scope — not existence, not metadata, not a distinguishable error. Reverse resolution of a Registry Reference for an entity outside the caller's scope returns the same result as one that was never issued.

This narrows the PRD rule against disclosing existence: it holds absolutely for the read and discovery surface, and is bounded to name-availability on the registration surface.

### A shared namespace is not a hole in tenant isolation

The objection is worth answering directly, because a namespace two tenants draw from looks at first like exactly the thing tenant isolation exists to prevent. It conflates two different properties.

**Data isolation** is the property that one tenant cannot read or change another's. It is intact and nothing above weakens it: visibility is the directed descendant relation, a conflict response carries only that the name is unavailable, an out-of-scope reverse resolution is indistinguishable from an unissued reference, and no read result carries an owning tenant identifier on either plane. **Namespace partitioning** — whether the space of *names* is per-tenant — is a separate question, and the answer here is deliberately no.

What crosses the boundary is therefore one bit: *this identifier is unavailable*. And `cpt-cf-types-registry-fr-registration-authority` narrows even that, by requiring the grant check to run **before** the identifier-availability check. A caller with no grant covering the region receives one response whether the name is free, held by a visible entity, held by an invisible one, or held by a tombstone or a Source Claim reservation, so the bit is legible only to a subject that already holds authority over that part of the namespace — in practice, the vendor whose prefix it is. That is a stronger position than the public analogues below, where anyone who can reach the API can probe.

Per-tenant namespaces are not an alternative that was passed over; they are incompatible with three decisions this gear rests on. A chained identifier would stop being unambiguous, since `A~B~` would denote different types depending on whose context resolved it, and a `$ref` would need a tenant to resolve at all. ADR-0001 derives the Registry Reference deterministically from the identifier, so two tenants holding one string would derive one reference — repairable only by folding the tenant into the derivation, which destroys the portability that made it deterministic. And the reseller topology this ADR is built around requires a descendant to derive from a contract an ancestor published, which presupposes one namespace for both.

### The contract exposes whether the caller owns an entity, not who does

Ownership is stored, asserted, and evaluated; it is not returned as a tenant identifier. A read result carries a boolean — the Context Tenant is the owner, or it is not — and no `owner_tenant_id`. On the platform plane, where there is no Context Tenant unless one is named, even the boolean is absent.

Three reasons, and the first is the one that decides it. A tenant identifier is not actionable: nothing in the platform lets one tenant ask another for anything through Types Registry, so the value answers no question its holder can act on. It is also a disclosure this ADR did not otherwise make — a tenant seeing an ancestor-owned contract would learn that ancestor's identity, and by browsing enough of them, the shape of the hierarchy above it, which §Visibility deliberately keeps flowing in one direction. And for an externally managed entity it would additionally surface the vendor's own tenant mapping.

The boolean is deliberately not named for authority. Owning an entity is necessary for managing it and not sufficient: `cpt-cf-types-registry-fr-registration-authority` also requires a grant covering the candidate identifier. A field called "manageable" would promise more than it knows. The composite question — may I change this — is answered by the caller from the boolean and the entity's origin, since no write path reaches an externally managed entity however its source assigns it.

The **global versus tenant-owned** distinction is not exposed either. It was considered: it discloses nothing about the hierarchy and would let a management view group a catalogue into platform contracts, inherited contracts, and one's own. It is left out because no requirement or actor names that grouping, and the discovery filter is narrowed to match rather than left able to select a bucket the response cannot explain. Adding both back later is additive.

A read may name a **Context Tenant** — the platform's term for the tenant scope root of an operation, which may differ from the subject's own — and the availability verdict is then computed for it rather than for the subject. This is not a hole in the directed relation, because the two tenants govern different things: **visibility is evaluated for the subject, availability for the Context Tenant.** Their visible sets are not nested — an entity owned by a descendant is visible to that descendant and not to its parent — so evaluating visibility for the Context Tenant instead would let an ancestor read a descendant's private contracts by naming it. The platform PDP checks that the subject's tenant is an ancestor of the one named.

The same rule governs the filter. Discovery selects by scope — mine, or everything visible — and never by a supplied tenant identifier. Accepting one would reopen on the read surface what `cpt-cf-types-registry-fr-registration-authority` closes on the write surface, where ownership is derived from the `SecurityContext` and never accepted as request data: a caller could otherwise probe for its own ancestors by filtering on a guessed identifier and observing whether the result is empty.

### Deletion blocked by an invisible dependent

An ancestor may be unable to delete its own contract because a descendant registered a dependent on it that the ancestor cannot see.

This is correct behaviour, not a defect. The ancestor published a contract into its subtree and something took a dependency on it; unilateral deletion would break that dependent whether or not the ancestor can see it. The registry is enforcing the contract, not obstructing the owner.

The disclosure boundary still holds. A blocked deletion reports the number of blocking dependents and nothing that identifies them, their owners, or their content. That leaves the owner unable to resolve the block alone, which is deliberate: resolution requires either the dependents' owners to remove them or a platform-authority operation that can see across the boundary. Types Registry exposes the count so the owner can distinguish "blocked" from "failed", and platform operators retain an authorized path to enumerate the dependents: a Dry Run deletion on the platform plane, which runs the same check and is not bound by this disclosure rule. There is no separate enumeration operation.

The same restraint governs **Dry Run diagnostics**, and it has to be said explicitly because the temptation runs the other way. A Dry Run exists to tell a caller precisely what would go wrong, so its natural instinct is to name the offending dependent — but a tenant-plane Dry Run refused by a dependent the caller cannot see must report a count and no identity, exactly as the real deletion does. A rehearsal that discloses more than the act it rehearses would make the disclosure boundary optional.

### Ownership is immutable, and a mistake is repaired by purge

Ownership scope is fixed when an entity is admitted and never changes afterwards. There is no ownership correction operation.

The argument for providing one runs: a mistaken owner cannot be repaired by deleting and re-registering, because ADR-0001 reserves a deleted identifier permanently, so the identifier would be stranded and an immutable owner field would make the mistake permanent. ADR-0013 removes that premise. Purge releases the identifier, so the repair is delete, purge, re-register under the correct owner, and nothing is stranded.

Before admission the question does not arise at all: the owner of a candidate is derived from the requesting context, so correcting it means submitting again.

For an admitted entity the purge route is not merely an equivalent — it is the more honest one. Changing an owner changes the visible audience of a contract, so a correction would have to establish which registered dependents would lose sight of the entity and which entities the new owner would lose sight of, and then reject or migrate them. That is a migration wearing the word "correction". Purge and re-registration make the same disruption explicit: dependents break visibly and are re-registered, rather than being silently migrated under an operation whose name suggests a repair.

The cost is that purge is disabled in production, so a mis-owned production entity cannot be repaired in place. ADR-0013 already contemplates enabling purge there for a specific planned migration, which is exactly what this is. The alternative — a correction operation available in production — would be a way to change who can see a contract without anyone re-approving it.

Two consequences follow for the rest of this ADR. `entity.ownership_scope` and the ownership recorded on a version family are write-once, so the admission path that locks the family row compares them and never updates them. And the question of whether correction between global and tenant ownership is permitted, and whether a target owner must accept it, dissolves rather than being deferred.

### Authorization matrix

Visibility never implies authority. The matrix below is the P1 baseline; concrete permission names belong to DESIGN.

| Operation | Global entity | Tenant-owned entity |
|---|---|---|
| Register | Platform authority — a platform gear during startup registration, or a platform-plane maintenance job | Owning tenant, within its own scope |
| Admit content revision | Platform authority | Owning tenant |
| Delete | Platform authority | Owning tenant |
| Exact resolve, batch resolve | Any tenant | Owner subtree only |
| Discovery, search, query assistance | Any tenant | Owner subtree only |
| Derive a new type from it | Any tenant that can see it; the derived type is owned by the deriving tenant | Any tenant in the owner subtree; same rule |
| Publish a Version Successor | Platform authority | Owning tenant only, never a descendant |

**The platform plane has no human actor.** Its callers are gears and maintenance jobs, authenticated as workloads; a person acts on it only by invoking a job, never by holding a credential for it. That is why the Register row above names a gear and a job rather than a role: there is no platform counterpart to the Tenant Administrator, and inventing one would put a human credential on the surface that can author global contracts and read across every tenant. The deliberateness that ADR-0013 requires of a purge lives in the decision to run the job, not in a person calling an endpoint.

Two further properties of the plane follow from the matrix rather than being added to it.

**It reads across every tenant, unfiltered by visibility.** There is no requesting tenant, so the descendant relation has no left-hand side and nothing to evaluate; authorization still applies through the PDP, and only the tenancy relation drops out. This is not a convenience: the purge dry run of ADR-0013 reports what would be released broken down by owner across tenant boundaries, and the operator path this ADR promises for enumerating a blocked deletion's dependents exists precisely to see entities the deleting tenant cannot. Consequently the non-disclosure rule above — that an out-of-scope entity is indistinguishable from a missing one — is a property of the tenant surface and does not hold here. Ownership disclosure does **not** widen with it: an entity read carries no owning tenant identifier on either plane, and the one operation that must name owners, the purge dry run of ADR-0013, carries them in its own report rather than through the entity model.

**It cannot create a tenant-owned entity.** Ownership is derived from the requesting context and is never request data; a platform-plane request has no tenant context, so there is nothing an owner could be derived from. This is a consequence of the rule rather than a separate prohibition, and it leaves purge as the only cross-tenant mutation — destructive maintenance under an operator rather than authoring.

That second property has a corollary this ADR does not resolve. Ordinary deletion belongs to the owning tenant, and ADR-0013 requires an entity to be `DELETED` before it can be purged. When a tenant is removed from the platform, its entities therefore have no one left who can delete them and no operation by which the platform can: the owner is gone and the platform plane cannot author in its place. Tenant offboarding is out of scope here for the same reason relocation is, and it is recorded as an open question rather than left to be discovered — it also has to decide what becomes of dependents in other tenants that reference the departing tenant's contracts.

Startup registration by platform gears is a platform-plane operation and authenticates as such rather than carrying a tenant `SecurityContext`. A tenant-plane registration derives the owning tenant from the request's `SecurityContext`, and the resulting entity is tenant-owned. **Ownership is never request data.** A tenant-plane request body that carries an owner is rejected rather than honoured, and there is no payload field by which a caller selects `global`.

Three of this ADR's own rules depend on that. "Within its own scope" in the Register row is a definition rather than a check only while the owner *is* the requesting tenant; if the caller states it, the row becomes a comparison that something has to enforce. §Ownership is immutable already assumes it, since "the owner of a candidate is derived from the requesting context, so correcting it means submitting again" is meaningless if the caller supplies the owner. And `entity.owner_tenant_id` is the column SecureORM scopes every read on, so accepting it from a request body would populate a security-scoping column from caller-controlled input.

"Within its own scope" in the Register row bounds the *ownership* of the result, not the region of the namespace a tenant may name. Those are different limits, and leaving the second unstated would make the global identifier namespace first-come-first-served: a tenant could occupy `gts.<other-vendor>.<package>...` merely by registering it before that vendor did. Authority over a region of the namespace is therefore a **grant**, evaluated by the platform PDP against the candidate's canonical GTS Identifier, and specified by `cpt-cf-types-registry-fr-registration-authority`. Two consequences belong here because they interact with this ADR's own rules:

* the grant check **precedes** the identifier-availability check. This ADR deliberately permits a registration conflict to disclose that a name is unavailable; evaluating availability first would extend that disclosure to callers with no authority to register there at all, turning a bounded leak into an enumeration primitive. An unauthorized caller receives one response whether the identifier is free, held by a visible entity, held by an invisible one, or held by a tombstone or a Source Claim reservation;
* the grant governs writing, never reading. Visibility remains the directed descendant relation above, so a subject may hold a grant over a region it cannot fully see, and may see contracts it holds no grant to modify. This is the same separation the matrix already asserts, applied to the namespace rather than to the individual entity.

### Consequences

* Ownership scope is a stored, indexed property of every Managed Entity, and every read path filters on the directed descendant relation. The tenant hierarchy therefore becomes a read-path dependency of Types Registry, not only of authorization.
* The family record introduced by ADR-0008 carries the family's ownership scope, so the successor-ownership rule is a lookup and a constraint rather than a convention.
* A tenant can build on ancestor contracts without any ability to alter them, which is the property that makes a shared contract safe to publish downward.
* A blocked deletion can require platform-operator involvement. This is an accepted operational cost of not disclosing dependents across the boundary.
* Identifier existence is observable at registration time to any caller willing to attempt a registration. This is bounded to the name and is documented rather than hidden.
* Moving a tenant in the hierarchy changes the visible audience of every entity it owns and of every entity visible to it. That is a platform-level operation whose interaction with Types Registry needs its own analysis; it is out of scope here.

### Confirmation

This decision is confirmed when:

* a tenant-owned entity resolves and appears in discovery for its owner and every descendant, and is absent — indistinguishably from never having existed — for ancestors, siblings, and unrelated tenants;
* a descendant can derive a new type from an ancestor-owned type, owns the resulting family, and is rejected when it attempts to publish a successor in the ancestor's family;
* a global entity is visible to every tenant and modifiable by none of them;
* registering an identifier held by an entity invisible to the caller returns the same conflict response as one held by a visible entity, and neither reveals owner, scope, content, or status;
* reverse resolution of a Registry Reference for an out-of-scope entity is indistinguishable from reverse resolution of an unissued reference, a batch read reports it `not_found` exactly as it reports an identifier that was never registered, and a discovery page omits it;
* no tenant-plane read result carries an owning tenant identifier, and a tenant caller can determine only whether the entity is its own; discovery accepts a scope selector and rejects a supplied tenant identifier;
* a platform-plane read returns entities owned by every tenant, without disclosing which tenant owns any of them, evaluates a Tenant Availability verdict only when a Context Tenant is named explicitly, and cannot create a tenant-owned entity under any grant;
* an externally managed entity is visible to the subtree of the tenant its source names, a plugin response omitting the ownership assertion or naming an unknown tenant is rejected as an invalid source response, and the assertion grants no write authority because no write path to an external entity exists;
* deletion blocked by a dependent invisible to the caller reports a count and no identifying information, and a platform-authority path can enumerate the dependents;
* the owner of an entity admitted on the tenant plane equals the requesting tenant of its `SecurityContext`, and a request body carrying an owner is rejected rather than honoured;
* no operation changes the ownership scope of an admitted entity, and a mis-assigned owner is repaired only by delete, purge, and re-registration;
* every row of the authorization matrix is covered by a test that also asserts the negative case;
* a tenant holding a grant over one vendor prefix can register inside it and is refused outside it, and a tenant-plane request cannot create a global entity under any grant;
* an unauthorized caller cannot distinguish a free identifier from one held by a visible entity, an invisible entity, a tombstone, or a Source Claim reservation, proving the grant check runs before the availability check.

## Pros and Cons of the Options

### Directed subtree visibility

* Good, because one published contract serves a whole subtree, which is what global identifier uniqueness forces and what a reseller topology needs.
* Good, because it runs opposite to tenant-tree barriers and therefore does not weaken them.
* Bad, because every read path acquires a hierarchy traversal, and the tenant hierarchy becomes a correctness dependency of resolution.
* Bad, because an ancestor's deletion can be blocked by dependents it cannot see.

### Strict isolation

* Good, because visibility is trivially decidable and no hierarchy lookup enters the read path.
* Good, because no cross-boundary disclosure question arises at all.
* Bad, because it cannot express a shared contract. Global identifier uniqueness means the same type cannot be registered per tenant, so a reseller would have to invent a distinct identifier per sub-tenant and the platform would treat semantically identical types as unrelated.
* Bad, because platform types would be the only shareable contracts, pushing every multi-tenant contract toward global scope and defeating tenant ownership.

### Full-tree visibility

* Good, because ancestors could administer the contracts of their subtree directly.
* Bad, because it exposes descendant-invented contracts upward, leaking business structure that the tenant tree exists to compartmentalize.
* Bad, because it conflicts with tenant-tree barriers rather than running orthogonally to them.

## More Information

### Industry Practice

* [Microsoft Dataverse](https://learn.microsoft.com/en-us/power-apps/developer/data-platform/introduction-entities) separates system (platform-owned) from custom (tenant-authored) metadata, with distinct authority over each — the same split as global versus tenant-owned here.
* [Kubernetes RBAC](https://kubernetes.io/docs/reference/access-authn-authz/rbac/) distinguishes cluster-scoped from namespaced resources, and cluster-scoped definitions such as CRDs are visible to all namespaces while being modifiable only with cluster authority.
* [AWS Organizations](https://docs.aws.amazon.com/organizations/latest/userguide/orgs_manage_policies.html) applies policy downward through an organizational hierarchy, with the parent's declarations binding descendants and no reverse flow — the same directionality adopted here.
* [Amazon S3 general purpose buckets](https://docs.aws.amazon.com/AmazonS3/latest/userguide/bucketnamingrules.html) are the closest large-scale precedent for a name space shared across tenant boundaries: they "exist in a global namespace, which means that each bucket name must be unique across all AWS accounts in all the AWS Regions within a partition", and a name taken by one account is unavailable to every other. AWS accepts the same one-bit disclosure this ADR accepts, and without the grant-ordering rule that narrows it here.

  Two details make the comparison more useful than a coincidence of shape. On releasing a name, AWS says a deleted bucket's name "might become available again in the global namespace for anyone to re-create", but "might not become available immediately, and in some cases might not become available again at all" — an unresolved hazard stated as vagueness. Types Registry faces the same hazard in a sharper form, because a deterministic Registry Reference reproduces itself when a name is reused, and answers it explicitly instead: an identifier is never rebound, the tombstone is permanent, and ADR-0013's purge is the single named exception, disabled by default. And AWS is under visible counter-pressure — general purpose buckets have acquired an account-scoped naming form, and directory buckets never used the global namespace — which is a reminder that a global namespace is a cost accepted for a reason rather than a free property. Here the reason is that GTS identifiers are meaningful across vendors by design, and the vendor-structured prefix plus the grant model of `cpt-cf-types-registry-fr-registration-authority` is what keeps the space governed rather than first-come-first-served.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0004**: [0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md](./0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md)
- **ADR-0008**: [0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md)
- **ADR-0010**: [0010-cpt-cf-types-registry-adr-tenant-availability-evaluation.md](./0010-cpt-cf-types-registry-adr-tenant-availability-evaluation.md)
- **ADR-0013**: [0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md](./0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md) — decides the purge that releases an identifier, which is what makes delete, purge, re-register the repair for a mis-assigned owner and lets ownership stay immutable.
- **Design note**: [../design-notes/lifecycle-and-tenant-state-model.md](../design-notes/lifecycle-and-tenant-state-model.md)

This decision directly addresses:

* `cpt-cf-types-registry-fr-tenant-ownership` - defines the scopes, the directed visibility relation, and the separation of visibility from management authority.
* `cpt-cf-types-registry-fr-lifecycle` - supplies the authority model for admission, revision, and deletion, and resolves the blocked-deletion disclosure question.
* `cpt-cf-types-registry-fr-id-resolution` - requires out-of-scope reverse resolution to be indistinguishable from an unissued reference.
* `cpt-cf-types-registry-fr-register-schemas`, `cpt-cf-types-registry-fr-register-instances` - bound what a registration conflict may reveal.
* `cpt-cf-types-registry-contract-platform-auth` - grounds the authorization matrix in the platform AuthN/AuthZ contract.
