//! GTS schema definitions for `AuthZ` resolver plugins.

use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

/// GTS type definition for `AuthZ` resolver plugin instances.
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.authz_resolver.plugin.v1~
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.authz_resolver.plugin.v1~"),
    description = "AuthZ Resolver plugin specification",
    properties = "",
)]
pub struct AuthZResolverPluginSpecV1;
