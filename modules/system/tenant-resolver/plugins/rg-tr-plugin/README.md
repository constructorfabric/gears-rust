# RG Tenant Resolver Plugin

Tenant resolver plugin that resolves tenant hierarchy via the Resource Group module. Production replacement for `static-tr-plugin`.

## How It Works

Tenants are represented as RG groups whose type code starts with `TENANT_RG_TYPE_PATH` (`gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`). The plugin reads tenant data via `ResourceGroupReadHierarchy` (bypass AuthZ), filters by this prefix, and maps matching groups to `TenantInfo` / `TenantRef`.

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
modules:
  rg_tr_plugin:
    config:
      vendor: "cyberfabric"
      priority: 50
```

## Dependencies

- **`resource-group-sdk`** — `ResourceGroupReadHierarchy` trait for hierarchy queries
- **`tenant-resolver-sdk`** — `TenantResolverPluginClient` trait, domain models
- **`types-registry-sdk`** — GTS plugin instance registration
