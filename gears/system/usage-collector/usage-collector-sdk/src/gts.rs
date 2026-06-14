//! GTS plugin spec for usage-collector storage-plugin discovery and binding.

use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

/// GTS plugin specification for usage-collector storage backends.
///
/// Concrete storage plugins publish a `PluginV1<UsageCollectorPluginSpecV1>`
/// instance to `types-registry` and register their scoped
/// [`crate::plugin_api::UsageCollectorPluginV1`] client in `ClientHub` under
/// `ClientScope::gts_id(&instance_id)`. The empty `properties = ""` is
/// intentional — instance metadata (`vendor`, `priority`) is carried by the
/// `PluginV1<P>` base type.
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-contract-storage-plugin:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-contract-gts-registry:p1
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = "gts.cf.toolkit.plugins.plugin.v1~cf.core.uc.plugin.v1~",
    description = "Usage Collector plugin specification",
    properties = "",
)]
pub struct UsageCollectorPluginSpecV1;
