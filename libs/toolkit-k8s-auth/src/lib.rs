#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

//! Kubernetes `TokenReview`-based platform-plane authenticator for `ToolKit`.
//!
//! Provides [`K8sTokenReviewAuthenticator`], a concrete
//! [`InternalAuthenticator`](toolkit_security::InternalAuthenticator) that
//! validates an inbound `X-ToolKit-Internal-Token` (a projected `ServiceAccount`
//! JWT) by submitting it to the Kubernetes `TokenReview` API. On success it
//! resolves the caller to a
//! [`PlatformIdentity::KubernetesServiceAccount`](toolkit_security::PlatformIdentity).
//!
//! This lives in its own leaf crate (pulling in `kube` / `k8s-openapi`) so the
//! foundational crates stay free of the Kubernetes client. Wire it into the
//! `OoP` bootstrap via `DynInternalAuthenticator` when the deployment uses
//! `InternalCredential::KubeServiceAccountToken` (Profile 3).
//!
//! ```rust,no_run
//! use toolkit_k8s_auth::{K8sAuthError, K8sTokenReviewAuthenticator};
//!
//! # async fn wire() -> Result<(), K8sAuthError> {
//! // Validates tokens scoped to the `toolkit-internal` audience.
//! let authenticator =
//!     K8sTokenReviewAuthenticator::try_default(vec!["toolkit-internal".to_owned()]).await?;
//! # let _ = authenticator;
//! # Ok(())
//! # }
//! ```

pub mod cache;

pub use cache::{CachingInternalAuthenticator, DEFAULT_TOKEN_REVIEW_CACHE_TTL};

use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec};
use kube::api::{Api, PostParams};
use toolkit_security::{InternalAuthNError, InternalAuthenticator, PlatformIdentity};

/// Prefix of the `user.username` returned by `TokenReview` for a
/// `ServiceAccount`: `system:serviceaccount:<namespace>:<name>`.
const SA_USERNAME_PREFIX: &str = "system:serviceaccount:";

/// Extra key carrying the originating pod name, when the API server includes it.
const POD_NAME_EXTRA_KEY: &str = "authentication.kubernetes.io/pod-name";

/// Per-request timeout for the Kubernetes `TokenReview` API call.
const TOKEN_REVIEW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Error constructing a [`K8sTokenReviewAuthenticator`].
#[derive(Debug, thiserror::Error)]
pub enum K8sAuthError {
    /// The Kubernetes client could not be constructed (no in-cluster config or
    /// kubeconfig, or the config was invalid).
    #[error("failed to construct Kubernetes client: {0}")]
    Client(#[from] kube::Error),
}

/// A platform-plane authenticator backed by the Kubernetes `TokenReview` API.
#[derive(Clone)]
pub struct K8sTokenReviewAuthenticator {
    client: kube::Client,
    /// Expected token audiences. When non-empty they are sent to the API server,
    /// which rejects tokens not scoped to (at least) one of them.
    audiences: Vec<String>,
    /// Per-request timeout for the `TokenReview` API call.
    timeout: std::time::Duration,
}

impl std::fmt::Debug for K8sTokenReviewAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("K8sTokenReviewAuthenticator")
            .field("audiences", &self.audiences)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl K8sTokenReviewAuthenticator {
    /// Build an authenticator from an existing [`kube::Client`] with the
    /// default per-request timeout.
    #[must_use]
    pub fn new(client: kube::Client, audiences: Vec<String>) -> Self {
        Self {
            client,
            audiences,
            timeout: TOKEN_REVIEW_TIMEOUT,
        }
    }

    /// Override the per-request timeout for the `TokenReview` API call.
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Build an authenticator using the ambient Kubernetes configuration
    /// (in-cluster service account, or a local kubeconfig for development).
    ///
    /// # Errors
    /// Returns [`K8sAuthError::Client`] if no valid configuration is available.
    pub async fn try_default(audiences: Vec<String>) -> Result<Self, K8sAuthError> {
        let client = kube::Client::try_default().await?;
        Ok(Self::new(client, audiences))
    }

    /// Submit a `TokenReview` for `token` and resolve the platform identity.
    async fn review(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        let audiences = if self.audiences.is_empty() {
            None
        } else {
            Some(self.audiences.clone())
        };

        let review = TokenReview {
            spec: TokenReviewSpec {
                token: Some(token.to_owned()),
                audiences,
            },
            ..Default::default()
        };

        let api: Api<TokenReview> = Api::all(self.client.clone());
        let response =
            match tokio::time::timeout(self.timeout, api.create(&PostParams::default(), &review))
                .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(e)) => {
                    // A failed API call is a backend problem, not a bad credential.
                    tracing::warn!(error = %e, "TokenReview API call failed");
                    return Err(InternalAuthNError::Unavailable);
                }
                Err(_) => {
                    tracing::warn!("TokenReview API call timed out");
                    return Err(InternalAuthNError::Unavailable);
                }
            };

