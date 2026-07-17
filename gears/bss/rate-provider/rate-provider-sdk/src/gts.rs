//! GTS schema for FX rate-provider *source* plugins.
//!
//! Each source plugin (ECB, http-json, …) registers a `PluginV1` instance of this
//! type with the types-registry; the core `bss-rate-provider` gear discovers them
//! by querying instances of this type and orders them by `priority` (lower =
//! tried first) to build the fallback composite.

use toolkit_gts::{gts_type_schema, PluginV1};

/// GTS type for a rate-provider source-plugin instance.
///
/// Instance ids look like
/// `gts.cf.toolkit.plugins.plugin.v1~cf.bss.rate_provider_source.plugin.v1~<vendor>.<segment>`.
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.bss.rate_provider_source.plugin.v1~"),
    description = "FX rate-provider source plugin specification",
    properties = "",
)]
pub struct RateProviderSourcePluginSpecV1;
