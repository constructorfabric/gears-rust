// Updated: 2026-10-06 by Constructor Tech
#![doc = include_str!("../README.md")]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// The gear publishes this crate's GTS declarations itself; see `gts` and `infra::audit`.
toolkit_gts::declare_gts_inventory!();

pub mod api;
pub mod client;
pub mod config;
#[doc(hidden)]
pub mod domain;
pub mod gear;
pub(crate) mod gts;
#[doc(hidden)]
pub mod infra;

pub use gear::CredStoreGear;
