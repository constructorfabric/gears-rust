# ToolKit Plugins

A plugin system lets several implementations of one interface run side by side. A **main gear** owns
the public API and delegates each call to one **plugin gear**, selected at runtime from the plugin
instances published in types-registry.

The reference implementation is `gears/system/tenant-resolver/`; the snippets below are taken from it:

| Crate | Role |
|-------|------|
| `tenant-resolver-sdk/` | Public trait `TenantResolverClient`, plugin trait `TenantResolverPluginClient`, GTS spec `TenantResolverPluginSpecV1` |
| `tenant-resolver/` | Main gear: publishes the spec schema, selects a plugin by vendor, registers the public client |
| `plugins/static-tr-plugin/` | Plugin with tenants from configuration |
| `plugins/single-tenant-tr-plugin/` | Plugin with no configuration |
| `plugins/rg-tr-plugin/` | Plugin backed by the resource-group gear (`deps = [resource_group]`) |

> [!IMPORTANT]
> **Plugin isolation rule.** Regular gears do not depend on or consume plugin gears. They call the
> main gear's public API (`hub.get::<dyn TenantResolverClient>()`), so plugins stay swappable.

For the ClientHub API itself (registration, scoped clients, errors) see
[03_clienthub_and_plugins.md](./toolkit_unified_system/03_clienthub_and_plugins.md).

## How it fits together

1. The SDK declares the plugin spec, a GTS Type Schema derived from `PluginV1`
   (`gts.cf.toolkit.plugins.plugin.v1~`, provided by `toolkit_gts`).
2. The main gear publishes that schema with `gts_publication(crates = [<sdk>])`.
3. Each plugin publishes one instance of the spec with `gts_publication(entities = ...)`, built from its
   configuration (`vendor`, `priority`).
