#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! `bss-rate-provider-ecb-plugin` — the ECB source plugin.
//!
//! Fetches the ECB daily reference-rate feed, parses the EUR-based direct pairs,
//! and registers itself as a [`bss_ledger_sdk::RateProviderV1`] source plugin: a
//! `PluginV1<RateProviderSourcePluginSpecV1>` instance in the types-registry plus
//! a scoped `ClientHub` entry keyed by its GTS instance id. The core
//! `bss-rate-provider` gear discovers it and composes it with any other sources.

pub mod config;
pub mod gear;
pub mod infra;

pub use gear::EcbRateProviderPlugin;
