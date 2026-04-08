# Static CredStore Plugin

CredStore storage-backend plugin that serves pre-configured secrets from YAML configuration. Designed for development, testing, and fixed-credential deployments where a full secrets vault is unnecessary.

## Overview

The `cf-static-credstore-plugin` module provides:

- **Static secret mapping** — secrets defined in YAML config, loaded and validated at init
- **Four sharing scopes** — Private, Tenant, Shared (tenant-scoped), and Global
- **O(1) lookup** — secrets are pre-indexed into separate `HashMap`s per scope
- **Deterministic precedence** — lookup order: Private → Tenant → Shared → Global
- **Strict config validation** — invalid keys, duplicate entries, and contradictory field combinations are rejected at startup

The plugin registers itself via the types registry as a `CredStorePluginClientV1` implementation and is discovered by the `credstore` gateway module.

## Configuration

Add the plugin section under your module configuration:

```yaml
static-credstore-plugin:
  config:
    vendor: "hyperspot"   # GTS vendor name (default: "hyperspot")
    priority: 100          # Plugin priority, lower = higher (default: 100)
    secrets:
      # Private secret — only accessible by this specific user in this tenant
      - tenant_id: "11111111-1111-1111-1111-111111111111"
        owner_id: "22222222-2222-2222-2222-222222222222"
        key: "my-api-key"
        value: "sk-secret-123"

      # Tenant secret — accessible by any user within the tenant
      - tenant_id: "11111111-1111-1111-1111-111111111111"
        key: "team-api-key"
        value: "sk-team-456"

      # Shared secret — tenant-scoped, visible to descendant tenants via gateway walk-up
      - tenant_id: "11111111-1111-1111-1111-111111111111"
        key: "org-api-key"
        value: "sk-org-789"
        sharing: "shared"

      # Global secret — accessible by any tenant and any user (fallback)
      - key: "platform-api-key"
        value: "sk-global-000"
```

### Secret fields

| Field       | Type            | Required | Description                                                                 |
|-------------|-----------------|----------|-----------------------------------------------------------------------------|
| `tenant_id` | `UUID`          | No       | Tenant scope. `None` → global secret.                                       |
| `owner_id`  | `UUID`          | No       | Subject scope. **Only valid for `private` sharing.** Requires `tenant_id`.  |
| `key`       | `string`        | Yes      | Secret reference key. Must match `SecretRef` format (alphanumeric, `-`, `_`). |
| `value`     | `string`        | Yes      | Plaintext secret value (converted to bytes at init).                        |
| `sharing`   | `SharingMode`   | No       | Explicit sharing mode. When omitted, inferred from `tenant_id`/`owner_id`.  |

### Sharing mode inference

When `sharing` is omitted, the mode is inferred automatically:

| `tenant_id` | `owner_id` | Inferred mode |
|:------------|:-----------|:--------------|
| `None`      | —          | `shared` (global) |
| `Some`      | `None`     | `tenant`      |
| `Some`      | `Some`     | `private`     |

You can override the default with an explicit `sharing` value (e.g. set `sharing: "shared"` on a tenant-scoped secret to make it visible to descendant tenants).

### Validation rules

The plugin rejects invalid configurations at startup with a descriptive error:

- **Invalid key** — `key` must be a valid `SecretRef` (alphanumeric, `-`, `_`)
- **Nil UUIDs** — `tenant_id` and `owner_id` must not be `00000000-0000-0000-0000-000000000000`
- **`owner_id` without `tenant_id`** — global secrets cannot have an owner
- **`owner_id` on non-Private secret** — `owner_id` is only valid when resolved sharing is `private`
- **`private` without `owner_id`** — explicit `sharing: "private"` requires `owner_id`
- **Global with non-Shared mode** — `tenant_id: None` only allows `shared` (or inferred `shared`)
- **Duplicate keys** — within the same scope (same tenant + sharing mode), keys must be unique

## Lookup precedence

When a secret is requested, the plugin checks maps in this order:

1. **Private** — keyed by `(tenant_id, owner_id, key)`, matched against `SecurityContext`
2. **Tenant** — keyed by `(tenant_id, key)`, any subject in the tenant
3. **Shared** — keyed by `(tenant_id, key)`, tenant-scoped but visible to descendants
4. **Global** — keyed by `key` only, fallback for any caller

The first match wins. This means a Private secret shadows a Tenant secret with the same key for the matching user, while other users in the same tenant still see the Tenant-level value.

### `SecretMetadata::owner_id` resolution

For **Private** secrets, `owner_id` comes from the config. For **Tenant**, **Shared**, and **Global** secrets, `owner_id` is not stored — the plugin fills it from `SecurityContext::subject_id()` of the caller at lookup time.

## Architecture

```
module.rs          ModKit module — init, config loading, GTS registration
config.rs          YAML config model + resolve_sharing() + validation docs
domain/
  service.rs       Service — from_config() builder + get() lookup
  client.rs        CredStorePluginClientV1 impl (maps SecretEntry → SecretMetadata)
  mod.rs           Re-exports
```

### Init sequence

1. Load `StaticCredStorePluginConfig` from module config
2. `Service::from_config()` — validate all entries, build lookup maps
3. Register GTS plugin instance in types-registry
4. Store `Arc<Service>` in module state
5. Register `CredStorePluginClientV1` scoped client in `ClientHub`

## Testing

```bash
cargo test -p cf-static-credstore-plugin
```

The test suite covers:

- Lookup per scope (private, tenant, shared, global)
- Precedence across all four scopes
- Owner/tenant isolation
- Config validation (all rejection rules)
- Sharing mode inference and explicit overrides
- `SecretMetadata` owner resolution from `SecurityContext`

## License

Apache-2.0
