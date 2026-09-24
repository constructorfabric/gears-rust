// Created: 2026-08-12 by Virtuozzo International GmbH
// `coverage(off)` marks the test-module declarations the coverage run should
// not count; the attribute is nightly-only, and `cargo llvm-cov` is what sets
// the cfg. Same line as `toolkit-security` and every other crate that uses it.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! Settings Service gear
//!
//! The service that owns platform settings: declaration registry, scoped value
//! resolution, and validate-then-set writes. Its public contract lives in
//! `cf-gears-settings-service-sdk`; this crate is the implementation.
//!
//! # What is here so far
//!
//! The gear scaffold and its bootstrap contract. Startup reads
//! deployment-owned configuration fail-closed and acquires the database
//! capability.
//!
//! # `ClientHub` registration
//!
//! `init` builds both SDK clients over real implementations and registers
//! them into `ClientHub`: `SettingsReaderClient` over the value resolver and
//! the secret resolver, `SettingsContributionClient` over the contribution
//! service. Both are bound in process; a deployment that wires either one
//! remotely is refused at boot, since this release publishes no remote
//! contract for them (see `config::SettingsServiceConfig`).
//!
//! The `gear-scaffold` definition of done is still open for a different
//! reason: it also asks for the remaining GTS control-plane schemas and the
//! minimal category seed at init, which are not here yet. Its checkbox stays
//! unticked for those, not for the bindings.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod audit;
pub mod config;
pub mod domain;
pub mod field;
pub mod gear;
pub mod infra;
pub mod log_text;
pub mod precondition;

#[cfg(test)]
pub(crate) mod test_support;

pub use config::SettingsServiceConfig;
pub use gear::SettingsService;
