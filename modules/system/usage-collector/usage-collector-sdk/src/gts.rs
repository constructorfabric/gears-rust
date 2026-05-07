//! GTS schema for usage-collector storage plugin instances.

use gts_macros::struct_to_gts_schema;
use modkit::gts::BaseModkitPluginV1;

/// GTS type for storage backend plugin instances registered with types-registry.
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = BaseModkitPluginV1,
    schema_id = "gts.cf.core.modkit.plugin.v1~cf.core.usage_collector.storage_plugin.v1~",
    description = "Usage Collector storage plugin specification",
    properties = ""
)]
pub struct UsageCollectorStoragePluginSpecV1;
