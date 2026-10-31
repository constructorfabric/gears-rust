//! Explicit rustls integration boundary for TLS bootstrap and configuration.
//!
//! Protocol code needs rustls types. Application primitives use the facade's
//! hash, MAC, RNG and signature APIs instead of backend-specific factories.

pub use rustls::crypto::CryptoProvider;

/// Construct the provider selected by this build's platform/FIPS policy.
///
/// This neither installs a process-wide provider nor claims deployment
/// validation. TLS bootstrap retains its conflict checks and runtime witness;
/// application primitives validate their independently cached provider.
#[must_use]
pub fn configured_provider() -> CryptoProvider {
    crate::provider::configured_provider()
}
