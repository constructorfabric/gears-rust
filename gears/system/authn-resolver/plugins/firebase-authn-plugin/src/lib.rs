//! Configurable Firebase `AuthN` plugin.
//! The verifier preserves RS256/JWK validation; identity and tenant selection are deployment data.
pub mod config;
pub mod firebase;
mod gear;
pub use gear::FirebaseAuthnPlugin;
