# Typed ClientHub and Plugin Architecture

## ClientHub Overview

The **ClientHub** (`libs/toolkit/src/client_hub.rs`) resolves typed clients for inter-gear calls:

- **In-process clients** — a local adapter registered by the provider gear
- **Remote clients** — REST or gRPC clients that resolve the provider through the directory
- **Scoped clients** — several implementations of one interface keyed by a `ClientScope` (plugins)

### Client types

- **`*-sdk` crate** defines the trait and types exposed to other gears.
- **Gear crate** implements a local adapter of the SDK trait for in-process calls.
- **Remote clients** implement the same SDK trait.
- Consumers resolve the client by interface type (plus a scope for plugins).

## In-Process vs Remote Clients

| Aspect       | In-Process              | Remote (OoP)               |
|--------------|-------------------------|----------------------------|
| Transport    | Direct call             | REST or gRPC               |
| Isolation    | Shared process          | Separate process           |
| Contract     | Trait in `*-sdk/` crate | Trait in `*-sdk/` crate    |
| Registration | `ClientHub::register()` | `#[toolkit::consumes]` registers a resolving client unless a local one exists; see [09_oop_grpc_sdk_pattern.md](09_oop_grpc_sdk_pattern.md) |

`get` returns, in order: a local registration, the calling gear's remote proxy, a process-wide remote
proxy. A local registration always wins over a proxy.

## Publish in `init` (provider gear)

```rust
#[async_trait::async_trait]
impl Gear for MyGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: crate::config::Config = ctx.config_or_default()?;
        let db = ctx.db_required()?;
        let svc = Arc::new(domain::service::MyService::new(db, cfg));
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let api: Arc<dyn my_gear_sdk::MyGearApi> =
            Arc::new(crate::domain::local_client::MyGearLocalClient::new(svc));
        ctx.client_hub().register::<dyn my_gear_sdk::MyGearApi>(api);
        Ok(())
    }
}
```

`GearCtx` (`libs/toolkit/src/context.rs`) reads the gear's `gears.<name>.config` section with
`config()` (section required), `config_or_default()`, and `config_expanded()` /
`config_expanded_or_default()` (also expand `${VAR}` in fields marked `#[expand_vars]`). `db()` returns
`Option<DBProvider>`; `db_required()` returns an error when the gear has no database.

## Consume (consumer gear)

```rust
let api = ctx.client_hub().get::<dyn my_gear_sdk::MyGearApi>()?;
```

## Scoped Clients (for Plugins)

```rust
use toolkit::client_hub::ClientScope;

// A plugin registers under its GTS instance ID
ctx.client_hub()
    .register_scoped::<dyn MyPluginClient>(ClientScope::gts_id(&instance_id), plugin_impl);

// The main gear resolves the selected plugin
let scope = ClientScope::gts_id(&selected_instance_id);
let plugin = ctx.client_hub().get_scoped::<dyn MyPluginClient>(&scope)?;
```

Scoped and unscoped registrations are independent. Plugins normally register through
`toolkit::plugins::bind_plugin_instances` rather than calling `register_scoped` directly; see
[TOOLKIT_PLUGINS.md](../TOOLKIT_PLUGINS.md).

## Plugins

A plugin system is a main gear with a public API plus plugin gears that implement a separate plugin
trait. Each plugin publishes a GTS instance of the main gear's plugin spec and binds its implementation
as a scoped client under `ClientScope::gts_id(<instance id>)`. The main gear lists the instances from
types-registry, selects one by vendor and priority (`toolkit::plugins`) and calls it through
`get_scoped` / `try_get_scoped`. Regular gears never consume plugin gears directly.

The complete guide, based on `gears/system/tenant-resolver/`, is [TOOLKIT_PLUGINS.md](../TOOLKIT_PLUGINS.md).

## ClientHub API Reference

### Registration

```rust
// Global (unscoped) client
ctx.client_hub().register::<dyn MyGearApi>(api);

// Scoped client (plugins)
ctx.client_hub().register_scoped::<dyn MyPluginClient>(scope, plugin);
```

### Resolution

```rust
// Global client
let api = ctx.client_hub().get::<dyn MyGearApi>()?;

// Try global client (returns None if not found)
let api = ctx.client_hub().try_get::<dyn MyGearApi>();

// Scoped client
let plugin = ctx.client_hub().get_scoped::<dyn MyPluginClient>(&scope)?;

// Try scoped client (returns None if not found)
let plugin = ctx.client_hub().try_get_scoped::<dyn MyPluginClient>(&scope);
```

### Removal

```rust
// Remove global client
let removed = ctx.client_hub().remove::<dyn MyGearApi>();

// Remove scoped client
let removed = ctx.client_hub().remove_scoped::<dyn MyPluginClient>(&scope);
```

## Error handling

```rust
use toolkit::client_hub::ClientHubError;

match ctx.client_hub().get::<dyn MyGearApi>() {
    Ok(api) => { /* use api */ }
    Err(ClientHubError::NotFound { type_key }) => { /* nothing registered */ }
    Err(ClientHubError::TypeMismatch { type_key }) => { /* stored value has another type */ }
    // Returned only by `get_scoped`
    Err(ClientHubError::ScopedNotFound { type_key, scope }) => { /* ... */ }
    Err(ClientHubError::ScopedTypeMismatch { type_key, scope }) => { /* ... */ }
}
```

In `init`, propagate with `?` (`ClientHubError` converts into `anyhow::Error`). In domain code, map it to
the gear's domain error; REST handlers return `CanonicalError` (see
[05_errors_rfc9457.md](05_errors_rfc9457.md)).

## Quick checklist

- [ ] Define the SDK trait with `async_trait`, `Send + Sync`, and the caller's `SecurityContext` as the first parameter (most SDKs take `&SecurityContext`; users-info takes it by value).
- [ ] Implement a local adapter in the gear crate.
- [ ] Register it in `init()`: `ctx.client_hub().register::<dyn Trait>(api)`.
- [ ] Consume it: `ctx.client_hub().get::<dyn Trait>()?`, or `#[toolkit::consumes(contract = ..., from = "...")]` when the provider may run in another process.
- [ ] Hand-written gRPC clients connect with `toolkit_transport_grpc::client::connect_with_stack(uri, &GrpcClientConfig)` or `connect_with_retry(uri, &GrpcClientConfig)`.
- [ ] Plugins: follow [TOOLKIT_PLUGINS.md](../TOOLKIT_PLUGINS.md).
- [ ] Tests: register mock implementations of the same trait in a `ClientHub::new()`.
