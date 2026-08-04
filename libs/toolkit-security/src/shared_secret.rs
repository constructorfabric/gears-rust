//! Shared-secret platform-plane authenticator (dev / single-node profiles).
//!
//! [`SharedSecretInternalAuthenticator`] is a dependency-light concrete
//! [`InternalAuthenticator`] that validates an inbound
//! `X-ToolKit-Internal-Token` by comparing it, in constant time, against a
//! single pre-shared secret. On success it resolves the caller to
//! [`PlatformIdentity::Shared`] with a configured label.
//!
//! It exists so the platform plane can be exercised **end-to-end without
//! Kubernetes** (no `TokenReview` API, no projected service-account volume) —
//! useful for local demos, single-node deployments, and tests. It is **not** a
//! substitute for the K8s `TokenReview` validator in a real multi-tenant
//! cluster: a single shared secret has none of `TokenReview`'s per-workload
//! identity, rotation, or revocation properties.
//!
//! ```rust
//! use secrecy::SecretString;
//! use toolkit_security::{InternalAuthenticator, SharedSecretInternalAuthenticator};
//!
//! # async fn demo() {
//! let auth = SharedSecretInternalAuthenticator::new(
//!     SecretString::from("dev-internal-token"),
//!     "toolkit-host".to_owned(),
//! );
//! assert!(auth.authenticate("dev-internal-token").await.is_ok());
//! assert!(auth.authenticate("wrong").await.is_err());
//! # }
//! ```

use secrecy::{ExposeSecret, SecretString};

use crate::internal_auth::{InternalAuthNError, InternalAuthenticator, PlatformIdentity};

/// A platform-plane authenticator backed by a single pre-shared secret.
///
/// See the [module docs](self) for when this is (and is not) appropriate.
#[derive(Clone)]
pub struct SharedSecretInternalAuthenticator {
    secret: SecretString,
    peer_name: String,
}

impl std::fmt::Debug for SharedSecretInternalAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the secret.
        f.debug_struct("SharedSecretInternalAuthenticator")
            .field("peer_name", &self.peer_name)
            .finish_non_exhaustive()
    }
}

impl SharedSecretInternalAuthenticator {
    /// Build an authenticator that accepts exactly `secret` and resolves valid
    /// callers to [`PlatformIdentity::Shared`] with the label `peer_name`.
    #[must_use]
    pub fn new(secret: SecretString, peer_name: String) -> Self {
        Self { secret, peer_name }
    }
}

impl InternalAuthenticator for SharedSecretInternalAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        if constant_time_eq(token.as_bytes(), self.secret.expose_secret().as_bytes()) {
            Ok(PlatformIdentity::Shared {
                name: self.peer_name.clone(),
            })
        } else {
            Err(InternalAuthNError::InvalidToken)
        }
    }
}

/// Compare two byte slices without short-circuiting on the first differing
/// byte, so validation time does not leak the position of a mismatch.
///
/// The length comparison is not itself constant-time; a pre-shared secret's
/// length is not sensitive here.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn auth() -> SharedSecretInternalAuthenticator {
        SharedSecretInternalAuthenticator::new(SecretString::from("s3cr3t"), "peer".to_owned())
    }

    #[tokio::test]
    async fn accepts_matching_secret_and_resolves_shared_identity() {
        let identity = auth().authenticate("s3cr3t").await.expect("valid secret");
        assert_eq!(
            identity,
            PlatformIdentity::Shared {
                name: "peer".to_owned()
            }
        );
        assert_eq!(identity.peer_name(), "peer");
    }

    #[tokio::test]
    async fn rejects_wrong_secret() {
        let err = auth().authenticate("nope").await.unwrap_err();
        assert!(matches!(err, InternalAuthNError::InvalidToken));
    }

    #[tokio::test]
    async fn rejects_empty_and_prefix_tokens() {
        assert!(auth().authenticate("").await.is_err());
        assert!(auth().authenticate("s3cr3").await.is_err());
        assert!(auth().authenticate("s3cr3tt").await.is_err());
    }

    #[test]
    fn constant_time_eq_matches_std_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn debug_never_leaks_secret() {
        let rendered = format!("{:?}", auth());
        assert!(rendered.contains("peer"));
        assert!(!rendered.contains("s3cr3t"));
    }
}
