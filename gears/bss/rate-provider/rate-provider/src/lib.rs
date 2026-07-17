#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! `bss-rate-provider` — the core FX rate-provider gear.
//!
//! Discovers the registered source plugins (`bss-rate-provider-ecb-plugin`,
//! `bss-rate-provider-http-json-plugin`, …), orders them by `priority`, and
//! composes them with ordered fallback ([`composite::CompositeRateProvider`]).
//! It re-registers that composite as a `PluginV1` instance the ledger discovers
//! (a scoped [`bss_ledger_sdk::RateProviderV1`] keyed by its GTS instance id),
//! so a provider outage never touches the posting path — see `docs/DESIGN.md`.

pub mod composite;
pub mod config;
pub mod discovery;
pub mod module;
