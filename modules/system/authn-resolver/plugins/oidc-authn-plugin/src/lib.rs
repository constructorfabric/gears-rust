//! Oidc Authentication Resolver Plugin for CF.
//!
//! This crate provides JWT local validation via Oidc as the Identity
//! Provider. Non-JWT (opaque) bearer tokens are rejected with 401.
//! It is designed to register with the `CyberFabric` `AuthN` Resolver Gateway
//! as a plugin.
//!
//! # Architecture
//!
//! ```text
//! authenticate(bearer_token)
//!   ├── detect_token_type()
//!   │     ├── JWT    -> JwtValidator (local, cached JWKS)
//!   │     └── Opaque -> Unauthorized (401)
//!   └── ClaimMapper -> SecurityContext
//! ```

// `e2e-diagnostics` may be enabled in release images used for E2E environments.
// Production deployments should keep this feature disabled because it emits
// structured authentication result diagnostics for test assertions.

pub mod config;
pub mod domain;
pub mod infra;
pub mod module;

pub use domain::authenticate;
pub use domain::claim_mapper;
pub use domain::error;
pub use domain::token_type;
pub use domain::validator;

#[doc(hidden)]
pub use infra::circuit_breaker;
#[doc(hidden)]
pub use infra::jwks;
#[doc(hidden)]
pub use infra::oidc;

#[cfg(test)]
pub(crate) mod test_support;

pub use module::OidcAuthNPluginModule;

#[cfg(test)]
mod thread_safety {
    /// Compile-time proof that core plugin types are `Send + Sync`, preventing
    /// regressions that would break multi-threaded async usage.
    const _: fn() = || {
        fn must_be_send_sync<T: Send + Sync>() {}
        must_be_send_sync::<crate::domain::authenticate::OidcAuthNPlugin>();
        must_be_send_sync::<crate::infra::circuit_breaker::CircuitBreaker>();
        must_be_send_sync::<crate::domain::validator::JwtValidator>();
        must_be_send_sync::<crate::infra::jwks::JwksFetcher>();
    };
}
