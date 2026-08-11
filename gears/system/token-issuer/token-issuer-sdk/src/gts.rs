//! GTS (Global Type System) declarations for the token-issuer SDK.

use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.token_issuer.signing_plugin.v1~"),
    description = "Token-issuer signing plugin specification",
    properties = "",
)]
pub struct SigningPluginSpecV1;