        let status = response.status.ok_or_else(|| {
            InternalAuthNError::Other("TokenReview returned no status".to_owned())
        })?;

        if !status.authenticated.unwrap_or(false) {
            if let Some(err) = status.error.as_deref() {
                tracing::debug!(error = %err, "TokenReview rejected credential");
            }
            return Err(InternalAuthNError::InvalidToken);
        }

        // When audiences are requested, the API server returns the token's
        // audiences in the status. Reject tokens that do not satisfy at least
        // one of the configured expected audiences.
        if !self.audiences.is_empty() {
            let response_audiences = status.audiences.as_deref().unwrap_or_default();
            if !self
                .audiences
                .iter()
                .any(|a| response_audiences.contains(a))
            {
                tracing::debug!(
                    expected = ?self.audiences,
                    got = ?response_audiences,
                    "TokenReview audience mismatch"
                );
                return Err(InternalAuthNError::InvalidToken);
            }
        }

        let user = status
            .user
            .ok_or_else(|| InternalAuthNError::Other("TokenReview missing user info".to_owned()))?;
        let username = user.username.unwrap_or_default();

        let (namespace, service_account) = parse_sa_username(&username).ok_or_else(|| {
            InternalAuthNError::Other(format!(
                "authenticated principal is not a ServiceAccount: {username}"
            ))
        })?;

        let pod = user
            .extra
            .as_ref()
            .and_then(|extra| extra.get(POD_NAME_EXTRA_KEY))
            .and_then(|pods| pods.first())
            .cloned();

        Ok(PlatformIdentity::KubernetesServiceAccount {
            namespace,
            service_account,
            pod,
        })
    }
}

impl InternalAuthenticator for K8sTokenReviewAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        self.review(token).await
    }
}

/// Parse a `ServiceAccount` `TokenReview` username
/// (`system:serviceaccount:<namespace>:<name>`) into `(namespace, name)`.
///
/// Returns `None` for non-`ServiceAccount` principals or malformed values.
fn parse_sa_username(username: &str) -> Option<(String, String)> {
    let rest = username.strip_prefix(SA_USERNAME_PREFIX)?;
    let (namespace, name) = rest.split_once(':')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some((namespace.to_owned(), name.to_owned()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_service_account_username() {
        assert_eq!(
            parse_sa_username("system:serviceaccount:toolkit:flight-control"),
            Some(("toolkit".to_owned(), "flight-control".to_owned()))
        );
    }

    #[test]
    fn rejects_non_service_account_principal() {
        assert_eq!(parse_sa_username("system:node:node-1"), None);
        assert_eq!(parse_sa_username("alice"), None);
    }

    #[test]
    fn rejects_malformed_service_account_username() {
        // Missing name component.
        assert_eq!(parse_sa_username("system:serviceaccount:toolkit"), None);
        // Empty namespace / name.
        assert_eq!(parse_sa_username("system:serviceaccount::name"), None);
        assert_eq!(parse_sa_username("system:serviceaccount:ns:"), None);
    }

    #[test]
    fn keeps_name_with_trailing_colon_segments() {
        // `split_once` keeps everything after the first ':' as the name.
        assert_eq!(
            parse_sa_username("system:serviceaccount:ns:a:b"),
            Some(("ns".to_owned(), "a:b".to_owned()))
        );
    }
}
