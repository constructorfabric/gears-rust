//! Quota Resolver gear: maps principals to quota subjects and delegates quota
//! operations to a backend plugin found through types-registry.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;
