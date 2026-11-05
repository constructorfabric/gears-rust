# RG Tenant Resolver Plugin

Tenant resolver plugin that resolves tenant hierarchy via the Resource Group gear. Production replacement for `static-tr-plugin`.

## How It Works

Tenants are represented as RG groups whose type code is one of the configured `tenant_types` (default: `TENANT_RG_TYPE_PATH` = `gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`). The plugin reads tenant data via `ResourceGroupReadHierarchy` (bypass AuthZ), filters with `type in (<tenant_types>)`, and maps matching groups to `TenantInfo` / `TenantRef`.

### Metadata Convention

```json
{
  "status": "active",
  "self_managed": false
}
```

- `status` — maps to `TenantStatus` (active/suspended/deleted), default: active
- `self_managed` — barrier flag, default: false

### Barrier Semantics

Same as `static-tr-plugin`:
- **Ancestors**: if starting tenant is `self_managed`, return empty. Otherwise include barrier ancestor but stop traversal.
- **Descendants**: skip `self_managed` children and their subtrees.
- **is_ancestor**: `false` if descendant is `self_managed` or a barrier lies between them.

## Configuration

```yaml
gears:
  rg_tr_plugin:
    config:
      vendor: "constructorfabric"
      priority: 50
      # Exact RG type paths treated as tenants. Default: [TENANT_RG_TYPE_PATH].
      # Unlike resource-group's `tenant_type_paths` (prefixes), list every
      # concrete tenant type. Every entry must be registered in resource-group
      # (RG rejects a `$filter` on an unknown type).
      tenant_types:
        - "gts.cf.core.rg.type.v1~acme.core._.tenant.v1~"
```

## Dependencies

- **`resource-group-sdk`** — `ResourceGroupReadHierarchy` trait for hierarchy queries
- **`tenant-resolver-sdk`** — `TenantResolverPluginClient` trait, domain models
- **`types-registry-sdk`** — GTS plugin instance registration
