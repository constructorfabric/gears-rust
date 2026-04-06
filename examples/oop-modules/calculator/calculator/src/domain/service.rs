//! Domain service for calculator
//!
//! Contains the core business logic for accumulator operations.

use modkit_macros::domain_model;
use tracing::debug;

/// Domain service that performs accumulator operations.
///
/// This is a simple stateless service that implements the core
/// addition logic. It's registered in ClientHub and used by
/// the gRPC server.
#[domain_model]
#[derive(Clone, Default)]
pub struct Service;

impl Service {
    /// Create a new service.
    pub fn new() -> Self {
        Self
    }

    /// Add two numbers and return the sum.
    pub fn add(&self, a: i64, b: i64) -> i64 {
        debug!(a, b, "performing addition");
        a + b
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod tests;
