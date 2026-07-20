#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

//! `TimescaleDB` storage backend plugin for the Usage Collector storage Plugin SPI.
//!
//! Implements [`usage_collector_sdk::UsageCollectorPluginV1`] on `PostgreSQL` +
//! `TimescaleDB`. Layered DDD-light: [`gear`] performs the GTS registration
//! handshake, [`domain`] holds the SPI adapter and store port traits, and
//! [`infra`] holds the `sqlx`-backed Postgres implementations.

pub mod gear;

pub use gear::TimescaleDbUsageCollectorPlugin;

// === INTERNAL MODULES ===
// Implementation detail of the plugin. Exposed `pub` only so the crate's
// integration tests (separate `tests/*.rs` crates) can construct the stores,
// config, and metrics directly — NOT public API. External consumers depend on
// `TimescaleDbUsageCollectorPlugin` and resolve everything else through the
// plugin host. `#[doc(hidden)]` keeps these off the rendered API surface.
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
