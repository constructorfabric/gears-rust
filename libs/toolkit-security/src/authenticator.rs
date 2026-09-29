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
use std::pin::Pin;
use std::sync::Arc;

use crate::context::SecurityContext;
use crate::internal_auth::{InternalAuthNError, InternalAuthenticator, PlatformIdentity};

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
    /// cancel-safe: this future is dropped mid-flight whenever a client
    /// disconnects, because it runs from Axum/Tower middleware on a task the
    /// server aborts. An implementation must therefore hold no state across the
    /// await that would be corrupted by never resuming — an abandoned call must
    /// leave the authenticator exactly as it found it. Losing in-flight work
    /// (a cache insert, a single-flight slot) is fine; a half-applied mutation
    /// is not.
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

type BearerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SecurityContext, AuthNError>> + Send + 'a>>;

/// Object-safe erasure of [`BearerAuthenticator`].
///
/// [`BearerAuthenticator::authenticate`] returns `impl Future`, so the trait is
/// not `dyn`-compatible. This trait boxes the future so a concrete authenticator
/// can be stored behind an `Arc` and shared/injected as a trait object.
trait ErasedBearer: Send + Sync {
    fn authenticate<'a>(&'a self, token: &'a str) -> BearerFuture<'a>;
}

impl<A: BearerAuthenticator> ErasedBearer for A {
    fn authenticate<'a>(&'a self, token: &'a str) -> BearerFuture<'a> {
        Box::pin(BearerAuthenticator::authenticate(self, token))
    }
}

/// Injectable, object-safe tenant-plane authenticator.
///
/// Wraps a concrete [`BearerAuthenticator`] (e.g. an `AuthNResolverClient`
/// adapter) so it can be stored behind an `Arc` and registered in a
/// `ClientHub` / handed to the `OoP` HTTP runtime. Lives in `toolkit-security`
/// (a leaf crate, always available) so any gear can register the bridge without
/// depending on the bootstrap-gated `toolkit` runtime.
#[derive(Clone)]
pub struct DynBearerAuthenticator(Arc<dyn ErasedBearer>);

impl DynBearerAuthenticator {
    /// Wrap a concrete [`BearerAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn new<A: BearerAuthenticator + 'static>(authenticator: A) -> Self {
        Self(Arc::new(authenticator))
    }

    /// Wrap an already-`Arc`'d [`BearerAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn from_arc<A: BearerAuthenticator + 'static>(authenticator: Arc<A>) -> Self {
        // The blanket `impl<A: BearerAuthenticator> ErasedBearer for A` means
        // `Arc<A>` unsizes to `Arc<dyn ErasedBearer>` directly. An adapter
        // struct here would allocate a second `Arc` purely to hold the first,
        // and cost an extra pointer hop on every authenticate call.
        Self(authenticator)
    }
}

impl std::fmt::Debug for DynBearerAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynBearerAuthenticator")
            .finish_non_exhaustive()
    }
}

impl BearerAuthenticator for DynBearerAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        self.0.authenticate(token).await
    }
}

type InternalFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PlatformIdentity, InternalAuthNError>> + Send + 'a>>;

/// Object-safe erasure of [`InternalAuthenticator`] (same rationale as
/// [`ErasedBearer`]).
trait ErasedInternal: Send + Sync {
    fn authenticate<'a>(&'a self, token: &'a str) -> InternalFuture<'a>;
}

impl<A: InternalAuthenticator> ErasedInternal for A {
    fn authenticate<'a>(&'a self, token: &'a str) -> InternalFuture<'a> {
        Box::pin(InternalAuthenticator::authenticate(self, token))
    }
}

/// Injectable, object-safe platform-plane authenticator.
///
/// Wraps a concrete [`InternalAuthenticator`] (e.g. the K8s `TokenReview`
/// validator) so it can be stored behind an `Arc` and registered in a
/// `ClientHub` / handed to the `OoP` HTTP runtime. Lives in `toolkit-security`
/// (a leaf crate, always available) so any gear can register the bridge without
/// depending on the bootstrap-gated `toolkit` runtime — the platform-plane
/// mirror of [`DynBearerAuthenticator`].
#[derive(Clone)]
pub struct DynInternalAuthenticator(Arc<dyn ErasedInternal>);

