//! Secret lifecycle domain.
//!
//! Defines metadata models, persistence and type-resolution ports, hierarchical
//! lookup, crash-safe writes, expiry, and the value-fingerprint fence.

pub mod fence;
pub mod model;
pub mod repo;
pub mod service;
#[cfg(test)]
pub mod test_support;
pub mod type_resolver;
pub mod typing;
