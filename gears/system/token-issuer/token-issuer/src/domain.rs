//! Domain layer: claim assembly, the cap-token cache, JWT minting, the JWKS
//! builder, metrics, the error model, and the RMS-registry / peer-identity
//! ports. Free of transport/runtime concerns.

pub mod cache;
pub mod cap_verify;
pub mod claims;
pub mod downscope;
pub mod error;
pub mod jwks;
pub mod jws;
pub mod loopguard;
pub mod metrics;
pub mod obo;
pub mod obo_cache;
pub mod peer_identity;
pub mod rms_registry;
pub mod service;

pub use error::DomainError;
