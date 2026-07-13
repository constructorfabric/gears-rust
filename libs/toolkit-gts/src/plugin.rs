//! Base GTS Type for toolkit plugin instances.
//!
//! Every toolkit plugin registers a well-known GTS Instance of this Type Schema
//! in `types-registry` so consumers can discover and resolve plugins at runtime.
//! Gear-specific plugin specs (e.g. `AuthZResolverPluginSpecV1`,
//! `TenantResolverPluginSpecV1`) derive from this base type via the upstream
//! `#[gts_macros::struct_to_gts_schema(base = PluginV1, ...)]` pattern.
//!
//! ## Renamed from `BaseToolkitPluginV1`
//!
//! This struct was previously named `BaseToolkitPluginV1` (and briefly
//! `ToolkitPluginV1`) and lived in `libs/toolkit/src/gts/plugin.rs`. It now
//! lives here, in the `toolkit-gts` crate, alongside other platform-wide
//! GTS base types. A deprecated type alias
//! `toolkit::gts::BaseToolkitPluginV1<P>` is retained in the toolkit crate for
//! backward compatibility of external (published-crate) consumers.

use crate::gts_type_schema;
use gts::GtsInstanceId;

/// Base Type Schema for all toolkit plugin instances.
///
/// Plugins of any kind (resolvers, gateways, policy engines, etc.) register a
/// well-known GTS Instance of this type in `types-registry`. The `properties`
/// generic holds the plugin-kind-specific spec type (derived from this base
/// via GTS type chaining).
///
/// GTS Type Identifier: `gts.cf.toolkit.plugins.plugin.v1~`
#[derive(Debug)]
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~"),
    description = "Base toolkit plugin schema",
    properties = "id,vendor,priority,properties",
    base = true
)]
pub struct PluginV1<P: gts::GtsSchema> {
    /// Full GTS Instance Identifier for this plugin instance.
    pub id: GtsInstanceId,
    /// Vendor name, used for plugin selection when multiple of the same kind
    /// are registered.
    pub vendor: String,
    /// Selection priority — lower = higher priority.
    pub priority: i16,
    /// Plugin-kind-specific spec (derived type's properties).
    pub properties: P,
}

impl<P: gts::GtsSchema + gts::GtsSerialize + Default> PluginV1<P> {
    /// Assembles a `PluginV1<P>` from runtime config and returns the pair
    /// `(instance_id, json_payload)` ready for scoped-client registration
    /// and `TypesRegistryClient::register(...)`.
    ///
    /// `P` is constructed internally via `P::default()`. Derived unit-struct
    /// plugin specs declared through `#[toolkit_gts::gts_type_schema(base = PluginV1, ...)]`
    /// must `#[derive(Default)]` so the caller only specifies the type
    /// once — in the turbofish.
    ///
    /// Typical usage in a plugin gear's `init()`:
    ///
    /// ```ignore
    /// let (instance_id, payload) = PluginV1::<MyPluginSpecV1>::build_registration(
    ///     "<vendor>.<package>.<plugin_name>.v1",   // instance segment
    ///     cfg.vendor,                               // from YAML
    ///     cfg.priority,                             // from YAML
    /// )?;
    ///
    /// // Publish to types-registry:
    /// let results = registry.register(vec![payload]).await?;
    /// RegisterResult::ensure_all_ok(&results)?;
    ///
    /// // Register scoped client under the same instance id:
    /// ctx.client_hub().register_scoped::<dyn MyPluginClient>(
    ///     ClientScope::gts_id(&instance_id),
    ///     api,
    /// );
    /// ```
    ///
    /// The final instance id is `P::SCHEMA_ID + instance_segment` (e.g.
    /// `gts.cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~cf.builtin.static_authn_resolver.plugin.v1`).
    /// Registration via `types-registry-sdk` stays in the caller — this
    /// helper only builds the payload.
    ///
    /// For plugin specs that carry *real* data in `P` (rare — most specs
    /// are unit-struct markers), bypass this helper and construct
    /// `PluginV1 { id, vendor, priority, properties }` manually.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if serialisation fails (should not
    /// happen for well-formed derived specs produced by
    /// `#[struct_to_gts_schema(base = PluginV1, ...)]`).
    pub fn build_registration(
        instance_segment: &str,
        vendor: impl Into<String>,
        priority: i16,
    ) -> serde_json::Result<(GtsInstanceId, serde_json::Value)> {
        let id = GtsInstanceId::new(<P as gts::GtsSchema>::TYPE_ID, instance_segment);
        let instance = Self {
            id: id.clone(),
            vendor: vendor.into(),
            priority,
            properties: P::default(),
        };
        let payload = serde_json::to_value(&instance)?;
        Ok((id, payload))
    }
}
