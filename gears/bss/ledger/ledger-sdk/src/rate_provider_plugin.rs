//! GTS schema for the FX rate-provider *composite* plugin the ledger discovers.
//!
//! The external `bss-rate-provider` core gear registers a `PluginV1` instance of
//! this type in the types-registry (and a scoped [`crate::RateProviderV1`] client
//! keyed by the instance id). The ledger's `RateSyncJob` discovers it each tick
//! (`list_instances` + vendor/priority select + `get_scoped`), falling back to
//! [`crate::UnconfiguredRateProviderV1`] when none is registered.

use toolkit_gts::{gts_type_schema, PluginV1};

/// GTS type for the composite rate-provider plugin instance.
///
/// Instance ids look like
/// `gts.cf.toolkit.plugins.plugin.v1~cf.bss.rate_provider.plugin.v1~<vendor>.<segment>`.
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.bss.rate_provider.plugin.v1~"),
    description = "FX rate-provider composite plugin specification",
    properties = "",
)]
pub struct RateProviderPluginSpecV1;
