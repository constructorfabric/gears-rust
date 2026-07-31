//! Error type returned by [`GatewayProvider`](crate::GatewayProvider) operations
//! and the typed wrappers in this crate.

use thiserror::Error;

/// Errors produced when registering or deregistering routes, or when
/// constructing the typed inputs for a [`GatewayProvider`](crate::GatewayProvider).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GatewayError {
    /// The supplied `OpenAPI` document could not be parsed or contained no
    /// routable public operations.
    #[error("invalid OpenAPI spec: {reason}")]
    InvalidSpec {
        /// Human-readable reason the spec was rejected.
        reason: String,
    },

    /// The supplied endpoint was not a valid `scheme://authority` URI.
    #[error("invalid endpoint '{uri}': {reason}")]
    InvalidEndpoint {
        /// The offending endpoint string.
        uri: String,
        /// Human-readable reason the endpoint was rejected.
        reason: String,
    },

    /// No routes are registered for the requested gear.
    #[error("no routes registered for gear '{gear}'")]
    NotRegistered {
        /// The gear that had no registration.
        gear: String,
    },

    /// The gateway backend could not be reached, or returned a transport-level
    /// failure while applying the change.
    #[error("gateway transport error: {reason}")]
    Transport {
        /// Human-readable reason.
        reason: String,
    },
}
