# TR AuthZ Plugin

AuthZ resolver plugin that resolves tenant hierarchy via `TenantResolverClient`. Delegates all tenant operations to the tenant-resolver gear instead of accessing Resource Group directly.

## How It Works

1. **Tenant resolution** — `subject.properties["tenant_id"]` is always required (missing or nil → deny). `TenantContext.root_id` is a separate, optional target tenant: together with `resource.id` and `TenantContext.mode` it selects one of the rules R1–R8.
2. **Hierarchy query** — calls `TenantResolverClient::is_ancestor` and `get_descendants`, with the barrier mode taken from the request's `TenantContext.barrier_mode` and `get_descendants` filtered by its `tenant_status`.
3. **Predicate generation** — returns an `In` predicate on `owner_tenant_id` for tenant scoping (a single-value `In` for one tenant), plus `InGroup` when the resource has `group_ids` and the PEP advertises `GroupMembership`, and `InGroupSubtree` for `ancestor_group_ids` with `GroupMembership` and `GroupHierarchy`. A missing capability, an empty array or a malformed array is a deny

## Configuration

```yaml
gears:
  tr-authz-plugin:
    config:
      vendor: "constructorfabric"
      priority: 50  # Set lower than static-authz (100) to take precedence
```

## Dependencies

- **`tenant-resolver-sdk`** — `TenantResolverClient` trait for tenant hierarchy queries
- **`authz-resolver-sdk`** — `AuthZResolverPluginClient` trait, predicate types
- **`types-registry-sdk`** — GTS plugin instance publication into the platform catalogue (`PlatformTypesRegistryApi`), on the types-registry linked into the same process or on a remote one; an in-process registry without a database fails boot
