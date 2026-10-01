#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! Policy Engine gear.
//!
//! Stores, validates and evaluates tenant policy documents on the shared
//! evaluation facility, and serves as an admission-control engine plugin. See
//! `gears/system/policy-engine/docs/DESIGN.md`.
//!
//! [`PolicyEngine`] is the composed gear: `gear` wires every domain and infra
//! component together, runs its lifecycle, and exposes its readiness.
#![forbid(unsafe_code)]

#[doc(hidden)]
pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

pub use gear::PolicyEngine;
