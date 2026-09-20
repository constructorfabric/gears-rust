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
//! # `ClientHub` registration is not here yet
//!
//! `dod-gear-scaffold` also requires the client traits to be registered into
//! `ClientHub`. That is deliberately absent: registration publishes a binding
//! other gears resolve, and there is no implementation behind
//! `SettingsReaderClient` until the persistence adapter and value resolver
//! exist. Registering a stub would let a consumer bind successfully and fail on
//! every call — worse than a resolution failure, which is at least honest about
//! what is missing. Its checkbox stays unticked until the binding is real.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod audit;
pub mod config;
pub mod domain;
pub mod field;
pub mod gear;
pub mod infra;
pub mod precondition;

#[cfg(test)]
pub(crate) mod test_support;

pub use config::SettingsServiceConfig;
pub use gear::SettingsService;
