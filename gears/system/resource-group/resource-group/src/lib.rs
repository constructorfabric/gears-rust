// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-28 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-gear-scaffold:p1
//! Resource Group Gear — contracts and domain types.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// === MODULE DEFINITION ===
pub mod config;
pub mod gear;
pub use gear::ResourceGroup;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
