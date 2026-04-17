# RG AuthZ Plugin

Reference implementation of an AuthZ resolver plugin that uses the Resource Group module for tenant hierarchy resolution and group-based access control.

## Purpose

Demonstrates how an AuthZ plugin integrates with the RG module via `ResourceGroupReadHierarchy` trait to produce row-level access constraints. Intended as:

- A reference for building production AuthZ plugins with RG integration
- A testbed for the AuthZ + RG cross-module contract
- A starting point for vendors implementing custom authorization logic

## How It Works

1. **Tenant resolution** — extracts `tenant_id` from `TenantContext.root_id` or `subject.properties["tenant_id"]`
2. **Hierarchy query** — calls `get_group_descendants(tenant_id)` via `ResourceGroupReadHierarchy` (resolved from ClientHub)
3. **Barrier filtering** — walks the parent chain in memory; barrier groups (`metadata.barrier = true`) and all their descendants are excluded from the visible scope
4. **Predicate generation** — returns `In(owner_tenant_id, [visible_tenant_ids])` for tenant scoping, plus optional `InGroup` / `InGroupSubtree` predicates when group context is present in the request

### Barrier Semantics

A **barrier** is a tenant group with `metadata.barrier = true` (equivalent to `self_managed = true` in the tenant model). A barrier creates an isolation boundary: the parent tenant cannot see resources belonging to the barrier tenant or any of its descendants.

This is critical for multi-tenant SaaS platforms where a sub-tenant manages its own data independently. For example, a reseller (T1) provisions a self-managed customer (T7). T7's data is invisible to T1 even though T7 is in T1's hierarchy.

**Algorithm:** after fetching the full descendant tree from RG, the plugin walks each group's `parent_id` chain. If any ancestor in the chain has `metadata.barrier = true`, the group is excluded from the visible scope.

This mirrors the planned `tenant_closure.barrier` column behavior where `barrier = 1` means a barrier exists **on the path** between ancestor and descendant (not at the ancestor itself).

**Example hierarchy:**

```
T1 (root tenant)
├── T2 (normal department)       -- visible to T1
├── T7 (barrier=true, reseller)  -- NOT visible to T1
│   └── T8 (T7's customer)      -- NOT visible to T1 (behind barrier)
└── T9 (normal branch)           -- visible to T1
```

| Group | Visible to T1? | Reason |
|-------|----------------|--------|
| T1 | Yes | Root (self) |
| T2 | Yes | No barrier on path from T1 |
| T7 | **No** | Barrier group itself |
| T8 | **No** | Behind barrier (T7 is on path from T1 to T8) |
| T9 | Yes | No barrier on path from T1 |

**Note:** T7 querying its own subtree sees T7 + T8 (barrier is not "between" T7 and its descendants). This matches `tenant_closure` semantics where `ancestor_id = T7` rows have `barrier = 0`.

### Fail-Closed Policy

| Scenario | Decision | Reason |
|----------|----------|--------|
| Valid tenant + non-empty hierarchy | `true` | Normal operation |
| No tenant resolvable | `false` | Cannot scope access |
| Nil UUID tenant | `false` | Invalid identity |
| Empty hierarchy from RG | `false` | No visible scope |
| RG service error | `false` | Fail-closed on dependency failure |

## Configuration

```yaml
modules:
  rg_authz_plugin:
    config:
      vendor: "hyperspot"
      priority: 50
```

## Dependencies

- **`resource-group-sdk`** — `ResourceGroupReadHierarchy` trait for hierarchy queries
- **`authz-resolver-sdk`** — `AuthZResolverPluginClient` trait, predicate types
- **`types-registry-sdk`** — GTS plugin instance registration

## Related Documentation

- [AuthZ Architecture Design](../../../../docs/arch/authorization/DESIGN.md) — predicate types, capabilities, projection tables
- [Tenant Model](../../../../docs/arch/authorization/TENANT_MODEL.md) — tenant hierarchy, barriers, closure table semantics
- [Resource Group Model](../../../../docs/arch/authorization/RESOURCE_GROUP_MODEL.md) — RG topology, projection strategy
- [AuthZ Usage Scenarios](../../../../docs/arch/authorization/AUTHZ_USAGE_SCENARIOS.md) — end-to-end authorization flows
- [RG Module Design](../../../resource-group/docs/DESIGN.md) — `ResourceGroupReadHierarchy` trait, MTLS integration
- [Integration Test Plan](../../../../docs/arch/authorization/INTEGRATION_TEST_PLAN.md) — AuthZ + RG integration verification
