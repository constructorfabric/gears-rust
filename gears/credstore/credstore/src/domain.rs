//! Credential-store domain model and service boundaries.
//!
//! Contains authorization policy, errors, ports, hierarchical resolution, and
//! the secret lifecycle service independently of HTTP and database adapters.

pub mod authz;
pub mod error;
pub mod ports;
pub mod resolver;
pub mod secret;
