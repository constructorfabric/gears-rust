//! GTS schema definitions for tenant resolver plugins.
//!
//! This gear defines the GTS type for tenant resolver plugin instances.
//! Plugins register instances of this type with the types-registry to be
//! discovered by the gateway.

use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

/// GTS type definition for tenant resolver plugin instances.
///
/// Each plugin registers an instance of this type with its vendor-specific
/// instance ID. The gateway discovers plugins by querying types-registry
/// for instances matching this schema.
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.tenant_resolver.plugin.v1~
/// ```
///
/// # Example
///
/// ```ignore
/// // Plugin generates its instance ID
/// let instance_id = TenantResolverPluginSpecV1::gts_make_instance_id(
///     "cf.builtin.static_tenant_resolver.plugin.v1"
/// );
///
/// // Plugin creates instance data
/// let instance = PluginV1::<TenantResolverPluginSpecV1> {
///     id: instance_id.clone(),
///     priority: 100,
///     properties: TenantResolverPluginSpecV1,
/// };
///
/// // Register with types-registry
/// registry.register(&ctx, vec![serde_json::to_value(&instance)?]).await?;
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.tenant_resolver.plugin.v1~"),
    description = "Tenant Resolver plugin specification",
    properties = "",
)]
pub struct TenantResolverPluginSpecV1;
