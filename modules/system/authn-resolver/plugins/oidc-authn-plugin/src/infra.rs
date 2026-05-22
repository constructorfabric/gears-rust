//! Infrastructure adapters for the Oidc `AuthN` plugin.
//!
//! This layer owns network access, caches, and resilience primitives used by the
//! domain authentication flow.

pub mod circuit_breaker;
pub(crate) mod http_response;
pub mod jwks;
pub mod oidc;
pub(crate) mod retry;
pub mod runtime;
pub(crate) mod single_flight;
pub(crate) mod token_client;
pub(crate) mod ttl_cache;
#[doc(hidden)]
pub mod url_policy;
