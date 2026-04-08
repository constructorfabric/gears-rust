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

PEP enforces group-based constraints (`in_group`, `in_group_subtree`) in SQL by joining against projection tables. Two RG tables are relevant for AuthZ projections:

- **`resource_group`** — group entities with hierarchy (`parent_id`) and tenant scope (`tenant_id`)
- **`resource_group_closure`** — pre-computed ancestor-descendant pairs with depth, enabling efficient subtree queries

These tables are the canonical source of truth, owned by the RG module. External consumers (AuthZ resolver, Tenant Resolver, domain services) may maintain their own **projection copies** of these tables in their databases for efficient SQL joins — synchronized from RG via read contracts (`ResourceGroupReadHierarchy`).

> **Note:** `resource_group_membership` (resource-to-group M:N links) is a separate RG canonical table used for `in_group` predicates. It is not part of the hierarchy projection. Is expected to be very big and not recommended for projection.

PEP compiles SQL predicates that reference whichever projection is available in the domain service's database. The RG module does not dictate the projection schema in domain services — it only provides the canonical data and read contracts.

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
