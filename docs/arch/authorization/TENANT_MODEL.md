# Tenant Model

This document describes Gears' multi-tenancy model, tenant topology, and isolation mechanisms.

## Table of Contents

- [Tenant Model](#tenant-model)
  - [Table of Contents](#table-of-contents)
  - [Overview](#overview)
  - [Tenant Topology: Single-Root Tree](#tenant-topology-single-root-tree)
  - [Tenant Properties](#tenant-properties)
  - [Barriers (Self-Managed Tenants)](#barriers-self-managed-tenants)
  - [Context Tenant vs Subject Tenant](#context-tenant-vs-subject-tenant)
  - [Tenant Subtree Queries](#tenant-subtree-queries)
  - [Closure Table](#closure-table)
  - [References](#references)

---

## Overview

Gears use a **hierarchical multi-tenancy** model where tenants form a single-root tree. Every tenant except the root has exactly one parent, and all tenants descend from a single shared root. Tenants can have child tenants, creating organizational structures like:

```
Root
├── Organization A
│   ├── Team A1
│   └── Team A2
└── Organization B
    ├── Team B1
    └── Team B2
```

Key principles:
- **Isolation by default** — tenants cannot access each other's data
- **Hierarchical access** — parent tenants may access child tenant data (configurable)
- **Barriers** — child tenants can opt out of parent visibility via `self_managed` flag

---

## Tenant Topology: Single-Root Tree

The tenant structure is a **single-root tree** — every tenant except the root has exactly one parent, and all tenants descend from a single shared root.

```
           [Root]          ← The only tenant with no parent
          /      \
       [T1]      [T5]
      /    \       |
   [T2]    [T3]  [T6]
     |
   [T4]
```

**Properties:**
- Exactly one tenant in the whole hierarchy has `parent_id = NULL`; this tenant is the root.
- Every other tenant has exactly one parent.
- Depth is not bounded by the tenant model itself; any limits on hierarchy depth come from the concrete Account/Tenant Management service implementation (operational policy, performance envelope, storage constraints).
- The root is identified by convention (the single tenant with `parent_id = NULL`). There is no `is_system` field, and the root is not referred to as a "system tenant".

**Relationship with the RG forest.** When tenants are materialized as groups in the Resource Group gear, the single-root tree is embedded inside the broader **RG forest**: RG may hold several root groups (ownership-graph roots, auxiliary forests), each carrying its own `tenant_id`. At most one of those roots may be a tenant-type group — that root *is* the single tenant root described above. All other tenants live below it as sub-tenants. The RG create path assigns a non-tenant root to the target tenant, which defaults to the caller’s tenant. A tenant-type group takes its own group ID as its tenant ID. RG rejects creation of a second tenant-type root with `TenantRootAlreadyExists` (409 Conflict). [Group creation](../../../gears/system/resource-group/resource-group/src/domain/group_service.rs) · [Tenant root requirement](../../../gears/system/resource-group/docs/PRD.md#enforce-tenant-root-uniqueness)

**Why single-root tree?**
- **One OAuth client is enough** for S2S tenant-scoped flows that need to act as the root — no per-root credential fan-out at the vendor IdP.
- **Unambiguous "act as root" semantics** — platform-level tenant-scoped operations always address the same tenant.
- **Organizational autonomy is preserved via sub-roots** — in multi-tenant deployments each independent organization is modelled as its own sub-root directly under the root; barriers continue to provide isolation between sub-trees.
- **Works naturally for single-user / consumer deployments** of Gears -based products — the root *is* the tenant that owns all business objects, and no sub-roots are created.
- **Avoids the accidental complexity of DAGs** — closure-table rows, barriers, and ancestry queries stay tree-shaped.

**Deployment shapes:**

Both shapes satisfy the same topology invariant (exactly one `parent_id = NULL`); what differs is whether business objects live on the root.

| Deployment | Role of the root | Business objects on the root? |
|------------|------------------|-------------------------------|
| Multi-tenant vendor deployment | Structural anchor above organization sub-roots | No — they live on sub-roots and below |
| Single-user / consumer deployment | Owner of all platform data for the sole user | Yes |

See [ADR 0004](./ADR/0004-single-root-tenant-model-topology.md) for the rationale and the rejected alternatives (forest, DAG).

---

## Tenant Properties

| Property | Type | Description |
|----------|------|-------------|
| `id` | UUID | Unique tenant identifier |
| `parent_id` | UUID? | Parent tenant. `NULL` for exactly one tenant: the root. |
| `status` | enum | `active`, `suspended`, `deleted` |
| `self_managed` | bool | If true, creates a barrier — parent cannot access this subtree |

**Status semantics:**
- `active` — normal operation
- `suspended` — tenant temporarily disabled (e.g., billing issue), data preserved
- `deleted` — soft-deleted, may be purged after retention period

---

## Barriers (Self-Managed Tenants)

A **barrier** is created when a tenant sets `self_managed = true`. This prevents parent tenants from accessing the subtree rooted at the barrier tenant.

**Example:**

```
T1 (parent)
├── T2 (self_managed=true)  ← BARRIER
│   └── T3
└── T4
```

**Access from T1's perspective:**
- ✅ Can access T1's own resources
- ❌ Cannot access T2's resources (barrier)
- ❌ Cannot access T3's resources (behind barrier)
- ✅ Can access T4's resources

**Access from T2's perspective:**
- ✅ Can access T2's own resources
- ✅ Can access T3's resources (T3 is in T2's subtree, no barrier between them)

**Use cases:**
- Enterprise customer wants data isolation from reseller/partner
- Compliance requirements (data sovereignty)
- Organizational autonomy within a larger structure

**Barrier interpretation is context-dependent:**

Barriers are not absolute — their enforcement depends on the type of data and operation. The same parent-child relationship may have different access rules for different resource types:

| Data Type | Barrier Enforced? | Rationale |
|-----------|-------------------|-----------|
| Business data (tasks, documents) | ✅ Yes | Core isolation requirement |
| Usage/metrics for billing | ❌ No | Parent needs to bill child tenant |
| Audit logs | ⚠️ Configurable | Compliance may require parent visibility |
| Tenant metadata (name, status) | ❌ No | Parent needs to manage child tenants |

**Example:** Reseller T1 has enterprise customer T2 (`self_managed=true`):
- T1 ❌ cannot read T2's business data (tasks, files, etc.)
- T1 ✅ can read T2's usage metrics for billing purposes
- T1 ✅ can see T2's tenant metadata (name, status, plan)

This means `barrier_mode` in authorization requests applies to specific resource types, not globally. Each gear/endpoint decides whether barriers apply to its resources.

**Implementation:** The `tenant_closure` table includes a `barrier` column that indicates whether a barrier exists between ancestor and descendant. See [Closure Table](#closure-table).

---

## Context Tenant vs Subject Tenant

Two different tenant concepts appear in authorization:

| Concept | Description | Example |
|---------|-------------|---------|
| **Subject Tenant** | Tenant the user belongs to (from token/identity) | User's "home" organization |
| **Context Tenant** | Tenant scope for the current operation | May differ for cross-tenant operations |

**Typical case:** Subject tenant = Context tenant (user operates in their own tenant)

**Cross-tenant case:** Admin from parent tenant T1 operates in child tenant T2's context:
- Subject tenant: T1 (where admin belongs)
- Context tenant: T2 (where operation is scoped)

**In authorization requests:**
```jsonc
{
  "subject": {
    "properties": { "tenant_id": "T1" }  // Subject tenant
  },
  "context": {
    "tenant_context": {
      "mode": "root_only",  // Single tenant T2
      "root_id": "T2"
    }
    // OR for subtree:
    // "tenant_context": {
    //   "mode": "subtree",   // T2 + descendants
    //   "root_id": "T2"
    // }
  }
}
```

---

## Tenant Subtree Queries

Many operations need to query "all resources in tenant T and its children". This is a **subtree query**.

**Options for subtree queries:**

| Approach | Pros | Cons |
|----------|------|------|
| Recursive CTE | No extra tables | Slow for deep hierarchies, not portable |
| Explicit ID list from PDP | Simple SQL | Doesn't scale (thousands of IDs) |
| Closure table | Indexed ancestor lookup without recursive traversal | Requires sync, storage overhead |

Gears recommend **closure tables** for production deployments with hierarchical tenants.

**Tenant scope parameters (in `context.tenant_context`):**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `mode` | `"subtree"` | `"root_only"` (single tenant) or `"subtree"` (tenant + descendants) |
| `root_id` | — | Root tenant. Optional — PDP can determine from `token_scopes` or `subject.properties.tenant_id` |
| `barrier_mode` | `"respect"` | `"respect"` (respect barriers) or `"ignore"` (ignore). See [DESIGN.md](./DESIGN.md#3-tenant-subtree-predicate-op-in_tenant_subtree). |
| `tenant_status` | all | List of descendant statuses (`active`, `suspended`, `deleted`). In TR subtree resolution, the root is returned independently of this filter |

---

## Closure Table

The `tenant_closure` table is a denormalized representation of the tenant hierarchy. It contains every ancestor–descendant pair for SDK-visible tenants, including each tenant’s self-row. Account Management creates closure rows when provisioning completes.

**Schema:**

| Column | Type | Description |
|--------|------|-------------|
| `ancestor_id` | UUID | Ancestor tenant |
| `descendant_id` | UUID | Descendant tenant |
| `barrier` | SMALLINT NOT NULL DEFAULT 0 | 0 = no respected barrier on path `(ancestor, descendant]`, 1 = at least one `self_managed` tenant exists on that path |
| `descendant_status` | SMALLINT NOT NULL | Status of descendant tenant: `1=active`, `2=suspended`, `3=deleted` (denormalized for query efficiency). Wire values use the corresponding names. |

**Barrier semantics:** The `barrier` column is defined over the path `(ancestor, descendant]`: the ancestor endpoint is excluded, the descendant endpoint is included. Formally, `barrier = 1` iff any tenant on that path has `self_managed = true`. Self-rows `(id, id)` are a special case and always carry `barrier = 0`.

This endpoint rule is what makes a plain `AND barrier = 0` predicate satisfy the SDK contract:
- `get_ancestors(T, Respect)` returns empty if `T` itself is self-managed, because every strict-ancestor row ending at `T` has `barrier = 1`
- `get_descendants(T, Respect)` excludes a self-managed child `C` and its subtree, because rows starting at `T` and ending at `C` or below have `barrier = 1`
- `is_ancestor(A, D, Respect)` returns `false` when `D` itself is self-managed or when another self-managed tenant lies on `(A, D]`

**Example data for the hierarchy:**

```
T1
├── T2 (self_managed=true)
│   └── T3
└── T4
```

| ancestor_id | descendant_id | barrier | descendant_status (label) |
|-------------|---------------|---------|-------------------|
| T1 | T1 | 0 | active |
| T1 | T2 | 1 | active |
| T1 | T3 | 1 | active |
| T1 | T4 | 0 | active |
| T2 | T2 | 0 | active |
| T2 | T3 | 0 | active |
| T3 | T3 | 0 | active |
| T4 | T4 | 0 | active |

**Key observations:**
- `T1 → T2`: barrier = 1 because the descendant endpoint `T2` is self-managed and the descendant endpoint is included in `(ancestor, descendant]`
- `T1 → T3`: barrier = 1 because `T2` is on `(T1, T3]`
- `T2 → T2`: barrier = 0 because self-rows always carry `barrier = 0`
- `T2 → T3`: barrier = 0 because ancestor `T2` is excluded from `(T2, T3]`
- Because the hierarchy has a single root, that root appears as `ancestor_id` of every tenant in the table (subject to the barrier rules above).

**Query: "All tenants in T1's subtree, with `barrier_mode: "respect"`"**

```sql
-- barrier_mode: "respect" (default) adds the barrier clause
SELECT descendant_id FROM tenant_closure
WHERE ancestor_id = 'T1'
  AND barrier = 0
-- barrier_mode: "ignore" omits the barrier clause
```

Result: T1, T4 (T2 and T3 excluded due to barrier = 1)

**Query: "All tenants in T2's subtree"**

```sql
SELECT descendant_id FROM tenant_closure WHERE ancestor_id = 'T2' AND barrier = 0
```

Result: T2, T3 (`T2 → T2` is the self-row; `T2 → T3` has `barrier = 0` because ancestor `T2` is excluded from `(T2, T3]`)

**Future extensibility:** The `barrier` column is SMALLINT to allow future use as a bitmask for multiple barrier types (e.g., bit 0 for self_managed, bit 1 for data_sovereignty). 16 bits of bitmask headroom is ample for any realistic number of barrier dimensions, and the type is portable across PostgreSQL and MySQL. Selective enforcement changes from `barrier = 0` to a `(barrier & mask) = 0` check without touching the schema.

**Synchronization:** How projection tables are synchronized with vendor systems, consistency guarantees, and conflict resolution are out of scope for this document. See Tenant Resolver design documentation (TBD).

---

## References

- [DESIGN.md](./DESIGN.md) — Core authorization design
- [RESOURCE_GROUP_MODEL.md](./RESOURCE_GROUP_MODEL.md) — Resource group topology, membership, hierarchy
- [AUTHZ_USAGE_SCENARIOS.md](./AUTHZ_USAGE_SCENARIOS.md) — Authorization scenarios with tenant examples
- [ADR 0004: Single-Root Tenant Model Topology](./ADR/0004-single-root-tenant-model-topology.md) — Rationale for single-root tree vs. forest vs. DAG
