//! License Enforcement Gear
//!
//! Stores grants and packs, gives the management operations on them, and
//! answers `is_licensed` as a backend plugin of license-resolver. Root grants
//! come only from the admission plugin that the binary links; the evaluator
//! that decides which grants satisfy a check is linked the same way.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod test_support;
