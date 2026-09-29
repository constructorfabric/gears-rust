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
//! let auth = SharedSecretInternalAuthenticator::try_new(
//!     SecretString::from("dev-internal-token"),
//!     "toolkit-host".to_owned(),
//! )
//! .expect("a non-empty secret");
//! assert!(auth.authenticate("dev-internal-token").await.is_ok());
//! assert!(auth.authenticate("wrong").await.is_err());
//! # }
//! ```

use std::future::Future;

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

/// The placeholder written in place of a secret when a config is serialized.
///
/// Rejected as a secret: a config dumped by `--print-config` would otherwise
/// round-trip into a working one whose platform credential is this literal —
/// a value published in the repository.
pub const REDACTED_PLACEHOLDER: &str = "<redacted>";

/// Why a shared secret was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InvalidSharedSecret {
    /// The secret was empty, which would authenticate an empty token.
    #[error("shared secret must not be empty")]
    Empty,
    /// The secret is the redaction placeholder, i.e. a dumped config was fed
    /// back in as a real one.
    #[error(
        "shared secret is the literal `{REDACTED_PLACEHOLDER}` placeholder, which a serialized \
         config writes in place of the real secret"
    )]
    RedactedPlaceholder,
}

impl SharedSecretInternalAuthenticator {
    /// Build an authenticator that accepts exactly `secret` and resolves valid
    /// callers to [`PlatformIdentity::Shared`] with the label `peer_name`.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidSharedSecret`] if the secret is empty or is the
    /// [`REDACTED_PLACEHOLDER`]. An empty secret is the dangerous one: the
    /// comparison in [`InternalAuthenticator::authenticate`] is over bytes, and
    /// an empty secret matches an empty token — so anyone sending the internal
    /// header with no value would authenticate as `peer_name`.
    pub fn try_new(secret: SecretString, peer_name: String) -> Result<Self, InvalidSharedSecret> {
        match secret.expose_secret() {
            "" => Err(InvalidSharedSecret::Empty),
            REDACTED_PLACEHOLDER => Err(InvalidSharedSecret::RedactedPlaceholder),
            _ => Ok(Self { secret, peer_name }),
        }
    }
}

impl InternalAuthenticator for SharedSecretInternalAuthenticator {
    fn authenticate(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<PlatformIdentity, InternalAuthNError>> + Send {
        let result = if constant_time_eq(token.as_bytes(), self.secret.expose_secret().as_bytes()) {
            Ok(PlatformIdentity::Shared {
                name: self.peer_name.clone(),
            })
        } else {
            // A rejected platform-plane credential left no trace at all, so a
            // peer configured with the wrong secret looked identical to one
            // that was never configured. The peer name is the configured label,
            // not anything the caller supplied, and the token never appears.
            tracing::warn!(
                peer_name = %self.peer_name,
                "platform-plane authentication rejected: shared secret did not match"
            );
            Err(InternalAuthNError::InvalidToken)
        };
        std::future::ready(result)
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

    #[test]
    fn an_empty_secret_is_refused() {
        // The comparison is over bytes, so an empty secret matches an empty
        // token: anyone sending the internal header with no value would
        // authenticate. Refusing at construction is the only place to catch it.
        assert_eq!(
            SharedSecretInternalAuthenticator::try_new(SecretString::from(""), "peer".to_owned())
                .unwrap_err(),
            InvalidSharedSecret::Empty
        );
    }

    #[test]
    fn the_redaction_placeholder_is_refused() {
        // A config dumped by `--print-config` writes this in place of the
        // secret and deserializes cleanly, so without this the platform plane
        // would come up accepting a literal published in the repository.
        assert_eq!(
            SharedSecretInternalAuthenticator::try_new(
                SecretString::from(REDACTED_PLACEHOLDER),
                "peer".to_owned()
            )
            .unwrap_err(),
            InvalidSharedSecret::RedactedPlaceholder
        );
    }

    #[tokio::test]
    async fn an_empty_token_is_rejected_by_a_real_secret() {
        let rejected = auth().authenticate("").await;
        assert!(matches!(rejected, Err(InternalAuthNError::InvalidToken)));
    }

    fn auth() -> SharedSecretInternalAuthenticator {
        SharedSecretInternalAuthenticator::try_new(SecretString::from("s3cr3t"), "peer".to_owned())
            .expect("a non-empty secret")
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
