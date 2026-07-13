# Typed ClientHub and Plugin Architecture

## ClientHub Overview

The **ClientHub** provides type-safe client resolution for inter-gear communication. It supports both in-process and remote clients:

- **In-process clients** — direct function calls within the same process
- **Remote clients** — gRPC clients for OoP gears (resolved via DirectoryClient)
- **Scoped clients** — multiple implementations of the same interface keyed by scope (for plugins)

### Client types

- **`*-sdk` crate** defines the trait & types exposed to other gears.
- **Gear crate** implements a local adapter that implements the SDK trait for in-process communication.
- **gRPC clients** implement the same SDK trait for remote communication.
- Consumers resolve the typed client from ClientHub by interface type (+ optional scope).

## In-Process vs Remote Clients

| Aspect       | In-Process              | Remote (OoP)               |
|--------------|-------------------------|----------------------------|
| Transport    | Direct call             | gRPC                       |
| Latency      | Nanoseconds             | Milliseconds               |
| Isolation    | Shared process          | Separate process           |
| Contract     | Trait in `*-sdk/` crate | Trait in `*-sdk/` crate    |
| Registration | `ClientHub::register()` | DirectoryClient + gRPC client + `ClientHub::register()` |

## Publish in `init` (provider gear)

```rust
#[async_trait::async_trait]
impl Gear for MyGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg = ctx.gear_config::<crate::config::Config>();
        let svc = std::sync::Arc::new(domain::service::MyService::new(ctx.db.clone(), cfg));
        self.service.store(Some(svc.clone()));

        let api: std::sync::Arc<dyn my_gear_sdk::MyGearApi> =
            std::sync::Arc::new(crate::domain::local_client::MyGearLocalClient::new(svc));

        ctx.client_hub().register::<dyn my_gear_sdk::MyGearApi>(api);
        Ok(())
    }
}
```

## Consume (consumer gear)

```rust
let api = ctx.client_hub().get::<dyn my_gear_sdk::MyGearApi>()?;
```

## Scoped Clients (for Plugins)

For plugin-like scenarios where multiple implementations of the same interface coexist, use scoped clients:

```rust
use toolkit::client_hub::ClientScope;
use toolkit_gts::gts_id;

// Plugin registers with a scope (e.g., GTS instance ID)
let scope = ClientScope::gts_id(gts_id!("cf.toolkit.plugins.plugin.v1~vendor.pkg.my_gear.plugin.v1~acme.test._.plugin.v1"));
ctx.client_hub().register_scoped::<dyn MyPluginClient>(scope, plugin_impl);

// Main gear resolves the selected plugin
let scope = ClientScope::gts_id(&selected_instance_id);
let plugin = ctx.client_hub().get_scoped::<dyn MyPluginClient>(&scope)?;
```

### Key points

- Scoped clients are independent from global (unscoped) clients
- Use `ClientScope::gts_id()` for GTS-based plugin IDs
- See `docs/TOOLKIT_PLUGINS.md` for the complete plugin architecture guide

## Plugin Architecture Overview

ToolKit’s plugin system enables **gear + plugins** patterns where:

- **Main gear** exposes the public API and registers plugin **schemas** (GTS type definitions)
- **Plugin gears** register their **instances** (metadata + scoped client)
- The main gear resolves plugins via **scoped ClientHub** using GTS instance IDs

> **Plugin Isolation Rule:** Regular gears **cannot** depend on or consume plugin gears directly. All plugin functionality must be accessed through the main gear’s public API (`hub.get::<dyn MyGearClient>()`). This ensures plugin implementations remain swappable and decoupled.

```text
┌────────────────────────────────────────────────────────────────────┐
│                            MAIN MODULE                              │
│  • Exposes public API (REST + ClientHub)                           │
│  • Selects plugin based on config/context                          │
│  • Routes calls to selected plugin                                 │
└───────────────────────────────┬────────────────────────────────────┘
                                │ hub.get_scoped::<dyn PluginClient>(&scope)
                ┌───────────────┼───────────────┐
                │               │               │
                ▼               ▼               ▼
        ┌───────────┐   ┌───────────┐   ┌───────────┐
        │ Plugin A  │   │ Plugin B  │   │ Plugin C  │
        └───────────┘   └───────────┘   └───────────┘
```

### When to Use Plugins

Use the plugin pattern when:
- Multiple implementations of the same interface need to coexist
- The implementation is selected at runtime based on configuration or context
- You want vendor-specific or tenant-specific behavior
- New implementations should be addable without modifying the main gear

**Examples:** Authentication providers (OAuth2, SAML, LDAP), LLM providers (OpenAI, Anthropic), file parsers, tenant resolvers, search engines.

### Crate Structure

