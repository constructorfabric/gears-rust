# Static `CredStore` Plugin

`CredStore` **value-store** backend for development and testing: an in-memory
per-tenant secret store, optionally seeded from YAML configuration. Implements
the `CredStorePluginClientV1` contract (`get`/`put`/`delete`) so the stateful
`credstore` gear can use it as a backend without a full secrets vault.

## Overview

The `cf-gears-static-credstore-plugin` module provides:

- **Per-tenant value store** — `get`/`put`/`delete` keyed by `tenant_id` + `key`
  + optional `owner_id` (`Some` = private key class, `None` = tenant key class).
    No sharing/hierarchy/policy here — that lives in the gear.
- **Writable at runtime** — the gear's write saga (`put`/`delete`) mutates the
  in-memory store, so it works as a development backend, not just a read fixture.
- **Config seeding** — secrets defined in YAML are loaded and validated at init.
- **Read fallbacks** — config-seeded `shared`/global entries serve `owner_id =
  None` reads when no tenant-class entry exists.
- **Strict config validation** — invalid keys, duplicate entries, and
  contradictory field combinations are rejected at startup.

> **Note (stateful gear):** the gear resolves metadata from its own
> database first and only then reads the value here. A secret seeded **only** in
> this plugin's config (with no corresponding gear metadata row) is therefore
> *not* reachable through the gear — write it via the credstore API
> (`POST/PUT /credstore/v1/secrets`) so a metadata row exists.

The plugin registers itself via the types registry as a `CredStorePluginClientV1` implementation and is discovered by the `credstore` gear module.

## Rust usage

`ToolKit` normally discovers and instantiates the plugin through inventory. Direct
construction is useful for host wiring tests:

```rust
use static_credstore_plugin::StaticCredStorePlugin;

let plugin = StaticCredStorePlugin::default();
```

## Configuration

Add the plugin section under your gear configuration:

```yaml
static-credstore-plugin:
  config:
    vendor: "constructorfabric"   # GTS vendor name (default: "constructorfabric")
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

      # Shared secret — tenant-scoped, visible to descendant tenants via gear walk-up
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

## Read resolution

The gear calls `get(tenant_id, key, owner_id)`. The plugin resolves against
its in-memory key classes:

- **`owner_id = Some`** → the **private** class only: `(tenant_id, owner_id, key)`.
- **`owner_id = None`** → the **tenant** class `(tenant_id, key)`, falling back to
  config-seeded **shared** `(tenant_id, key)` then **global** `key`.

`put`/`delete` target the private class when `owner_id = Some`, otherwise the
tenant class (with `delete` also sweeping the `shared`/global fallbacks). The
config-seeded `shared`/global maps exist only to keep development configs
resolving; they are never written by `put`. The plugin returns the raw
`SecretValue` — all sharing/owner metadata is owned by the gear.

## Architecture

```text
gear.rs            ToolKit gear — initialization and GTS/ClientHub registration
config.rs          YAML config model, sharing inference, and validation
domain/
  service.rs       In-memory key classes and runtime get/put/delete operations
  client.rs        CredStorePluginClientV1 adapter
  mod.rs           Domain exports
```

### Init sequence

1. Load `StaticCredStorePluginConfig` from module config
2. `Service::from_config()` — validate all entries, build lookup maps
3. Register GTS plugin instance in types-registry
4. Store `Arc<Service>` in module state
5. Register `CredStorePluginClientV1` scoped client in `ClientHub`

## Testing

```bash
cargo test -p cf-gears-static-credstore-plugin
```

The test suite covers:

- Read per key class (private vs tenant) and `shared`/global fallbacks
- `put`/`delete` round-trips and owner/tenant isolation
- Config validation (all rejection rules)
- Sharing mode inference and explicit overrides
- The `CredStorePluginClientV1` trait impl (`get`/`put`/`delete`)

## License

Apache-2.0
