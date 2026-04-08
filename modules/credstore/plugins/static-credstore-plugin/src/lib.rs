#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod module;

pub use module::StaticCredStorePlugin;
