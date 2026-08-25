#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! `bss-rate-provider-http-json-plugin` — a generic GET-JSON source plugin.
//!
//! Fetches a plain REST rate feed, applies a configured field `mapping`, and
//! registers itself as a [`bss_ledger_sdk::RateProviderV1`] source plugin
//! (`PluginV1<RateProviderSourcePluginSpecV1>` instance + scoped `ClientHub`
//! entry). v1 scope (design O-11): single literal base currency, simple dotted
//! field paths, `none`/`bearer`/`header-key` auth.

pub mod config;
pub mod gear;
// `pub`, not `pub(crate)`: `tests/http_json_integration.rs` constructs
// `HttpJsonRateProvider` directly.
pub mod infra;

pub use gear::HttpJsonRateProviderPlugin;