4. The runtime publishes every manifest; see [Startup order](#startup-order) for when.
5. In `init`, each plugin binds its plugin-trait implementation as a scoped client under
   `ClientScope::gts_id(<instance id>)` (`toolkit::plugins::bind_plugin_instances`).
6. On the first API call, the main gear lists the spec's instances from types-registry, picks one with
   `toolkit::plugins::choose_registered_plugin_instance` and caches the choice in a `GtsPluginSelector`.

## Crate layout

```text
gears/system/tenant-resolver/
├── tenant-resolver-sdk/src/
│   ├── lib.rs           # declare_gts_inventory!() + re-exports
│   ├── api.rs           # TenantResolverClient (public)
│   ├── plugin_api.rs    # TenantResolverPluginClient (plugins)
│   ├── models.rs, error.rs
│   └── gts.rs           # TenantResolverPluginSpecV1
├── tenant-resolver/src/
│   ├── gear.rs          # consumes + gts_publication(crates = ...), registers the public client
│   ├── config.rs        # vendor
│   └── domain/          # service.rs (selection, delegation), local_client.rs, error.rs
└── plugins/static-tr-plugin/src/
    ├── gear.rs          # gts_publication(entities = ...), binds the scoped client
    ├── config.rs        # vendor, priority, plugin data
    └── domain/          # Service implements TenantResolverPluginClient
```

## SDK

The SDK has two traits. Consumers see only the public one; the plugin trait may differ in shape and is
called only by the main gear.

```rust
// api.rs
#[async_trait]
pub trait TenantResolverClient: Send + Sync {
    async fn get_tenant(&self, ctx: &SecurityContext, id: TenantId)
        -> Result<TenantInfo, TenantResolverError>;
    // ...
}

// plugin_api.rs
#[async_trait]
pub trait TenantResolverPluginClient: Send + Sync {
    async fn get_tenant(&self, ctx: &SecurityContext, id: TenantId)
        -> Result<TenantInfo, TenantResolverError>;
    // ...
}
```

`TenantResolverError` is an older pattern: new plugin SDKs return `CanonicalError` from both traits, per
[ADR 0005](arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md).

The plugin spec is a unit struct declared with the crate-local collector macro
`toolkit_gts::local::gts_type_schema`. It must derive `Default` so `PluginV1::<P>::build_registration`
can construct it.

```rust
// gts.rs
use toolkit::gts::PluginV1;
use toolkit_gts::local::gts_type_schema;

#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.tenant_resolver.plugin.v1~"),
    description = "Tenant Resolver plugin specification",
    properties = "",
)]
pub struct TenantResolverPluginSpecV1;
```

```rust
// lib.rs: the collector that `gts_publication(crates = [tenant_resolver_sdk])` reads
toolkit_gts::declare_gts_inventory!();
```

A crate listed in `crates = [...]` must use the `toolkit_gts::local::*` macros throughout; leftover
crate-root declarations fail manifest collection (see `libs/toolkit-gts/src/local.rs`).

Instance IDs append one segment to the spec's type ID:

```text
gts.cf.toolkit.plugins.plugin.v1~cf.core.tenant_resolver.plugin.v1~cf.builtin.static_tenant_resolver.plugin.v1
└──── PluginV1 ────────────────┘└──── spec ──────────────────────┘└──── instance segment ─────────────────┘
```

The SDK depends on `toolkit`, `toolkit-gts` and `gts`.

## Main gear

```rust
// tenant-resolver/src/gear.rs
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::gear(
    name = "tenant-resolver",
    gts_publication(crates = [tenant_resolver_sdk]),
)]
pub(crate) struct TenantResolver {
    service: OnceLock<Arc<Service>>,
}

#[async_trait]
impl Gear for TenantResolver {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: TenantResolverConfig = ctx.config_or_default()?;
        let hub = ctx.client_hub();
        let svc = Arc::new(Service::new(hub, cfg.vendor));
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let api: Arc<dyn TenantResolverClient> = Arc::new(TenantResolverLocalClient::new(svc));
        ctx.client_hub().register::<dyn TenantResolverClient>(api);
        Ok(())
    }
}
```

- `#[toolkit::consumes]` wires `dyn PlatformTypesRegistryApi`: the registry's local client when it
  runs in this process, a resolving REST client otherwise. It adds no startup-order dependency.
- `gts_publication(crates = [...])` publishes the SDK's declarations before application `init`. The
  default publisher adapter is `types_registry_sdk::publication`, so the gear crate depends on
  `types-registry-sdk` with the `rest-client` feature.
- The main gear has no `deps` on plugin crates.

The domain service resolves the plugin lazily and caches the selected instance ID
(`tenant-resolver/src/domain/service.rs`, trimmed):

```rust
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_registered_plugin_instance};
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

pub struct Service {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
}

impl Service {
    async fn get_plugin(&self) -> Result<Arc<dyn TenantResolverPluginClient>, DomainError> {
        let instance_id = self
            .selector
            .get_or_init_with_timeout(
                DISCOVERY_BUDGET,
                || self.resolve_plugin(),
                DomainError::TypesRegistryUnavailable,
            )
            .await?;
        let scope = ClientScope::gts_id(instance_id.as_ref());
        self.hub
            .try_get_scoped::<dyn TenantResolverPluginClient>(&scope)
            .ok_or_else(|| DomainError::PluginUnavailable {
                gts_id: instance_id.to_string(),
                reason: "client not registered yet".into(),
            })
    }

    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        let registry = self
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;

        let plugin_type_id = TenantResolverPluginSpecV1::gts_type_id();
        let pattern = GtsIdPattern::try_new(plugin_type_id.as_ref())
            .map_err(|e| DomainError::Internal(format!("plugin discovery pattern: {e}")))?;
        // Follows every page; fails on any page error.
        let instances = registry
            .list_instances(
                &PlatformSecurityContext::outbound_marker(),
                ListEntitiesRequest::matching(pattern),
            )
            .await
            .map_err(registry_failure)?;

        Ok(choose_registered_plugin_instance::<
            TenantResolverPluginSpecV1,
            dyn TenantResolverPluginClient,
        >(
            &self.hub,
            &self.vendor,
            instances
                .iter()
                .map(|instance| (instance.id.as_ref(), instance.content.as_ref())),
        )?)
    }
}
```

`registry_failure` (in the same file) maps registry errors to domain errors, using
`toolkit::plugins::is_discovery_outage` to tell a retryable outage from a fault.

Selection rules (`libs/toolkit/src/plugins/mod.rs`):

- `choose_plugin_instance::<P>` decodes every listed entry as `PluginV1<P>`, whatever its vendor, and
  fails with `ChoosePluginError::InvalidPluginInstance` on the first entry that does not decode or whose
  `id` differs from the listed ID. The vendor filter runs only after that check, so one malformed
  entry of any vendor fails the whole selection. Among entries whose `vendor` matches it returns the
  one with the **lowest** `priority` (the first wins a tie), or `PluginNotFound` if none matches.
- `choose_registered_plugin_instance::<P, C>` first drops entries without a scoped `C` client in the
  hub, without validating them, then runs `choose_plugin_instance` on the rest; a registered entry
  listed without content fails with `InvalidPluginInstance`. The registry keeps instances published
  by earlier boots, including disabled plugins and other vendors' instances; this variant decodes and
  selects only among plugins running in this process, so stale catalogue entries cannot fail it.
  Prefer it.
- `GtsPluginSelector` runs the resolver at most once at a time and caches only a successful result,
  so a failed resolution is retried on the next call. `reset()` clears the cache.

Map `ChoosePluginError` into the domain error:

```rust
impl From<toolkit::plugins::ChoosePluginError> for DomainError {
    fn from(e: toolkit::plugins::ChoosePluginError) -> Self {
        match e {
            toolkit::plugins::ChoosePluginError::InvalidPluginInstance { gts_id, reason } => {
                Self::InvalidPluginInstance { gts_id, reason }
            }
            toolkit::plugins::ChoosePluginError::PluginNotFound { vendor, .. } => {
                Self::PluginNotFound { vendor }
            }
        }
    }
}
```

## Plugin gear

```rust
// plugins/static-tr-plugin/src/gear.rs
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use types_registry_sdk::GtsId;

const INSTANCE_SEGMENT: &str = "cf.builtin.static_tenant_resolver.plugin.v1";

#[toolkit::gear(
    name = "static-tr-plugin",
    gts_publication(entities = Self::plugin_instance),
)]
#[derive(Default)]
pub struct StaticTrPlugin {
    service: OnceLock<Arc<dyn TenantResolverPluginClient>>,
}

impl StaticTrPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: StaticTrPluginConfig = inputs.config_or_default()?;
        let (id, document) = PluginV1::<TenantResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(GtsId::try_new(id.as_ref())?, document)])
    }
}

#[async_trait]
impl Gear for StaticTrPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: StaticTrPluginConfig = ctx.config_or_default()?;
        let service: Arc<dyn TenantResolverPluginClient> = Arc::new(Service::from_config(&cfg)?);
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // The instance was admitted before `init`: bind the scoped client to it.
        bind_plugin_instances::<TenantResolverPluginSpecV1, _>(
            &ctx.client_hub(),
            ctx.gts_publication_manifest()?.documents(),
            &service,
        )
    }
}
```

- `entities` builds documents from configuration only (`GtsPublicationInputs`); it runs before `init`.
- `bind_plugin_instances` registers the client under `ClientScope::gts_id(id)` for every instance of the
  spec in the admitted manifest, and fails if there is none.
- The plugin crate depends on `types-registry-sdk` with the `rest-client` feature (default publisher
  adapter) and on the SDK. It does not depend on the main gear.

## Startup order

The runtime publishes every `gts_publication(...)` manifest and waits until all are admitted before it
runs any application `init`. Application gears then initialize in dependency order. The main gear does
not depend on its plugins, so their relative `init` order is not fixed; that is why selection is lazy
and skips instances without a bound client. A plugin declares `deps` only for gears it needs itself
(`rg-tr-plugin` uses `deps = [resource_group]`; `deps` takes crate identifiers).

Details: [startup sequence](./toolkit_unified_system/08_lifecycle_stateful_tasks.md#startup-sequence),
[publishing GTS documents](./toolkit_unified_system/08_lifecycle_stateful_tasks.md#publishing-gts-documents),
and the [SDK publication guide](../gears/system/types-registry/types-registry-sdk/README.md#publishing-from-a-gear)
(publication timeouts, retries, batch size under `gears.<gear>.config.gts_publication`).

## Configuration

The main gear selects by `vendor`; each plugin declares its `vendor` and `priority` (from
`config/quickstart.yaml`):

```yaml
gears:
  tenant-resolver:
    config:
      vendor: "constructorfabric"
  static-tr-plugin:
    config:
      vendor: "constructorfabric"
      priority: 20
      tenants: [ ... ]
```

Among registered instances with a matching vendor, the lowest `priority` wins; on a tie the first in
listing order wins.

## Testing

- **Selection.** `types_registry_sdk::testing_platform::MockTypesRegistry` (feature `test-util`) stands in
  for the registry: `install(&hub)`, then `seed(id, content)` instances built with
  `PluginV1::<P>::build_registration` and `register_scoped` the plugin clients that should count as
  running. See `tenant-resolver/src/domain/service_tests.rs`.
- **Plugin binding.** Build a `GearCtx` and attach a manifest with
  `GearCtx::with_gts_publication_manifest_for_test` (`toolkit` feature `test-support`). See
  `plugins/static-tr-plugin/src/gear_tests.rs`.

## Further reading

- [03_clienthub_and_plugins.md](./toolkit_unified_system/03_clienthub_and_plugins.md) — ClientHub API and scoped clients
- [08_lifecycle_stateful_tasks.md](./toolkit_unified_system/08_lifecycle_stateful_tasks.md) — runtime phases and GTS publication
- [04_rest_operation_builder.md](./toolkit_unified_system/04_rest_operation_builder.md) — REST endpoints on the main gear
- [ToolKit Unified System](./toolkit_unified_system/README.md)
