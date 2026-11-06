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
/// ```
/// use authn_resolver_sdk::AuthNResolverPluginSpecV1;
/// use toolkit::gts::PluginV1;
///
/// // The plugin builds its instance ID and registration payload...
/// let (instance_id, payload) = PluginV1::<AuthNResolverPluginSpecV1>::build_registration(
///     "cf.builtin.static_authn_resolver.plugin.v1",
///     "constructorfabric",
///     100,
/// )?;
/// // ...and registers `payload` through the types-registry client
/// // (`registry.register(vec![payload]).await?`).
/// # let _ = (instance_id, payload);
/// # Ok::<(), serde_json::Error>(())
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