```text
gears/<gear-name>/
├── <gear>-sdk/                 # SDK: API traits, models, errors, GTS types
│   └── src/
│       ├── api.rs              # Public API trait (for consumers)
│       ├── plugin_api.rs       # Plugin API trait (implemented by plugins)
│       ├── models.rs           # Shared models
│       ├── error.rs            # Errors
│       └── gts.rs              # GTS schema for plugin instances
│
├── <gear>/                     # Main gear with plugin discovery
│   └── src/
│       ├── gear.rs             # Gear with plugin discovery
│       ├── config.rs           # Gear config (e.g., vendor selector)
│       └── domain/
│           ├── service.rs      # Plugin resolution and delegation
│           └── local_client.rs # Public client adapter
│
└── plugins/                    # Plugin implementations
    ├── <vendor-a>-plugin/
    │   └── src/
    │       ├── gear.rs         # Registers GTS instance + scoped client
    │       └── config.rs       # Plugin config (vendor + priority)
    └── <vendor-b>-plugin/
```

### Flow

1. **Main gear** registers plugin schema with GTS (`gts_schema_with_refs_as_string()`)
2. **Plugin** starts, registers instance + scoped client under `ClientScope::gts_id(instance_id)`
3. **Main gear** resolves plugin by querying types-registry and using `choose_plugin_instance`
4. **Requests** flow through the scoped client to the plugin implementation

### Step 1: Define Two API Traits in SDK

```rust
// <gear>-sdk/src/api.rs — Public API, consumed by other gears
#[async_trait]
pub trait MyGearClient: Send + Sync {
    async fn do_work(&self, ctx: &SecurityContext, input: Input) -> Result<Output, MyError>;
}

// <gear>-sdk/src/plugin_api.rs — Plugin API, implemented by plugins
#[async_trait]
pub trait MyGearPluginClient: Send + Sync {
    async fn do_work(&self, ctx: &SecurityContext, input: Input) -> Result<Output, MyError>;
}
```

**Why two traits?**
- The public trait is the stable contract for consumers — they don’t know or care which plugin is used
- The plugin trait may have different method signatures or additional methods
- Consumers call `hub.get::<dyn MyGearClient>()` — the main gear handles plugin routing internally

### Step 2: Define GTS Schema for Plugin Instances

```rust
// <gear>-sdk/src/gts.rs
use toolkit_gts::gts_type_schema;
use toolkit_gts::PluginV1;

#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.y.my_gear.plugin.v1~"),
    description = "My Gear plugin specification",
    properties = ""
)]
pub struct MyGearPluginSpecV1;
```

### Step 3: Main Gear Registers Schema + Public Client

```rust
// <gear>/src/gear.rs
#[toolkit::gear(
    name = "my-gear",
    deps = ["types-registry"],  // depends on types-registry, NOT on plugin crates
    capabilities = [rest]
)]
pub struct MyGear { /* ... */ }

#[async_trait]
impl Gear for MyGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: GearConfig = ctx.config_or_default()?;

        // Register plugin SCHEMA in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let schema_str = MyGearPluginSpecV1::gts_schema_with_refs_as_string();
        let schema_json: serde_json::Value = serde_json::from_str(&schema_str)?;
        let _ = registry.register(vec![schema_json]).await?;

        let svc = Arc::new(Service::new(ctx.client_hub(), cfg.vendor));

        // Register PUBLIC client (no scope) for other gears
        let api: Arc<dyn MyGearClient> = Arc::new(LocalClient::new(svc.clone()));
        ctx.client_hub().register::<dyn MyGearClient>(api);

        Ok(())
    }
}
```

### Step 4: Plugin Registers Instance + Scoped Client

```rust
// plugins/<vendor>-plugin/src/gear.rs
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;

#[toolkit::gear(
    name = "vendor-a-plugin",
    deps = ["types-registry"],
)]
pub struct VendorAPlugin { /* ... */ }

#[async_trait]
impl Gear for VendorAPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: PluginConfig = ctx.config_or_default()?;

        let instance_id = MyGearPluginSpecV1::gts_make_instance_id("vendor_a.pkg.my_gear.plugin.v1");

        // Register INSTANCE in types-registry (schema is already registered by main gear)
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = PluginV1::<MyGearPluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: MyGearPluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;
        let _ = registry.register(vec![instance_json]).await?;

        let service = Arc::new(Service::new());

        // Register SCOPED client with GTS instance ID
        let api: Arc<dyn MyGearPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn MyGearPluginClient>(ClientScope::gts_id(&instance_id), api);

        Ok(())
    }
}
```

Use `ctx.config()` only when startup must fail if `gears.<name>.config` is absent. For
gears and plugins with defaults, use `ctx.config_or_default()` instead.

### Step 5: Main Gear Resolves Plugin

Use `choose_plugin_instance` from `toolkit::plugins` — do **not** copy selection logic into each gear.

