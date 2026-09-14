---
status: accepted
date: 2026-07-26
decision-makers: Constructor Fabric Steering Committee
---

# Tenant Ownership, Visibility, and Management Authority

**ID**: `cpt-cf-types-registry-adr-tenant-ownership-visibility-authority`

## Table of Contents

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
* Visibility must not imply authority: seeing a contract is not permission to change it, and holding authority over a region is not permission to see what it holds.
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

An Externally Managed Entity carries the same two scopes, asserted by its Registry Source Plugin rather than stored here. Responsibilities split along what each side knows:

* the source states a **flat** fact: the entity is platform-wide or belongs to tenant X;
* Types Registry expands that fact into the directed descendant relation below.

ADR-0002 already gives the plugin a tenant identity so it can return enablement state. The plugin need not know the tenant hierarchy.

The scope is mandatory in a plugin response, so there is no default to get wrong in either direction; an absent assertion, or one naming a tenant the platform does not know, is an `INVALID_SOURCE_RESPONSE` rather than a silently invisible or silently universal entity.

Letting a source assert an owner does not offend `cpt-cf-types-registry-principle-local-authority`, though it looks as if it might. A plugin asserts only about content it already owns in full, so a wrong assertion discloses the vendor's own type to the wrong tenant — a vendor error about vendor data, not a breach of platform isolation, and no third party's data is reachable through it. What the platform does not delegate is the relation: visibility, authorization, and the availability verdict are computed here from what the source returns.

The scope is also **narrower in meaning** than its managed counterpart, and the shape being identical makes saying so necessary. Managed ownership does four jobs — it drives visibility, scopes SecureORM, confers write authority, and answers the "mine" discovery filter. For an externally managed entity only the first and the last apply: nothing is stored, and no write path to an external entity exists or is planned, so the assertion confers no authority over anything.

### Visibility is directed down the tenant tree

A tenant-owned entity is visible to its owning tenant and to every descendant of that tenant. It is not visible to ancestors, siblings, or unrelated tenants. The relation Types Registry evaluates is `is_descendant(requesting_tenant, owner_tenant)`, inclusive of the owner itself.

Tenant-tree barriers are not weakened by this rule. Barriers protect descendant **data** from ancestor access; contract visibility flows in the opposite direction, from ancestor to descendant. Types Registry therefore does not apply barrier filtering to the descendant-reads-ancestor relation. Authorization still governs whether a given caller may perform a given operation.

### Version-family ownership versus derivation

Two relations must not be confused.

**Version succession stays with the family.** Every member of a version family has the same ownership scope as the family root: `owner_scope(version_successor) == owner_scope(version_family_root)`. A descendant that uses an ancestor's `Customer.v1~` cannot publish `Customer.v2~`. Only the family owner extends its own family, which is what makes a published contract a contract rather than a shared mutable name.

**Derivation creates a new family with its own owner.** Where a tenant may derive at all, the derived type starts a new version family owned by the deriving tenant, which controls its successors and gains no authority over the base or the base's family.

**Visibility permits derivation; it does not imply it.** A tenant may derive only from a visible base whose GTS Identifier Region permits both:

* the vendor named by the candidate;
* tenant ownership of the result.

`cpt-cf-types-registry-fr-registration-policy` decides both per region and defaults closed. Visibility therefore makes a base *eligible* for derivation, while region policy grants extensibility.

The reseller case remains: one entry onboarding a vendor permits a tenant to derive beneath its own namespace. A platform contract, however, is no longer extensible by every tenant that can see it.

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

Only one bit crosses the boundary: *this identifier is unavailable*. `cpt-cf-types-registry-fr-registration-authority` narrows even that by requiring the grant check **before** identifier availability.

A caller without a covering grant receives the same response whether the name is:

* free;
* held by a visible or invisible entity;
* reserved by a tombstone or Source Claim.

Only a subject already authorized for that namespace region — normally its vendor — can observe availability. This is narrower than public registries where any API caller can probe names.

Per-tenant namespaces are incompatible with three existing decisions:

* `A~B~` would denote different types by tenant context, and every `$ref` would need that context to resolve;
* ADR-0001 deterministically derives one Registry Reference from one identifier. Including the tenant would destroy its portability;
* a descendant deriving from an ancestor's contract requires both to share one namespace.

### The contract exposes whether the caller owns an entity, not who does

Ownership is stored, asserted, and evaluated; it is not returned as a tenant identifier. A read result carries a boolean — the Context Tenant is the owner, or it is not — and no `owner_tenant_id`. On the platform plane, where there is no Context Tenant unless one is named, even the boolean is absent.

