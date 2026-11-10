// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
#![doc = include_str!("../README.md")]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
mod factory;
pub mod gear;
pub mod infra;

pub use factory::client_from_config;
pub use gear::VaultCredStorePlugin;
