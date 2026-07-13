//! GTS schema definitions for `AuthN` resolver plugins.
//!
//! This gear defines the GTS type for `AuthN` resolver plugin instances.
//! Plugins register instances of this type with the types-registry to be
//! discovered by the gateway.

use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

/// GTS type definition for `AuthN` resolver plugin instances.
///
/// Each plugin registers an instance of this type with its vendor-specific
/// instance ID. The gateway discovers plugins by querying types-registry
/// for instances matching this schema.
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.authn_resolver.plugin.v1~
/// ```
///
/// # Example
///
/// ```ignore
/// // Plugin generates its instance ID
/// let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(
///     "cf.builtin.static_authn_resolver.plugin.v1"
/// );
///
/// // Plugin creates instance data
/// let instance = PluginV1::<AuthNResolverPluginSpecV1> {
///     id: instance_id.clone(),
///     vendor: "constructorfabric".to_owned(),
///     priority: 100,
///     properties: AuthNResolverPluginSpecV1,
/// };
///
/// // Register with types-registry
/// registry.register(vec![serde_json::to_value(&instance)?]).await?;
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~"),
    description = "AuthN Resolver plugin specification",
    properties = "",
)]
pub struct AuthNResolverPluginSpecV1;
