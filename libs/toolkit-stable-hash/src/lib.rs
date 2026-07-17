#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![forbid(unsafe_code)]
//! Stable non-cryptographic hash implementations for compatibility contracts.
//!
//! Algorithms in this crate keep their output stable across compatible
//! releases. They are not suitable for security-sensitive hashing.

mod murmur3;
pub use murmur3::murmur3_x86_32;

#[cfg(test)]
mod murmur3_tests;
