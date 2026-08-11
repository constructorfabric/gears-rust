//! token-issuer — mints signed ES256 capability JWTs and serves JWKS/discovery.
//!
//! Provides: config, claim assembly, the get-or-mint cap-token cache, JWT
//! assembly + ES256 signing (via the injected `SigningClientV1` port), the JWKS
//! builder, the error model, the gear runtime (init + readiness-gated serve),
//! the GTS signing-plugin selector, and the public JWKS/discovery REST routes.

pub mod api;
pub mod config;
pub mod domain;
pub mod infra;
pub mod module;

pub use config::TokenIssuerConfig;
pub use module::TokenIssuerGear;
