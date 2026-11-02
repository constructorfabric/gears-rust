---
description: "Model Registry reads inherited catalog rows under a gear-built upward tenant scope, treats the PDP tenant constraint as a gate, respects barriers in the ancestor chain, and fails closed on subtree constraints."
status: accepted
date: 2026-11-02
---

Created:  2026-11-02 by Constructor Tech

# ADR-0001: Gear-Built Upward Read Scope, Tenant Gate, and Barrier-Respecting Inherited Catalog Rows

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The read scope is built by the gear from the ancestor chain](#the-read-scope-is-built-by-the-gear-from-the-ancestor-chain)
  - [The PDP tenant constraint is a gate on the acting tenant](#the-pdp-tenant-constraint-is-a-gate-on-the-acting-tenant)
  - [Barriers are respected for the ancestor chain](#barriers-are-respected-for-the-ancestor-chain)
  - [Subtree constraints fail closed](#subtree-constraints-fail-closed)
  - [One visibility predicate for every read path](#one-visibility-predicate-for-every-read-path)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Gear-built upward scope, tenant constraint as a gate, subtree constraints fail closed](#gear-built-upward-scope-tenant-constraint-as-a-gate-subtree-constraints-fail-closed)
  - [Own rows only](#own-rows-only)
  - [PDP tenant constraint as SQL clamp, inheritance by a second unscoped query](#pdp-tenant-constraint-as-sql-clamp-inheritance-by-a-second-unscoped-query)
  - [Declare `tenant_hierarchy` and project `tenant_closure`](#declare-tenant_hierarchy-and-project-tenant_closure)
  - [Per-row PDP decisions with the real owner tenant](#per-row-pdp-decisions-with-the-real-owner-tenant)
  - [Option 1 with `BarrierMode::Ignore` for the ancestor chain](#option-1-with-barriermodeignore-for-the-ancestor-chain)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-inherited-read-scope`

## Context and Problem Statement

A tenant's effective catalog is its own providers and models plus the `shared` providers and models of its ancestors, with shadowing per provider slug (`cpt-cf-model-registry-fr-effective-catalog`, `cpt-cf-model-registry-fr-provider-shadowing`). Every read path (resolution, the available list, management reads) must return rows owned by other tenants: the ancestors.

Two platform rules stand in the way, and one platform default shapes the answer:

1. [Secure ORM](../../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md): "Use `PolicyEnforcer` for all authorization decisions. Do not construct `AccessScope` manually in production code." The scope the PDP returns is a flat predicate on `owner_tenant_id`, pre-expanded for the acting tenant. Applied as a SQL clamp it keeps the tenant's own rows and drops every inherited row.
2. A PDP `InTenantSubtree` constraint carries `respect_barriers` and an optional `descendant_status` list. Secure ORM evaluates it against `tenant_closure`, a projection this gear does not have.
3. Tenant Resolver's default `BarrierMode::Respect` stops ancestor traversal at a self-managed tenant: a self-managed tenant's chain is empty, and the chain of a tenant below it ends at that tenant. `BarrierMode::Ignore` traverses through self-managed tenants.

CredStore met the same problem for inherited shared secrets and accepted [CredStore ADR-0005](../../../credstore/docs/ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md), which reads the chain with `BarrierMode::Ignore`. How should Model Registry read inherited rows, does inheritance cross isolation barriers, and what does the gear do with the PDP's tenant and subtree constraints?

## Decision Drivers

* **D1** — No `tenant_closure` projection, no Account Management co-location, no new PEP capability.
* **D2** — Inheritance respects isolation barriers: a self-managed tenant inherits nothing from above the barrier, and its subtree inherits from it.
* **D3** — Row visibility is decided in one place; a read path cannot widen it by omission.
* **D4** — Neither data nor authority crosses a barrier.
* **D5** — Fail closed on anything the gear cannot evaluate faithfully, including structured subtree predicates.
* **D6** — The resolution path stays within its latency budget: no Tenant Resolver call beyond the cached chain (`cpt-cf-model-registry-nfr-resolution-latency`).
* **D7** — Same scope and gate model as CredStore, so the inheriting gears answer the same policy the same way; barrier handling may be stricter.

## Considered Options

1. Gear-built upward scope, tenant constraint as a gate, subtree constraints fail closed
2. Own rows only
3. PDP tenant constraint as SQL clamp, inheritance by a second unscoped query
4. Declare `tenant_hierarchy` and project `tenant_closure`
5. Per-row PDP decisions with the real owner tenant
6. Option 1 with `BarrierMode::Ignore` for the ancestor chain

## Decision Outcome

Chosen option: "Gear-built upward scope, tenant constraint as a gate, subtree constraints fail closed", because it is the only option that gives a tenant its ancestors' shared rows up to the nearest isolation barrier without new platform machinery, keeps one authorization path for all reads, and matches CredStore ADR-0005 in everything but barrier handling (D1, D2, D3, D4, D5, D7).

### The read scope is built by the gear from the ancestor chain

Reads run through `SecureConn` under `AccessScope::for_tenants(chain)`, where `chain` is the acting tenant T0 followed by its ancestors. The scope is an upper bound only: it admits rows of the chain's tenants and nothing else, so a defect in a predicate cannot reach a sibling or a descendant row. The catalog visibility predicates are ANDed onto it: ancestor providers only when `sharing_mode = shared`; models only when their provider wins its slug and the model is own or `shared`. Writes use `AccessScope::for_tenant(T0)`.

This is a documented deviation from the Secure ORM rule against building `AccessScope` manually in production code. The manual scope is narrower than any clamp the PDP could return for the same rows.

### The PDP tenant constraint is a gate on the acting tenant

Every decision is asked with `owner_tenant_id = T0`, also when the returned row is owned by an ancestor. A constraint alternative is kept when its `owner_tenant_id` filter is `Eq` or `In` and admits T0. When no alternative remains, the result is `NOT_PERMITTED`. The tenant constraint never becomes a SQL clamp. Which ancestor rows T0 sees is decided by the catalog's sharing and shadowing rules, not by the PDP.

Row constraints on `id`, `provider_id`, `canonical_id`, and `gts_type` apply as returned: in SQL on lists, in memory on point operations. The PDP never sees an inherited row's real owner; this is accepted.

### Barriers are respected for the ancestor chain

The chain is read with Tenant Resolver's default `BarrierMode::Respect`, one chain only, cached per replica by the chain cache (`cpt-cf-model-registry-component-chain-cache`), with one Tenant Resolver call per miss. A self-managed tenant has an empty chain and sees its own rows only. A tenant below a self-managed tenant has a chain that ends at that tenant: it inherits the self-managed tenant's `shared` rows and nothing from above it. The gear never calls Tenant Resolver with `BarrierMode::Ignore`. This departs from CredStore ADR-0005, where shared secrets cross barriers.

### Subtree constraints fail closed

An `InTenantSubtree` filter on `owner_tenant_id` fails closed, regardless of `respect_barriers` or `descendant_status`: an alternative that carries it admits nothing (D5). The same holds for unknown `ScopeFilter` variants (the enum is `#[non_exhaustive]`) and for constraint kinds that cannot be evaluated in memory. The gate does not evaluate the chain.

### One visibility predicate for every read path

One storage function produces the scope and the visibility predicates for every read path: resolution, the available list, management lists, and management point reads. No repository read method accepts a caller-supplied tenant scope. A test enumerates the read methods.

### Consequences

* The Secure ORM deviation is documented here and is not to be "fixed" without superseding this ADR.
* PDP policies cannot target the owner of an inherited row; they are written against the acting tenant and the row properties above.
* Any policy that grants through `InTenantSubtree` denies in this gear. Policies for this gear must use `Eq` or `In` on the acting tenant.
* Hierarchy changes, including a change of a tenant's `self_managed` flag, apply within the chain-cache TTL.
* A self-managed tenant does not receive the root tenant's platform-wide shared catalog. Its administrator owns the catalog for its subtree.
* The chain cache holds one chain per entry and makes one Tenant Resolver call per miss; the gate adds no call (D6).
* Security: the scope bounds every read to the chain; no downward read exists, and no read crosses an isolation barrier. Performance: unchanged beyond the cached chain. Reliability: a Tenant Resolver failure on a miss fails closed. Data, integration, operations, compliance, UX, business: not affected.

### Confirmation

* Integration (SQLite and PostgreSQL): a self-managed tenant sees none of its ancestors' entries, `shared` or not; a descendant of a self-managed tenant sees that tenant's `shared` entries and nothing from above it; ancestors see nothing of the self-managed subtree.
* Integration: `InTenantSubtree` denies with `respect_barriers` true or false, with or without `descendant_status`; an unknown `ScopeFilter` variant denies.
* Integration: `Eq` or `In` admitting T0 passes; not admitting T0 returns `NOT_PERMITTED`.
* Integration: a caller `$filter` never widens a list; a PDP row constraint narrows lists in SQL and point reads in memory.
* Unit: a read-method enumeration test fails when a repository read method does not build its scope through the shared function.
* Review: no `BarrierMode::Ignore` call exists in the gear; the chain call uses `GetAncestorsOptions::default()`.

## Pros and Cons of the Options

### Gear-built upward scope, tenant constraint as a gate, subtree constraints fail closed

Reads run under `AccessScope::for_tenants(chain)` with the catalog predicates ANDed on; the PDP tenant constraint admits or refuses T0.

* Good, because inheritance works and stops at isolation barriers, with no new platform machinery.
* Good, because one authorization path serves every read and the scope is a strict upper bound.
* Good, because the scope and gate are the same model as CredStore.
* Bad, because it deviates from the Secure ORM rule.
* Bad, because the PDP never sees an inherited row's owner, and `InTenantSubtree` policies deny.

### Own rows only

Apply the PDP scope as returned; no inheritance.

* Good, because there is no deviation.
* Bad, because it contradicts `cpt-cf-model-registry-fr-effective-catalog`; the gear has no purpose without inheritance.

### PDP tenant constraint as SQL clamp, inheritance by a second unscoped query

Own rows through `SecureConn`; ancestor rows through a query outside the scope.

* Good, because own rows stay under the PDP clamp.
* Bad, because the inherited query runs outside any scope: the same deviation with weaker bounds.
* Bad, because two result sets are merged before shadowing, on the hot path.

### Declare `tenant_hierarchy` and project `tenant_closure`

Secure ORM expands subtree predicates in SQL; the PDP returns a predicate that admits ancestors.

* Good, because subtree predicates run in SQL; this is the platform direction for hierarchy-aware gears.
* Bad, because it needs a `tenant_closure` projection kept in sync with Account Management, a cross-gear data flow this gear does not otherwise need.
* Bad, because the PDP must still return a predicate admitting ancestors, which it does not do today.

### Per-row PDP decisions with the real owner tenant

Ask the PDP once per candidate row with `owner_tenant_id` set to the row's owner.

* Good, because the PDP sees the real owner.
* Bad, because it costs one decision per candidate row, up to eleven per resolution and one per page row on lists, against a 50 ms gateway budget.
* Bad, because the acting tenant would need grants in each ancestor's tenant, which inverts the meaning of sharing.

### Option 1 with `BarrierMode::Ignore` for the ancestor chain

The chain is read through self-managed tenants, as CredStore ADR-0005 does, so a self-managed tenant inherits its ancestors' `shared` rows.

* Good, because the inheritance surface is the same as CredStore's.
* Bad, because shared catalog data crosses an isolation barrier; the architecture review rejected this for catalog rows.
* Bad, because it is one more departure from a platform default.

## More Information

Revisit triggers:

* The platform offers a maintained `tenant_closure` projection to in-process gears, or the PDP returns tenant predicates that admit ancestors for a declared hierarchy capability.
* The PDP starts sending `InTenantSubtree` constraints routinely.
* A product need for PDP policies that refer to the owner of an inherited row.
* A product need for shared catalog entries to reach a self-managed subtree.

This ADR follows [CredStore ADR-0005](../../../credstore/docs/ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md) for the upward scope, the tenant gate, and the fail-closed handling of subtree constraints, and departs from it on barriers: CredStore reads the chain with `BarrierMode::Ignore`, this gear with `BarrierMode::Respect`. Platform rule departed from: [Secure ORM](../../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md) (no manual `AccessScope` in production code).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-model-registry-fr-effective-catalog` — inherited rows are read under the upward scope with the catalog visibility predicates.
* `cpt-cf-model-registry-fr-provider-shadowing` — shadowing runs over rows of the chain's tenants only.
* `cpt-cf-model-registry-fr-provider-sharing` — ancestor providers are visible only when `sharing_mode = shared`.
* `cpt-cf-model-registry-fr-model-sharing` — ancestor models are visible only when own or `shared` and their provider wins its slug.
* `cpt-cf-model-registry-fr-tenant-isolation` — the scope is an upper bound; no downward or sibling read exists, and no read crosses an isolation barrier.
* `cpt-cf-model-registry-fr-authorization` — the PDP tenant constraint is a gate on the acting tenant; subtree constraints deny.
* `cpt-cf-model-registry-contract-tenant-hierarchy` — the ancestor chain is read with the default `BarrierMode::Respect`.
* `cpt-cf-model-registry-nfr-resolution-latency` — no Tenant Resolver call beyond the cached chain.
* `cpt-cf-model-registry-component-chain-cache` — caches one chain per replica, one Tenant Resolver call per miss.
