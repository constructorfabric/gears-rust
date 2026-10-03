#![doc = include_str!("../README.md")]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod client;
pub mod config;
pub mod domain;
pub mod gear;
pub(crate) mod gts;
pub mod infra;

pub use gear::CredStoreGear;