An owning tenant identifier is not exposed because:

* it is not actionable — Types Registry offers no tenant-to-tenant request path;
* it reveals an ancestor's identity and, cumulatively, the hierarchy above the caller, which §*Visibility is directed down the tenant tree* deliberately keeps flowing in one direction;
* for an external entity, it also reveals the vendor's tenant mapping.

The first reason is decisive: the value answers no question its holder can act on.

The boolean is deliberately not named for authority. Owning an entity is necessary for managing it and not sufficient: `cpt-cf-types-registry-fr-registration-authority` also requires a grant covering the candidate identifier. A field called "manageable" would promise more than it knows. The composite question — may I change this — is answered by the caller from the boolean and the entity's origin, since no write path reaches an externally managed entity however its source assigns it.

The **global versus tenant-owned** distinction is not exposed either; it was considered and rejected — see *Sub-choices within the selected option*, below.

A read may name a **Context Tenant**, the tenant scope root of an operation, which may differ from the subject's tenant. The two identities govern different checks:

| Check | Evaluated for |
|---|---|
| Visibility | Requesting subject |
| Availability | Context Tenant |

Their visible sets are not nested. Evaluating visibility for the Context Tenant would let an ancestor read a descendant's private contracts merely by naming that descendant. The platform therefore authorizes the claim that the subject's tenant is an ancestor of the Context Tenant — DESIGN discharges it through the PDP-authorized ancestor call rather than through the read grant — but visibility remains subject-based.

The same rule governs the filter. Discovery selects by scope — mine, or everything visible — and never by a supplied tenant identifier. Accepting one would reopen on the read surface what `cpt-cf-types-registry-fr-registration-authority` closes on the write surface, where ownership is derived from the `SecurityContext` and never accepted as request data: a caller could otherwise probe for its own ancestors by filtering on a guessed identifier and observing whether the result is empty.

### Deletion blocked by an invisible dependent

An ancestor may be unable to delete its own contract because a descendant registered a dependent on it that the ancestor cannot see.

This is correct behaviour, not a defect. The ancestor published a contract into its subtree and something took a dependency on it; unilateral deletion would break that dependent whether or not the ancestor can see it. The registry is enforcing the contract, not obstructing the owner.

The disclosure boundary still holds. A blocked tenant-plane deletion reports only the dependent count — no identities, owners, or content.

The owner may therefore be unable to resolve the block alone. Resolution requires either the dependent owners to remove them or platform authority to inspect across the boundary. A platform-plane Dry Run deletion runs the same check and may enumerate those dependents; there is no separate enumeration API.

The count lets the tenant distinguish “blocked” from a general failure without widening disclosure.

The same restraint governs **Dry Run diagnostics**, and it has to be said explicitly because the temptation runs the other way. A Dry Run exists to tell a caller precisely what would go wrong, so its natural instinct is to name the offending dependent — but a tenant-plane Dry Run refused by a dependent the caller cannot see must report a count and no identity, exactly as the real deletion does. A rehearsal that discloses more than the act it rehearses would make the disclosure boundary optional.

### Ownership is immutable, and a mistake is repaired by purge

Ownership scope is fixed when an entity is admitted and never changes afterwards. There is no ownership correction operation.

The argument for providing one runs: a mistaken owner cannot be repaired by deleting and re-registering, because ADR-0001 reserves a deleted identifier permanently, so the identifier would be stranded and an immutable owner field would make the mistake permanent. ADR-0013 removes that premise. Purge releases the identifier, so the repair is delete, purge, re-register under the correct owner, and nothing is stranded.

Before admission the question does not arise at all: the owner of a candidate is derived from the requesting context, so correcting it means submitting again.

For an admitted entity the purge route is not merely an equivalent — it is the more honest one. Changing an owner changes the visible audience of a contract, so a correction would have to establish which registered dependents would lose sight of the entity and which entities the new owner would lose sight of, and then reject or migrate them. That is a migration wearing the word "correction". Purge and re-registration make the same disruption explicit: dependents break visibly and are re-registered, rather than being silently migrated under an operation whose name suggests a repair.

The cost is that purge is disabled by default and expected to stay disabled in production, so a mis-owned production entity cannot be repaired in place while it is. ADR-0013 already contemplates enabling purge there for a specific planned migration, which is exactly what this is. The alternative — a correction operation available in production — would be a way to change who can see a contract without anyone re-approving it.

