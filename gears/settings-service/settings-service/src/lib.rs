// Created: 2026-08-12 by Constructor Tech
//! Settings Service gear
//!
//! The service that owns platform settings: declaration registry, scoped value
//! resolution, and staged apply. Its public contract lives in
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

pub use config::SettingsServiceConfig;
pub use gear::SettingsService;
