#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! `bss-rate-provider-sdk` — the FX rate-provider source-plugin contract.
//!
//! Defines the GTS plugin spec [`gts::RateProviderSourcePluginSpecV1`] that a
//! rate-provider *source* plugin (ECB, http-json, …) registers as a `PluginV1`
//! instance in the types-registry, and the shared utilities every rate-provider
//! plugin gear needs: exact-decimal [`conversion`], HTTP-to-domain [`error`]
//! mapping, the [`metrics`] fetch-metrics seam, a shared [`http_client`]
//! constructor, an instrumented [`fetch`] helper, and the shared
//! [`registration`] sequence every plugin gear's `init()` performs.
//!
//! A source plugin registers `register_scoped::<dyn bss_ledger_sdk::RateProviderV1>`
//! keyed by its GTS instance id; the core `bss-rate-provider` gear discovers the
//! instances, orders them by `priority`, and composes them.

pub mod conversion;
pub mod error;
pub mod fetch;
pub mod gts;
pub mod http_client;
pub mod metrics;
pub mod registration;

pub use gts::RateProviderSourcePluginSpecV1;