Two consequences follow for the rest of this ADR. `entity.ownership_scope` and the ownership recorded on a version family are write-once, so the admission path that locks the family row compares them and never updates them. And the question of whether correction between global and tenant ownership is permitted, and whether a target owner must accept it, dissolves rather than being deferred.

### Authorization matrix

Visibility never implies authority. The matrix below is the P1 baseline; concrete permission names belong to DESIGN.

| Operation | Global entity | Tenant-owned entity |
|---|---|---|
| Register | Platform authority — a platform gear during startup registration, or a platform-plane maintenance job | Owning tenant, within its own scope |
| Admit content revision | Platform authority | Owning tenant |
| Delete | Platform authority | Owning tenant |
| Exact resolve, batch resolve | Any tenant holding a read grant | Owner subtree only, holding a read grant |
| Discovery, search, query assistance | Any tenant holding a list grant | Owner subtree only, holding a list grant |
| Derive a new type from it | Any tenant that can see it, where registration policy admits the candidate and a covering PDP grant authorizes it; the derived type is owned by the deriving tenant | Any tenant in the owner subtree, subject to the same policy and grant checks |
| Publish a Version Successor | Platform authority | Owning tenant only, never a descendant |

Every tenant-plane row requires a grant, the read rows included. What those rows state is which entities an authorized operation may reach, not that it needs no authorization: `read` and `list` are actions of their own, and the visible set is the ceiling a grant is intersected with rather than a substitute for one. A release ships baseline read grants, so the default audience of a contract remains its visible subtree; what the two actions add is the ability to narrow that audience per subject — closing the catalogue to a third-party token, or to a role restricted inside its own tenant — which visibility cannot express, being a property of the tenant rather than of the subject. P1 narrows per subject only; a read grant carries no region, unlike a write grant.

**The platform plane has no human actor.** Gears and maintenance jobs authenticate as workloads. A person may trigger a job but never holds a platform-plane credential.

The Register row therefore names gears and jobs, not a role. Introducing a platform equivalent of Tenant Administrator would expose a human credential able to author global contracts and read every tenant. ADR-0013's deliberate purge act is the decision to run the job, not a person calling an endpoint.

Two further properties of the plane follow from the matrix rather than being added to it.

**The platform plane reads across every tenant without visibility filtering.** With no requesting tenant, the descendant relation has no left-hand side. The tenant PDP is not substituted for it.

Under `cpt-cf-adr-two-plane-auth`:

* `PlatformSecurityContext` never reaches the tenant `PolicyEnforcer`;
* the authenticated workload identity authorizes a platform handler;
* any narrowing is workload policy, not a subject grant.

The matrix above is therefore tenant-plane policy; the platform plane authorizes the plane itself. This is required for ADR-0013 purge reports grouped by owner and for enumerating dependents invisible to a deleting tenant.

Tenant non-disclosure does not apply on this plane, but ownership disclosure still does not widen. Entity reads expose no owner tenant on either plane; purge names owners only in its job-specific report.

**It cannot create a tenant-owned entity.** Ownership is derived from the requesting context and is never request data; a platform-plane request has no tenant context, so there is nothing an owner could be derived from. This is a consequence of the rule rather than a separate prohibition, and it leaves purge as the only cross-tenant mutation — destructive maintenance under an operator rather than authoring.

This leaves one unresolved corollary. Ordinary deletion belongs to the owner, while ADR-0013 requires `DELETED` before purge. After tenant removal, no owner remains to delete its entities, and the platform plane cannot author on its behalf.

Tenant offboarding is therefore an open question, like tenant relocation. It must also decide what happens to cross-tenant dependents of the departing tenant's contracts.

Startup registration by platform gears is a platform-plane operation and authenticates as such rather than carrying a tenant `SecurityContext`. A tenant-plane registration derives the owning tenant from the request's `SecurityContext`, and the resulting entity is tenant-owned. **Ownership is never request data.** A tenant-plane request body that carries an owner is rejected rather than honoured, and there is no payload field by which a caller selects `global`.

Three rules depend on ownership never being request data:

* “Within its own scope” remains a definition because the requesting tenant *is* the owner, not a caller-supplied value to compare.
* §*Ownership is immutable, and a mistake is repaired by purge* assumes the candidate owner is derived from the request context; otherwise correcting a candidate by resubmitting it from the correct context would be meaningless.
* `entity.owner_tenant_id`, which SecureORM uses for read scoping, never comes from caller-controlled payload.

