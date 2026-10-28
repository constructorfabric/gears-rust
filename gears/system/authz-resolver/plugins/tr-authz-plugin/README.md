# TR AuthZ Plugin

AuthZ resolver plugin that resolves tenant hierarchy via `TenantResolverClient`. Delegates all tenant operations to the tenant-resolver gear instead of accessing Resource Group directly.

## How It Works

1. **Tenant resolution** — extracts `tenant_id` from `TenantContext.root_id` or `subject.properties["tenant_id"]`
2. **Hierarchy query** — calls `TenantResolverClient::get_descendants(tenant_id, {barrier_mode: Respect})`. Barrier filtering is handled by tenant-resolver internally.
3. **Predicate generation** — returns `In(owner_tenant_id, [visible_tenant_ids])` for tenant scoping, plus optional `InGroup` / `InGroupSubtree` predicates when group context is present in the request

## Configuration

```yaml
gears:
  tr_authz_plugin:
    config:
      vendor: "constructorfabric"
      priority: 50  # Set lower than static-authz (100) to take precedence
```

## Dependencies

- **`tenant-resolver-sdk`** — `TenantResolverClient` trait for tenant hierarchy queries
- **`authz-resolver-sdk`** — `AuthZResolverPluginClient` trait, predicate types
- **`types-registry-sdk`** — GTS plugin instance publication into the platform catalogue (`PlatformTypesRegistryApi`) of the types-registry linked into the same process; the host must link a database-backed registry
