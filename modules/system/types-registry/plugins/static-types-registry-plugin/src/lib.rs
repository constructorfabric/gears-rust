#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod module;

pub use module::StaticTypesRegistryPlugin;