"Within its own scope" in the Register row bounds the *ownership* of the result, not the region of the namespace a tenant may name. Those are different limits, and leaving the second unstated would make the global identifier namespace first-come-first-served: a tenant could occupy `gts.<other-vendor>.<package>...` merely by registering it before that vendor did. Authority over a region of the namespace is therefore a **grant**, evaluated by the platform PDP against the candidate's canonical GTS Identifier, and specified by `cpt-cf-types-registry-fr-registration-authority`. Two consequences belong here because they interact with this ADR's own rules:

* the grant check **precedes** the identifier-availability check. This ADR deliberately permits a registration conflict to disclose that a name is unavailable; evaluating availability first would extend that disclosure to callers with no authority to register there at all, turning a bounded leak into an enumeration primitive. An unauthorized caller receives one response whether the identifier is free, held by a visible entity, held by an invisible one, or held by a tombstone or a Source Claim reservation;
* a grant authorizes an operation and never widens what that operation may see. Reading is authorized as its own action, so a caller needs a read grant — gear-wide in P1, where only write grants carry a region — while visibility remains the directed descendant relation above: the grant decides whether the operation runs, the relation bounds what it may return, and neither substitutes for the other. A subject may therefore hold a write grant over a region it cannot fully see, see contracts it holds no grant to modify, and be refused a catalogue it can see. This is the same separation the matrix already asserts, applied to the namespace rather than to the individual entity;
* **a grant creates an entity only where registration policy already admits the candidate,** and what it admits is configuration rather than a constant. `cpt-cf-types-registry-fr-registration-policy` decides per GTS Identifier Region whether candidates there may be tenant-owned and which vendors they may name, both closed by default, and it is evaluated from the candidate's identifier and plane before the PDP is consulted. This is a bound on what a grant can reach rather than a grant of its own, so the two never disagree: a region the policy closes cannot be opened by any grant, and a grant is never asked about a candidate the policy has already refused. The bound applies where ownership comes into being — a candidate creating a new logical entity — not to a revision or deletion of one already admitted, so closing a region stops the next entity rather than freezing what it admitted. Withdrawing ongoing write authority stays the grant's job, the revocable half of the pair.

  The bound exists because of §*Ownership is immutable, and a mistake is repaired by purge*. The requested owner must equal the family's stored owner, so revision cannot repair a platform contract admitted as tenant-owned. Repair requires delete and ADR-0013 purge, the destructive operation guarded by deployment policy.

  The default is therefore closed, not merely narrow. A missing entry refuses registration immediately; an over-broad entry may be discovered only after an entity has an owner that cannot change. Opening a region is a deliberate act with a named beneficiary. The offboarding problem above cannot arise where tenant ownership was never admitted.

  Under the shipped declarations that includes `gts.cf.toolkit.*`, where GTS §3.6 has the pattern cover the whole derivation chain beneath the prefix, so a type derived from a platform base type and an Instance of either are inside it rather than beside it.

### Consequences

* Ownership scope is a stored, indexed property of every Managed Entity, and every read path filters on the directed descendant relation. The tenant hierarchy therefore becomes a read-path dependency of Types Registry, not only of authorization.
* Reads acquire a policy dependency alongside the hierarchy one: an unreachable PDP fails a read closed exactly as it fails a registration. The cost is that a deployment must ship the baseline read grants with the release, because a tenant subject holding none cannot resolve even a platform contract.
* The family record introduced by ADR-0008 carries the family's ownership scope, so the successor-ownership rule is a lookup and a constraint rather than a convention.
* The GTS namespace is closed to tenant ownership and to cross-vendor naming until a region is opened, so onboarding a vendor is a deliberate configuration act rather than a consequence of that vendor's first registration. The cost is that a deployment must declare a region before the vendors it serves can register in it, and a missing declaration surfaces as a refused registration.
* A platform contract is extensible per region rather than uniformly. Two deployments of the same release can differ in which of the platform's own types third parties may derive from, which is deliberate — it is the XaaS operator's decision — and it means a derived type admitted in one deployment may be refused in another.
* A tenant can build on ancestor contracts without any ability to alter them, which is the property that makes a shared contract safe to publish downward.
* A blocked deletion can require platform-operator involvement. This is an accepted operational cost of not disclosing dependents across the boundary.
* Identifier existence is observable at registration time only after registration policy admits the candidate and the caller presents a covering grant. It is bounded to the name; unauthorized callers cannot distinguish free and occupied identifiers.
* Moving a tenant in the hierarchy changes the visible audience of every entity it owns and of every entity visible to it. That is a platform-level operation whose interaction with Types Registry needs its own analysis; it is out of scope here.

