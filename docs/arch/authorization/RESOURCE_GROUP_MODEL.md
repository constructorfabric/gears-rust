<!-- Updated: 2026-04-07 by Constructor Tech -->

# Resource Group Model — AuthZ Perspective

This document describes how CyberFabric's authorization system uses Resource Groups (RG) for access control. For the full RG module design (domain model, API contracts, database schemas, type system), see [RG Technical Design](../../../modules/system/resource-group/docs/DESIGN.md).

---

## Overview

CyberFabric uses **resource groups** as an optional organizational layer for grouping resources. The primary purpose from the AuthZ perspective is **access control** — granting permissions at the group level rather than per-resource.

```
Tenant T1
├── [Group A]
│   ├── Resource 1
│   ├── Resource 2
│   └── [Group A.1]
│       └── Resource 3
├── [Group B]
│   ├── Resource 1
│   └── Resource 4
└── (ungrouped resources)
```

Key principles:
- **Optional** — resources may exist without group membership
- **Many-to-many** — a resource can belong to multiple groups
- **Hierarchical** — groups form a strict forest (single parent, no cycles)
- **Tenant-scoped** — groups exist within tenant boundaries
- **Typed** — groups have dynamic GTS types with configurable parent/membership rules

For topology details (forest invariants, type system, query profiles), see [RG DESIGN §Domain Model](../../../modules/system/resource-group/docs/DESIGN.md#31-domain-model).

---

## How AuthZ Uses Resource Groups

AuthZ consumes RG data as a **PIP (Policy Information Point)** source. RG is policy-agnostic — it stores hierarchy and membership data without evaluating access decisions. AuthZ plugin reads this data to resolve group-based predicates.

### Projection Tables

RG tables are the canonical source of truth, owned by the RG module. External consumers (AuthZ resolver, domain services) may maintain **projection copies** in their databases — synchronized from RG via read contracts (`ResourceGroupReadHierarchy`).

**Projectable tables:**

- **`resource_group`** — group entities with hierarchy (`parent_id`) and tenant scope (`tenant_id`)
- **`resource_group_closure`** — pre-computed ancestor-descendant pairs with depth, enabling efficient subtree queries
- **`resource_group_membership`** — resource-to-group M:N links (see guidance below)

#### Progressive projection strategy

Whether and which tables to project depends on the deployment topology and access patterns. **Do not add projections speculatively** — each projection creates an additional database, synchronization load, and operational complexity.

| Deployment | Recommended projections | Rationale |
|------------|------------------------|-----------|
| **Monolith** (single shared DB) | **None** — all tables are already co-located | PEP JOINs against canonical tables directly; no extra databases or sync needed |
| **Microservices** (separate DBs, typical case) | **`resource_group` + `resource_group_closure`** | Enables `in_group_subtree` predicates locally; hierarchy tables are small (~100 K rows). Membership resolved by PDP via capability degradation → `in` predicates |
| **Microservices** with membership filtering/pagination | **`resource_group` + `resource_group_closure` + `resource_group_membership`** | Only when profiling confirms the two-request pattern (RG API → domain service) is unacceptable for latency budget. Membership table grows as `M_resources × N_groups_per_resource` and is expected to be **10× or more larger** than hierarchy tables — see [RG DESIGN §Storage Estimates](../../../modules/system/resource-group/docs/DESIGN.md#storage-estimates) for concrete numbers |

> **Important:** When a domain service query includes filters by resource group attributes (e.g., `GET /tasks?status=pending&project={projectX}&after=…&limit=50`), the two-request pattern means N additional round-trips to the RG Membership API (one per filter page or group), not just +1. If this N-request fan-out violates the latency budget, that is the signal to project the membership table locally.
>
> **Architecture guidance:** default to consuming degraded `in` predicates from PDP. The `in_group` and `in_group_subtree` predicates are natively executable within the RG module; domain services that choose not to project the membership table rely on PDP capability degradation.

PEP within the RG module compiles `in_group`/`in_group_subtree` predicates into SQL subqueries using the membership table. Domain services without the membership projection receive degraded `in` predicates and do not need group-related projection tables for authorization filtering.

- RG canonical table schemas: [RG DESIGN §Database Schemas](../../../modules/system/resource-group/docs/DESIGN.md#37-database-schemas--tables)
- When to use which table: [AUTHZ_USAGE_SCENARIOS §Choosing Projection Tables](./AUTHZ_USAGE_SCENARIOS.md#choosing-projection-tables)

### Access Inheritance

- **Explicit membership, inherited access** — a resource is added to a specific group (explicit). Access is inherited top-down: a user with access to parent group G1 can access resources in all descendant groups via `in_group_subtree` predicate.
- **Flat group access** — `in_group` predicate checks direct membership only (no hierarchy traversal).

### Integration Path

AuthZ plugin reads RG hierarchy via `ResourceGroupReadHierarchy` trait (narrow, hierarchy-only read contract). In microservice deployments, this uses MTLS-authenticated requests to the RG service; in monolith deployments, it's a direct in-process call via ClientHub. See [RG DESIGN §RG Authentication Modes](../../../modules/system/resource-group/docs/DESIGN.md#rg-authentication-modes-jwt-vs-mtls).

---

## Relationship with Tenant Model

**Tenants** and **Resource Groups** serve different purposes:

| Aspect | Tenant | Resource Group |
|--------|--------|----------------|
| **Purpose** | Ownership, isolation, billing | Grouping for access control |
| **Scope** | System-wide | Per-tenant |
| **Resource relationship** | Ownership (1:N) | Membership (M:N) |
| **Hierarchy** | Forest (multiple roots) | Forest (multiple roots per tenant) |
| **Type system** | Fixed (built-in tenant type) | Dynamic (GTS-based, vendor-defined types) |

Resource groups operate **within** tenant boundaries — groups are tenant-scoped, cross-tenant groups are forbidden, and authorization always includes a tenant constraint alongside group predicates.

**Key rules:**

1. **Groups are tenant-scoped** — a group belongs to exactly one tenant
2. **Cross-tenant groups are forbidden** — a group cannot span multiple tenants
3. **Tenant constraint always applies** — authorization always includes a tenant constraint alongside group predicates

**Further reading:**

- Tenant topology, barriers, closure tables: [TENANT_MODEL.md](./TENANT_MODEL.md)
- Tenant-hierarchy-compatible validation on group writes: [RG DESIGN §Tenant Scope for Ownership Graph](../../../modules/system/resource-group/docs/DESIGN.md#tenant-scope-for-ownership-graph)
- Tenant constraint compilation: [DESIGN.md](./DESIGN.md)

---

## References

- [RG Technical Design](../../../modules/system/resource-group/docs/DESIGN.md) — Full RG module design (domain model, API, database schemas, security, auth modes)
- [RG PRD](../../../modules/system/resource-group/docs/PRD.md) — Product requirements
- [RG OpenAPI](../../../modules/system/resource-group/docs/openapi.yaml) — REST API specification
- [DESIGN.md](./DESIGN.md) — Core authorization design
- [TENANT_MODEL.md](./TENANT_MODEL.md) — Tenant topology, barriers, closure tables
- [AUTHZ_USAGE_SCENARIOS.md](./AUTHZ_USAGE_SCENARIOS.md) — Authorization scenarios with resource group examples
