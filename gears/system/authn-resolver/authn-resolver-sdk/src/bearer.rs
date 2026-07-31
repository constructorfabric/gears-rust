//! Adapter from [`AuthNResolverClient`] to the transport-agnostic
//! [`BearerAuthenticator`] trait.
//!
//! The HTTP/gRPC middleware in `toolkit-http-middleware` depends only on
//! [`BearerAuthenticator`] (owned by `toolkit-security`) so it need not know
//! about the `AuthN` Resolver at all. This adapter is the concrete bridge,
//! supplied at the gear/bootstrap layer (`cpt-cf-adr-two-plane-auth`): an
//! out-of-process gear that wants tenant-plane authentication resolves the
//! in-process [`AuthNResolverClient`] from its `ClientHub` and wraps it here.
//!
//! # Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use authn_resolver_sdk::{AuthNResolverClient, AuthNResolverBearerAuthenticator};
//!
//! let client: Arc<dyn AuthNResolverClient> = hub.get::<dyn AuthNResolverClient>()?;
//! let authenticator = AuthNResolverBearerAuthenticator::new(client);
//! // hand `authenticator` to the OoP bootstrap (wrapped in DynBearerAuthenticator)
//! ```

use std::future::Future;
use std::sync::Arc;

use toolkit_security::{AuthNError, BearerAuthenticator, SecurityContext};

use crate::api::AuthNResolverClient;
use crate::error::AuthNResolverError;

/// Wraps an [`AuthNResolverClient`] so it satisfies [`BearerAuthenticator`].
///
/// Every call performs a full re-validation of the token via the resolver
/// (zero-trust; no trusted-peer fast path) and maps the resolver's rich error
/// type into the coarse, transport-safe [`AuthNError`].
#[derive(Clone)]
pub struct AuthNResolverBearerAuthenticator {
    inner: Arc<dyn AuthNResolverClient>,
}

impl AuthNResolverBearerAuthenticator {
    /// Create an adapter over the given resolver client.
    #[must_use]
    pub fn new(inner: Arc<dyn AuthNResolverClient>) -> Self {
        Self { inner }
    }
}

impl std::fmt::Debug for AuthNResolverBearerAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthNResolverBearerAuthenticator")
            .finish_non_exhaustive()
    }
}

impl BearerAuthenticator for AuthNResolverBearerAuthenticator {
    fn authenticate(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<SecurityContext, AuthNError>> + Send {
        // Clone into an owned future so the returned `impl Future` is `'static`
        // + `Send` (the resolver client's own future is boxed by `async_trait`).
        let inner = Arc::clone(&self.inner);
        let token = token.to_owned();
        async move {
            inner
                .authenticate(&token)
                .await
                .map(|result| result.security_context)
                .map_err(|err| map_error(&err))
        }
    }
}

/// Map the resolver's error taxonomy onto the coarse [`AuthNError`] surfaced at
/// the trust boundary. The message never carries the token or provider detail.
fn map_error(err: &AuthNResolverError) -> AuthNError {
    match err {
        AuthNResolverError::Unauthorized(_) => AuthNError::InvalidToken,
        AuthNResolverError::NoPluginAvailable | AuthNResolverError::ServiceUnavailable(_) => {
            AuthNError::Unavailable
        }
        AuthNResolverError::TokenAcquisitionFailed(_) | AuthNResolverError::Internal(_) => {
            AuthNError::Other("authentication failed".to_owned())
        }
    }
}
