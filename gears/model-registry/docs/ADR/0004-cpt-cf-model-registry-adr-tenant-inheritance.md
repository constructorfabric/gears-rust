---
status: accepted
date: 2026-02-18
---

<!-- Updated: 2026-08-06 by Constructor Tech -->

# Additive Tenant Inheritance with Provider Shadowing


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Additive Inheritance with Shadowing](#additive-inheritance-with-shadowing)
  - [Strict Inheritance (No Override)](#strict-inheritance-no-override)
  - [Explicit Copy (No Inheritance)](#explicit-copy-no-inheritance)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-tenant-inheritance`

## Context and Problem Statement

Model Registry operates in a multi-tenant hierarchy where child tenants inherit from parent tenants. How should providers and their models be inherited, at what granularity may a child tenant customize what it inherited, and who holds approval authority over an inherited model?

## Decision Drivers

* `cpt-cf-model-registry-fr-provider-management` — Provider CRUD with inheritance
* `cpt-cf-model-registry-fr-tenant-isolation` — Tenant-scoped operations
* `cpt-cf-model-registry-fr-manual-model-management` — model creation resolves `provider_id` in the caller's own tenant only
* PRD requirement: "Child tenant can only restrict, not expand parent permissions"
* PRD requirement: "Model creation is same-tenant only" — a model's `tenant_id` always equals its provider's `tenant_id`
* Compliance isolation — tenants may need to exclude certain providers
* Flexibility — different tenants have different vendor relationships

## Considered Options

* Additive Inheritance with Shadowing
* Strict Inheritance (no override)
* Explicit Copy (no inheritance)

## Decision Outcome

Chosen option: "Additive Inheritance with Shadowing", because it provides the flexibility needed for compliance isolation while maintaining the "restrict only" security model.

**Scope: the eval reads.** Additive visibility and shadowing govern `GET /v1/models` and `GET /v1/models/{canonical_id}` — the surface an inference caller uses. The admin endpoints under `/v1/admin/…` operate on the tenant set the `authz-resolver` PDP grants and perform no hierarchy walk. The two mechanisms are independent: how far an admin reaches is a policy decision, and it neither widens nor narrows what a tenant's eval reads return.

**Shadowing is keyed on the provider slug, and only on the provider slug.** A child tenant registers a provider under an inherited slug and thereby takes that slug over for itself and its descendants. Because a model always belongs to exactly one provider owned by the same tenant, the shadowed ancestor provider's **entire model set** leaves the child's subtree along with the provider record. There is no per-model shadow and no per-model override: a colliding `canonical_id` at a closer tenant is a *consequence* of that tenant owning its own provider of the same slug, never a mechanism of its own.

This holds regardless of the shadowing provider's `status`. An `active` shadow hides the ancestor's models and lets the child populate its own under the slug; a `disabled` shadow hides the ancestor's models and additionally blocks the child from creating or evaluating any of its own.

### Consequences

* Good, because a child tenant can exclude an inherited provider outright, for its whole subtree, with a single write (compliance isolation)
* Good, because inherited providers are available by default (convenience)
* Good, because there is exactly one override lever with one meaning — no second, finer-grained lever whose interaction with the first would have to be specified
* Good, because "restrict only" is structural: a child can hide an ancestor's models but can never attach to, re-approve, re-price, or otherwise mutate them
* Neutral, because the override is all-or-nothing per slug — a tenant that wants to keep most of an ancestor provider's catalog but drop one model must shadow the provider and re-create the models it wants to keep
* Bad, because resolution logic is more complex: whether a given model row is visible depends on the requester's whole ancestor chain, so it is not a property of the row and cannot be stored on it — it has to be recomputed per request
* Bad, because the resolution must fail closed. Shadowing is a compliance-isolation lever, so a read that cannot establish who wins a slug must be refused rather than answered: falling through to an ancestor on an unresolved closer tenant would serve exactly the models the shadow exists to hide
* Bad, because shadowing is silent from the child's point of view — models that resolved yesterday vanish the moment a provider with a colliding slug is registered. The management listing does not surface the hidden rows either: it is scoped by the PDP, and shadowing has no meaning without a reference chain, so an ancestor's shadowed models are visible only to a caller granted that ancestor's tenant

### Confirmation

* Unit tests verify inheritance resolution order (tenant → parent → ... → root, first match wins per slug)
* Unit tests verify a child provider with a colliding slug shadows the ancestor's provider **and hides every model attached to it**, for both `active` and `disabled` shadows
* Unit tests verify an ancestor model is excluded when its provider's slug is shadowed by a closer tenant **even though no colliding `canonical_id` exists there** — the case a `canonical_id`-keyed merge silently gets wrong
* Unit tests verify `create_model` writes the model into its resolved provider's tenant, and that a provider outside the caller's access scope is refused with `provider_not_found`
* Unit tests verify a disabled provider makes every model attached to it unavailable for eval, independent of each model's own approval status
* Integration tests verify compliance isolation end to end, and that the admin endpoints return exactly the rows the PDP scope covers while the eval listing still shadows

## Pros and Cons of the Options

### Additive Inheritance with Shadowing

Inheritance model:
- Child tenant sees: ancestors' providers + own providers
- Resolution order: tenant → parent → ... → root, first match wins, resolved **per slug**
- Shadowing: child registers a provider with the same slug; that provider wins the slug for the child and its descendants
- Model visibility follows the provider: a model is visible to a requester only when its own provider is the winner for its slug in that requester's chain
- Exclusion: shadow the slug. The ancestor's models leave the subtree whether the shadow is `active` or `disabled`; `disabled` additionally blocks the child's own models under that slug

Model ownership:
- A model's `tenant_id` always equals its provider's `tenant_id`
- A model is created in its provider's tenant; the provider is resolved within the caller's access scope
- There is no per-model shadow, per-model override, or per-model exclusion

Approval:
- A model's approval status is set by the tenant admin of the model's own tenant, which is always its provider's tenant
- Descendants inherit that status along with the model and hold **no** approval authority over it — a descendant cannot approve, reject, or revoke an ancestor's model, not even for its own scope
- The only lever a descendant has over an inherited provider's models is to shadow the provider, which removes the whole set. "Restrict only" is realized by that lever, not by a per-model veto

* Good, because flexibility for compliance isolation
* Good, because intuitive "inherit and customize" model
* Good, because consistent with PRD shadowing examples
* Good, because "restrict only" prevents privilege escalation
* Good, because one granularity (the provider slug) governs visibility, ownership, and approval authority alike — the three cannot drift apart
* Neutral, because requires clear documentation
* Bad, because resolution logic complexity
* Bad, because a tenant that wants to drop a single inherited model has to shadow its whole provider and rebuild the rest of the catalog

### Strict Inheritance (No Override)

Child inherits everything from parent without ability to override.

* Good, because simple and predictable
* Good, because no resolution ambiguity
* Bad, because no compliance isolation possible
* Bad, because child cannot exclude vendor they don't want
* Bad, because inflexible for enterprise scenarios

### Explicit Copy (No Inheritance)

No automatic inheritance; admin manually copies configuration.

* Good, because explicit control over everything
* Good, because no hidden inherited state
* Bad, because administrative burden
* Bad, because configuration drift between tenants
* Bad, because doesn't scale with tenant hierarchy depth

## More Information

Shadowing example from PRD:
```
Root tenant: azure-prod → platform Azure subscription (active), with model azure-prod::gpt-4o
Tenant A:    azure-prod → Tenant A's Azure subscription (active, shadows root) — starts empty
Tenant B:    (no override) → uses root's azure-prod

Request from Tenant B for azure-prod::gpt-4o → root's provider, root's model
Request from Tenant A for azure-prod::gpt-4o → not found
```

Root's model is hidden along with root's provider, and Tenant A's own `azure-prod` starts with no
models. The canonical ID resolves for Tenant A only once Tenant A creates or discovers its own
`gpt-4o` under its own provider — an independent row, with Tenant A's own approval status,
capabilities, and cost. It is never root's model reached through Tenant A's provider: a canonical
ID never resolves to a mix of one tenant's provider and another tenant's model.

Exclusion example:
```
Root tenant: openai → platform OpenAI account (active), with models
Tenant C:    openai → Tenant C's own provider row, status: disabled

Tenant C's subtree has no route to OpenAI at all (compliance requirement)
```

Note what does the work here: root's models leave Tenant C's subtree because of the **shadow**, not
because of the `disabled` status — an `active` shadow would hide them just the same. What `disabled`
adds is that Tenant C cannot create or evaluate any models of its own under the slug either, closing
the remaining route.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-model-registry-fr-provider-management` — Inheritance with shadowing support
* `cpt-cf-model-registry-fr-tenant-isolation` — Tenant-scoped resolution
* `cpt-cf-model-registry-fr-manual-model-management` — A model is created in its provider's tenant; approval authority belongs to that tenant
* `cpt-cf-model-registry-fr-get-tenant-model` — Shadow-aware single-model resolution
* `cpt-cf-model-registry-fr-list-tenant-models` — Shadow-aware additive merge across the ancestor chain
* `cpt-cf-model-registry-fr-list-tenant-models-management` — Shadowed rows retained, marked, and read-only for audit
* `cpt-cf-model-registry-principle-additive-inheritance` — Establishes inheritance model
* `cpt-cf-model-registry-usecase-register-provider` — Provider registration with shadowing