### Confirmation

This decision is confirmed when:

* a tenant-owned entity resolves and appears in discovery for its owner and every descendant, and is absent — indistinguishably from never having existed — for ancestors, siblings, and unrelated tenants;
* a descendant whose candidate is admitted by registration policy and authorized by a covering grant can derive a new type from an ancestor-owned type, owns the resulting family, and is rejected when it attempts to publish a successor in the ancestor's family;
* a candidate deriving from a base whose region fails to admit *either* its vendor *or* tenant ownership — each parameter tested on its own — is refused before the PDP is consulted, with a reason naming configuration rather than authorization, and the same candidate is admitted once that region is opened;
* a tenant deriving beneath its own vendor namespace is admitted by the single entry that onboards that vendor, with no entry per type;
* closing a region after it admitted an entity leaves that entity's owner able to revise and to delete it, so nothing is frozen by a configuration change and the purge repair stays reachable;
* a global entity is visible to every tenant and modifiable by none of them;
* registering an identifier held by an entity invisible to the caller returns the same conflict response as one held by a visible entity, and neither reveals owner, scope, content, or status;
* reverse resolution of a Registry Reference for an out-of-scope entity is indistinguishable from reverse resolution of an unissued reference, a batch read reports it `not_found` exactly as it reports an identifier that was never registered, and a discovery page omits it;
* no tenant-plane read result carries an owning tenant identifier, and a tenant caller can determine only whether the entity is its own; discovery accepts a scope selector and rejects a supplied tenant identifier;
* a platform-plane read returns entities owned by every tenant, without disclosing which tenant owns any of them, evaluates a Tenant Availability verdict only when a Context Tenant is named explicitly, and cannot create a tenant-owned entity under any grant;
* an externally managed entity is visible to the subtree of the tenant its source names, a plugin response omitting the ownership assertion or naming an unknown tenant is rejected as an invalid source response, and the assertion grants no write authority because no write path to an external entity exists;
* deletion blocked by a dependent invisible to the caller reports a count and no identifying information, and a platform-authority path can enumerate the dependents;
* the owner of an entity admitted on the tenant plane equals the requesting tenant of its `SecurityContext`, and a request body carrying an owner is rejected rather than honoured;
* no operation changes the ownership scope of an admitted entity, and a mis-assigned owner is repaired only by delete, purge, and re-registration;
* a caller holding no read grant is refused, indistinguishably for a free, visible, invisible, or reserved identifier, and a caller holding one receives exactly its subject tenant's visible set — naming a descendant as Context Tenant changes the availability verdict and never the visible set;
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

### Sub-choices within the selected option

Alternatives considered while shaping the option above, recorded here rather than in *Decision Outcome*, which states what was chosen.

Strict isolation was rejected because it cannot express the case the platform actually has. A reseller tenant that publishes a contract for its sub-tenants would have to register a separate entity per sub-tenant, and global identifier uniqueness makes that impossible: the same contract cannot exist twice, so each sub-tenant would need a different identifier for what is semantically one type. Every downstream mechanism — derivation chains, Concrete Reference Sets, compatibility — would then treat them as unrelated types.

Full-tree visibility was rejected because it inverts the direction that matters. An ancestor gains nothing from seeing contracts its descendants invented, and exposing them upward leaks the descendant's business structure.

**Where the extensibility of a region is decided** had three candidates once a stakeholder asked that a vendor be able to close its contracts to other vendors' derivations.

A **PDP grant** is the natural home for write authority: it already matches GTS patterns and is operator-owned and audited. It cannot be the only mechanism because the platform plane authorizes a workload, not a subject, and every gear in one process shares that identity. A rule keyed on *which vendor is registering* is therefore unenforceable there.

Both planes can inspect the vendor named by the candidate identifier; that is a request property, not a grant. The division is:

* grants decide *who* may write in a region;
* registration policy decides *what the region admits*.

A **property in the authored GTS document** — the registrant marking its own type open or closed — was rejected because the deciding actor is wrong, and because such a property travels with content the registrant supplies. The requirement is that a platform operator, not the gear developer who published the type, decides which vendors may extend it. The release still ships the entries that make a stock deployment correct, but they are the platform's own defaults rather than a per-type authority: an operator may open any region, and no entry a type's author writes constrains that.

