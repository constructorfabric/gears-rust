//! Transport-agnostic bearer-token authentication abstraction.
//!
//! [`BearerAuthenticator`] decouples the HTTP/gRPC transport layers from the
//! concrete `AuthN` Resolver client. The transport only needs to hand a raw
//! bearer token to an implementation and receive a reconstructed
//! [`SecurityContext`] back. The concrete `AuthNResolverClient` adapter is
//! injected at the gear/bootstrap layer so neither `toolkit-http` nor
//! `toolkit-transport-grpc` need to depend on the full `ToolKit` framework.
//!
//! This lives in `toolkit-security` (not `toolkit-http`) so it stays
//! transport-agnostic and reusable by the gRPC path — it returns
//! [`SecurityContext`], which `toolkit-security` already owns, and
//! `toolkit-security` has no dependency on any transport crate.

use std::future::Future;

use crate::context::SecurityContext;

/// Neutral authentication error returned by a [`BearerAuthenticator`].
///
/// Intentionally coarse-grained and transport-agnostic: it never carries the
/// token or any provider-specific detail so it is safe to surface at a trust
/// boundary. Concrete adapters map their own error types into these variants.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthNError {
    /// The token was syntactically present but failed validation
    /// (invalid signature, expired, malformed claims, etc.).
    #[error("invalid or expired token")]
    InvalidToken,
    /// The authentication backend could not be reached or returned a
    /// transient failure. Callers may choose to retry or surface a 503.
    #[error("authentication backend unavailable")]
    Unavailable,
    /// Any other authentication failure. The message must not contain the
    /// token or other sensitive material.
    #[error("authentication failed: {0}")]
    Other(String),
}

/// Re-validates a raw bearer token and reconstructs a [`SecurityContext`].
///
/// Implementations perform a full validation on every call — there is no
/// trusted-peer fast path (zero-trust; see `cpt-cf-adr-two-plane-auth`). The transport layer
/// stays generic over this trait; the concrete `AuthNResolverClient` adapter
/// is supplied at the gear/bootstrap layer.
///
/// The returned future is `Send` so the trait can be used from Axum/Tower
/// middleware running on a multi-threaded runtime.
pub trait BearerAuthenticator: Send + Sync {
    /// Validate `token` and reconstruct the corresponding [`SecurityContext`].
    ///
    /// # Errors
    ///
    /// Returns [`AuthNError`] if the token is invalid, the backend is
    /// unavailable, or authentication otherwise fails.
    fn authenticate(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<SecurityContext, AuthNError>> + Send;
}