```rust
// <gear>/src/domain/service.rs
use toolkit::plugins::{GtsPluginSelector, choose_plugin_instance};

impl Service {
    async fn get_plugin(&self) -> Result<Arc<dyn MyGearPluginClient>, DomainError> {
        let instance_id = self.selector.get_or_init(|| self.resolve_plugin()).await?;
        let scope = ClientScope::gts_id(instance_id.as_ref());
        self.hub
            .get_scoped::<dyn MyGearPluginClient>(&scope)
            .map_err(|_| DomainError::PluginClientNotFound {
                gts_id: instance_id.to_string(),
            })
    }

    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        let registry = self.hub.get::<dyn TypesRegistryClient>()?;
        let plugin_type_id = MyGearPluginSpecV1::gts_type_id().clone();
        let instances = registry
            .list(ListQuery::new()
                .with_pattern(format!("{plugin_type_id}*"))
                .with_is_type(false))
            .await?;

        // Shared selection: filters by vendor, picks lowest priority
        Ok(choose_plugin_instance::<MyGearPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.gts_id.as_str(), &e.content)),
        )?)
    }
}
```

Add `From<ChoosePluginError> for DomainError` in `domain/error.rs`:

```rust
impl From<toolkit::plugins::ChoosePluginError> for DomainError {
    fn from(e: toolkit::plugins::ChoosePluginError) -> Self {
        match e {
            toolkit::plugins::ChoosePluginError::InvalidPluginInstance { gts_id, reason } => {
                Self::InvalidPluginInstance { gts_id, reason }
            }
            toolkit::plugins::ChoosePluginError::PluginNotFound { vendor } => {
                Self::PluginNotFound { vendor }
            }
        }
    }
}
```

### Gear Dependencies

```rust
// Main gear depends on types-registry, but NOT on plugin crates
#[toolkit::gear(name = "my-gear", deps = ["types-registry"], capabilities = [rest])]
pub struct MyGear { /* ... */ }

// Each plugin depends on types-registry
#[toolkit::gear(name = "vendor-a-plugin", deps = ["types-registry"])]
pub struct VendorAPlugin { /* ... */ }
```

### Plugin Configuration

```yaml
gears:
  my-gear:
    config:
      vendor: "VendorA"  # Select VendorA plugin

  vendor-a-plugin:
    config:
      vendor: "VendorA"
      priority: 10

  vendor-b-plugin:
    config:
      vendor: "VendorB"
      priority: 20
```

### Plugin Checklist

- [ ] SDK defines both `<Gear>Client` trait (public) and `<Gear>PluginClient` trait (plugins)
- [ ] SDK defines GTS schema type with `#[gts_type_schema]` (from `toolkit_gts`)
- [ ] Main gear depends on `types-registry` but MUST NOT depend on plugin crates
- [ ] Main gear registers plugin **schema** using `gts_type_schema_with_refs_as_string()`
- [ ] Main gear registers public client WITHOUT scope
- [ ] Main gear resolves plugin lazily (after types-registry is ready)
- [ ] Each plugin depends on `types-registry`
- [ ] Each plugin registers its **instance** (not schema)
- [ ] Each plugin registers scoped client with `ClientScope::gts_id(&instance_id)`
- [ ] Plugin selection uses priority for tiebreaking
- [ ] Use `choose_plugin_instance` from `toolkit::plugins` for selection logic

### Reference Example

Study the production implementation in `gears/system/tenant-resolver/`:
- `tenant-resolver-sdk/` — SDK with `TenantResolverClient`, `TenantResolverPluginClient`, and GTS spec
- `tenant-resolver/` — Main gear that registers schema and selects plugin by vendor config
- `plugins/static-tr-plugin/` — Config-based plugin implementation
- `plugins/single-tenant-tr-plugin/` — Zero-config plugin implementation

Also see: `examples/plugin-gears/tenant-resolver/`

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
    Err(ClientHubError::NotFound { type_key }) => { /* handle missing client */ }
    Err(ClientHubError::TypeMismatch { type_key }) => { /* handle type mismatch */ }
}
```

## Best practices

- **SDK traits**: Define in `*-sdk` crate, require `Send + Sync + 'static`.
- **Local adapters**: Implement SDK trait in gear crate, register in `init()`.
- **gRPC clients**: Use `toolkit_transport_grpc::client` utilities (`connect_with_stack`, `connect_with_retry`).
- **Plugins**: Use `ClientScope::gts_id()` for instance IDs; register scoped clients.
- **Error handling**: Convert domain errors to SDK errors and to `Problem` for REST.
- **Testing**: Register mock clients in tests using the same trait.

## Quick checklist

- [ ] Define SDK trait with `async_trait` and `SecurityContext` first param.
- [ ] Implement local adapter in gear crate.
- [ ] Register client in `init()`: `ctx.client_hub().register::<dyn Trait>(api)`.
- [ ] Consume client: `ctx.client_hub().get::<dyn Trait>()?`.
- [ ] For plugins: use `ClientScope::gts_id()` and `register_scoped()`.
- [ ] For OoP: use gRPC client utilities and register both local and remote clients.