**A constant in code** was the prior design: one hard-coded `gts.cf.toolkit.*` reservation. Review found it covered only two identifiers and left the rest of the `cf` platform space outside the bound.

Widening the prefix would also cover vendor derivations beneath platform base types, closing the extension path this ADR preserves. A closed-default per-region table expresses both cases. The platform vendor identity remains one build-time value rather than a literal repeated at each check.

The **global versus tenant-owned** distinction is not exposed either. It was considered: it discloses nothing about the hierarchy and would let a management view group a catalogue into platform contracts, inherited contracts, and one's own. It is left out because no requirement or actor names that grouping, and the discovery filter is narrowed to match rather than left able to select a bucket the response cannot explain. Adding both back later is additive.

## More Information

### Industry Practice

* [Microsoft Dataverse](https://learn.microsoft.com/en-us/power-apps/developer/data-platform/introduction-entities) separates system (platform-owned) from custom (tenant-authored) metadata, with distinct authority over each — the same split as global versus tenant-owned here.
* [Kubernetes RBAC](https://kubernetes.io/docs/reference/access-authn-authz/rbac/) distinguishes cluster-scoped from namespaced resources, and cluster-scoped definitions such as CRDs are visible to all namespaces while being modifiable only with cluster authority.
* [AWS Organizations](https://docs.aws.amazon.com/organizations/latest/userguide/orgs_manage_policies.html) applies policy downward through an organizational hierarchy, with the parent's declarations binding descendants and no reverse flow — the same directionality adopted here.
* [Amazon S3 general purpose buckets](https://docs.aws.amazon.com/AmazonS3/latest/userguide/bucketnamingrules.html) are the closest large-scale precedent for a name space shared across tenant boundaries: they "exist in a global namespace, which means that each bucket name must be unique across all AWS accounts in all the AWS Regions within a partition", and a name taken by one account is unavailable to every other. AWS accepts the same one-bit disclosure this ADR accepts, and without the grant-ordering rule that narrows it here.

  The comparison adds two useful cautions. First, AWS describes name reuse after deletion as uncertain. Types Registry faces a sharper hazard because deterministic Registry References reproduce on reuse, so it gives an explicit rule instead: tombstones reserve identifiers permanently, with disabled-by-default ADR-0013 purge as the single exception.

  Second, AWS has added account-scoped forms, while directory buckets never used its global namespace. A global namespace is a cost accepted for a reason, not a free property. Here the reason is cross-vendor GTS identity; vendor prefixes and `cpt-cf-types-registry-fr-registration-authority` grants keep that space governed rather than first-come-first-served.

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)
- **ADR-0001**: [0001-cpt-cf-types-registry-adr-storage-identity-query-model.md](./0001-cpt-cf-types-registry-adr-storage-identity-query-model.md)
- **ADR-0002**: [0002-cpt-cf-types-registry-adr-external-source-live-delegation.md](./0002-cpt-cf-types-registry-adr-external-source-live-delegation.md)
- **ADR-0004**: [0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md](./0004-cpt-cf-types-registry-adr-gts-minor-version-identity-evolution.md)
- **ADR-0008**: [0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md](./0008-cpt-cf-types-registry-adr-managed-version-family-lifecycle.md)
- **ADR-0010**: [0010-cpt-cf-types-registry-adr-tenant-availability-evaluation.md](./0010-cpt-cf-types-registry-adr-tenant-availability-evaluation.md)
- **ADR-0013**: [0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md](./0013-cpt-cf-types-registry-adr-platform-purge-of-deleted-entities.md) — decides the purge that releases an identifier, which is what makes delete, purge, re-register the repair for a mis-assigned owner and lets ownership stay immutable.

This decision directly addresses:

* `cpt-cf-types-registry-fr-tenant-ownership` - defines the scopes, the directed visibility relation, and the separation of visibility from management authority; its requirement that a visible entry be readable subject to authorization is what makes reads their own actions.
* `cpt-cf-types-registry-fr-lifecycle` - supplies the authority model for admission, revision, and deletion, and resolves the blocked-deletion disclosure question.
* `cpt-cf-types-registry-fr-id-resolution` - requires out-of-scope reverse resolution to be indistinguishable from an unissued reference.
* `cpt-cf-types-registry-fr-register-schemas`, `cpt-cf-types-registry-fr-register-instances` - bound what a registration conflict may reveal.
* `cpt-cf-types-registry-contract-platform-auth` - grounds the authorization matrix in the platform AuthN/AuthZ contract.
