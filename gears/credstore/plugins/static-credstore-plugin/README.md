# Static `CredStore` Plugin

`CredStore` **value-store** backend for development and testing: an in-memory
versioned store. Implements the `CredStorePluginClientV2` contract
(`put`/`get`/`delete_key` plus the optional `destroy`) so the stateful
`credstore` gear can use it as a backend without a full secrets vault.

## Overview

The `cf-gears-static-credstore-plugin` module provides:

- **A dumb per-tenant versioned key-value store** - entries are keyed by
  `StoreKey { tenant_id, record_id }`; under each key every `put` creates a
  new immutable version. The plugin knows nothing about references, owners,
  sharing or hierarchy; the gear's metadata row names the current version
  through its `value_version` (ADR-0006).
- **Ordered versions** - a per-key monotonic counter: the n-th `put` under a
  key returns `"n"` (destroyed numbers are never reissued).
- **`destroy` supported** - `supports_destroy` is `true`; `Below(v)` removes
  every version older than `v`, `Exactly(v)` removes that one. `destroy` and
  `delete_key` of anything not held are successes (idempotent).
- **Writable at runtime** - the gear's write protocol mutates the in-memory
  store, so it works as a development backend, not just a fixture.
- **No config seeding** - values enter the store only through the credstore
  API. A `secrets:` block in this plugin's config is rejected at startup.

The plugin registers itself via the types registry as a `CredStorePluginClientV2`
implementation and is discovered by the `credstore` gear module.

## Rust usage

`ToolKit` normally discovers and instantiates the plugin through inventory. Direct
construction is useful for host wiring tests:

```rust
use static_credstore_plugin::StaticCredStorePlugin;

let plugin = StaticCredStorePlugin::default();
```

## Configuration

```yaml
static-credstore-plugin:
  config:
    vendor: "constructorfabric"   # GTS vendor name (default: "constructorfabric")
    priority: 100                 # Plugin priority, lower = higher (default: 100)
```

Both keys are GTS-instance registration input; there is nothing else to
configure. Unknown keys — including the former `secrets:` list — fail
validation (`deny_unknown_fields`).

## Contract

| Method | Behaviour |
|---|---|
| `put(ctx, key, value)` | Stores a new immutable version under `key`; returns its version (`"1"`, `"2"`, ...). |
| `get(ctx, key, version)` | Returns the bytes of that version, or `None`. |
| `delete_key(ctx, key)` | Removes the key with all versions; a missing key is `Ok(())`. |
| `supports_destroy()` | `true`. |
| `destroy(ctx, key, selector)` | `Below(v)` / `Exactly(v)`; idempotent. |

## Architecture

```text
gear.rs            ToolKit gear — initialization and GTS/ClientHub registration
config.rs          Config model (vendor, priority)
domain/
  service.rs       In-memory (tenant_id, record_id) → versions store
  client.rs        CredStorePluginClientV2 adapter
  mod.rs           Domain exports
```

### Init sequence

1. Load `StaticCredStorePluginConfig` from module config
2. Register GTS plugin instance in types-registry
3. Store `Arc<Service>` in module state
4. Register `CredStorePluginClientV2` scoped client in `ClientHub`

## Testing

```bash
cargo test -p cf-gears-static-credstore-plugin
```

The test suite covers:

- `put`/`get`/`delete_key` round-trips, per-key version counters and tenant isolation
- `destroy` (`Below`/`Exactly`) and idempotent `delete_key`/`destroy`
- Config validation (unknown keys, including a legacy `secrets:` block, are rejected)
- The `CredStorePluginClientV2` trait impl

## License

Apache-2.0