impl DynInternalAuthenticator {
    /// Wrap a concrete [`InternalAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn new<A: InternalAuthenticator + 'static>(authenticator: A) -> Self {
        Self(Arc::new(authenticator))
    }

    /// Wrap an already-`Arc`'d [`InternalAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn from_arc<A: InternalAuthenticator + 'static>(authenticator: Arc<A>) -> Self {
        // As in `DynBearerAuthenticator::from_arc`: the blanket impl lets
        // `Arc<A>` unsize directly, so no adapter allocation is needed.
        Self(authenticator)
    }
}

impl std::fmt::Debug for DynInternalAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynInternalAuthenticator")
            .finish_non_exhaustive()
    }
}

impl InternalAuthenticator for DynInternalAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        self.0.authenticate(token).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use uuid::Uuid;

    // A tenant-plane authenticator that echoes the token into the subject_type
    // on success, or fails for the token "bad".
    struct FakeBearer;

    impl BearerAuthenticator for FakeBearer {
        fn authenticate(
            &self,
            token: &str,
        ) -> impl Future<Output = Result<SecurityContext, AuthNError>> + Send {
            let result = if token == "bad" {
                Err(AuthNError::InvalidToken)
            } else {
                SecurityContext::builder()
                    .subject_id(Uuid::from_u128(1))
                    .subject_tenant_id(Uuid::from_u128(2))
                    .subject_type(token)
                    .build()
                    .map_err(|e| AuthNError::Other(e.to_string()))
            };
            std::future::ready(result)
        }
    }

    struct FakeInternal;

    impl InternalAuthenticator for FakeInternal {
        fn authenticate(
            &self,
            token: &str,
        ) -> impl Future<Output = Result<PlatformIdentity, InternalAuthNError>> + Send {
            let result = if token == "bad" {
                Err(InternalAuthNError::InvalidToken)
            } else {
                Ok(PlatformIdentity::Shared {
                    name: token.to_owned(),
                })
            };
            std::future::ready(result)
        }
    }

    #[tokio::test]
    async fn dyn_bearer_new_delegates_to_inner() {
        let auth = DynBearerAuthenticator::new(FakeBearer);
        let ctx = BearerAuthenticator::authenticate(&auth, "alice")
            .await
            .expect("authenticates");
        assert_eq!(ctx.subject_type(), Some("alice"));

        let err = BearerAuthenticator::authenticate(&auth, "bad")
            .await
            .expect_err("rejects");
        assert!(matches!(err, AuthNError::InvalidToken));
    }

    #[tokio::test]
    async fn dyn_bearer_from_arc_and_clone_delegate_to_inner() {
        let auth = DynBearerAuthenticator::from_arc(Arc::new(FakeBearer));
        let cloned = auth.clone();
        let ctx = BearerAuthenticator::authenticate(&cloned, "bob")
            .await
            .expect("authenticates");
        assert_eq!(ctx.subject_type(), Some("bob"));
    }

    #[test]
    fn dyn_bearer_debug_is_non_exhaustive() {
        let auth = DynBearerAuthenticator::new(FakeBearer);
        // The whole rendering, not just the struct name: the name alone can
        // only fail on a rename, while what this is guarding is that the
        // wrapped authenticator -- which may hold a secret -- stays out of the
        // output, and that the `..` marker says so.
        assert_eq!(format!("{auth:?}"), "DynBearerAuthenticator { .. }");
    }

    #[tokio::test]
    async fn dyn_internal_new_delegates_to_inner() {
        let auth = DynInternalAuthenticator::new(FakeInternal);
        let identity = InternalAuthenticator::authenticate(&auth, "gear-a")
            .await
            .expect("authenticates");
        assert_eq!(identity.peer_name(), "gear-a");

        let err = InternalAuthenticator::authenticate(&auth, "bad")
            .await
            .expect_err("rejects");
        assert!(matches!(err, InternalAuthNError::InvalidToken));
    }

    #[tokio::test]
    async fn dyn_internal_from_arc_and_clone_delegate_to_inner() {
        let auth = DynInternalAuthenticator::from_arc(Arc::new(FakeInternal));
        let cloned = auth.clone();
        let identity = InternalAuthenticator::authenticate(&cloned, "gear-b")
            .await
            .expect("authenticates");
        assert_eq!(identity.peer_name(), "gear-b");
    }

    #[test]
    fn dyn_internal_debug_is_non_exhaustive() {
        let auth = DynInternalAuthenticator::new(FakeInternal);
        // As above: the platform-plane authenticator this wraps holds the
        // shared secret, so the assertion is that nothing of it is rendered.
        assert_eq!(format!("{auth:?}"), "DynInternalAuthenticator { .. }");
    }
}
