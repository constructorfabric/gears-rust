use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.credstore.plugin.v1~"),
    description = "CredStore plugin specification",
    properties = "",
)]
pub struct CredStorePluginSpecV1;
