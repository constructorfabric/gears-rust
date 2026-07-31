//! Declarative configuration for the platform (internal) authentication plane.
//!
//! [`InternalAuthConfig`] is the single, transport-agnostic config surface used
//! by every participant in the platform plane:
//!
//! - **`OoP` gears** (`oop_http.internal_auth`) — selects the *inbound* HTTP
//!   validator for the gear's own routes **and** the *outbound* credential the
//!   gear attaches to its `DirectoryService` calls.
//! - **The `gear-orchestrator`** — selects the *inbound* gRPC validator that
//!   protects the `DirectoryService` RPCs.
//! - **The `api-gateway`** (`gateway_proxy.internal_auth`) — selects the
//!   *outbound* credential attached to the edge's `DirectoryService` polls.
//!
//! Two providers are supported:
//!
//! - [`InternalAuthConfig::SharedSecret`] — a single pre-shared token. No
//!   Kubernetes required; ideal for local demos, single-node deployments, and
//!   tests. Both the inbound validator and the outbound credential are derived
//!   from the same `secret`.
//! - [`InternalAuthConfig::Kube`] — a projected `ServiceAccount` token
//!   (Profile 3). Inbound validation uses the Kubernetes `TokenReview` API
//!   (built in a layer that can depend on `kube`); the outbound credential is
//!   read (and rotated) from `token_path`.
//!
//! This crate builds only the dependency-light shared-secret validator
//! ([`build_authenticator`](InternalAuthConfig::build_authenticator)); the
//! `TokenReview` validator and the outbound interceptor are constructed by the
//! `kube` / transport layers, which read the accessors here.

use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::authenticator::DynInternalAuthenticator;
use crate::shared_secret::SharedSecretInternalAuthenticator;

/// Default caller label assigned to a validated shared-secret peer.
pub const DEFAULT_INTERNAL_PEER_NAME: &str = "toolkit-internal";

/// Platform-plane authentication provider selection.
///
/// Serialized with an internal `provider` tag, e.g.
///
/// ```yaml
/// internal_auth:
///   provider: shared_secret
///   secret: "dev-internal-token"
///   peer_name: "hello"
/// ```
///
/// or
///
/// ```yaml
/// internal_auth:
///   provider: kube
///   audiences: ["toolkit-internal"]
///   token_path: /var/run/secrets/tokens/toolkit-internal
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum InternalAuthConfig {
    /// A single pre-shared secret (dev / single-node). See the [module
    /// docs](self).
    SharedSecret {
        /// The shared token accepted (inbound) and attached (outbound).
        secret: String,
        /// Caller label assigned to validated peers (inbound side only).
        #[serde(default = "default_peer_name")]
        peer_name: String,
    },
    /// A projected Kubernetes `ServiceAccount` token (Profile 3).
    Kube {
        /// Expected token audiences for `TokenReview` (inbound). When empty,
        /// the API server's default audience validation applies.
        #[serde(default)]
        audiences: Vec<String>,
        /// Projected-token path to read + rotate for outbound calls. When
        /// absent, no outbound credential is attached (inbound-only).
        #[serde(default)]
        token_path: Option<PathBuf>,
    },
}

fn default_peer_name() -> String {
    DEFAULT_INTERNAL_PEER_NAME.to_owned()
}

impl InternalAuthConfig {
    /// Build the **inbound** validator when it can be constructed without a
    /// heavier backend.
    ///
    /// Returns `Some` for [`InternalAuthConfig::SharedSecret`]. Returns `None`
    /// for [`InternalAuthConfig::Kube`], whose `TokenReview` validator must be
    /// built by a layer that can depend on `kube` (e.g. `toolkit-k8s-auth`),
    /// using [`kube_audiences`](Self::kube_audiences).
    #[must_use]
    pub fn build_authenticator(&self) -> Option<DynInternalAuthenticator> {
        match self {
            Self::SharedSecret { secret, peer_name } => {
                let auth = SharedSecretInternalAuthenticator::new(
                    SecretString::from(secret.clone()),
                    peer_name.clone(),
                );
                Some(DynInternalAuthenticator::new(auth))
            }
            Self::Kube { .. } => None,
        }
    }

    /// The static outbound credential for the shared-secret provider, if any.
    #[must_use]
    pub fn shared_secret(&self) -> Option<SecretString> {
        match self {
            Self::SharedSecret { secret, .. } => Some(SecretString::from(secret.clone())),
            Self::Kube { .. } => None,
        }
    }

    /// Whether this config selects the Kubernetes provider.
    #[must_use]
    pub fn is_kube(&self) -> bool {
        matches!(self, Self::Kube { .. })
    }

    /// The configured `TokenReview` audiences for the Kubernetes provider.
    #[must_use]
    pub fn kube_audiences(&self) -> Option<&[String]> {
        match self {
            Self::Kube { audiences, .. } => Some(audiences),
            Self::SharedSecret { .. } => None,
        }
    }

    /// The projected-token path for the Kubernetes provider's outbound
    /// credential, if configured.
    #[must_use]
    pub fn kube_token_path(&self) -> Option<&Path> {
        match self {
            Self::Kube { token_path, .. } => token_path.as_deref(),
            Self::SharedSecret { .. } => None,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn deserializes_shared_secret_with_default_peer_name() {
        let cfg: InternalAuthConfig = serde_json::from_value(serde_json::json!({
            "provider": "shared_secret",
            "secret": "s"
        }))
        .unwrap();
        match &cfg {
            InternalAuthConfig::SharedSecret { secret, peer_name } => {
                assert_eq!(secret, "s");
                assert_eq!(peer_name, DEFAULT_INTERNAL_PEER_NAME);
            }
            InternalAuthConfig::Kube { .. } => panic!("expected shared_secret"),
        }
        assert!(cfg.build_authenticator().is_some());
        assert!(!cfg.is_kube());
    }

    #[test]
    fn deserializes_kube_with_token_path() {
        let cfg: InternalAuthConfig = serde_json::from_value(serde_json::json!({
            "provider": "kube",
            "audiences": ["toolkit-internal"],
            "token_path": "/var/run/secrets/tokens/toolkit-internal"
        }))
        .unwrap();
        assert!(cfg.is_kube());
        assert_eq!(
            cfg.kube_audiences(),
            Some(&["toolkit-internal".to_owned()][..])
        );
        assert_eq!(
            cfg.kube_token_path().map(Path::to_owned),
            Some(PathBuf::from("/var/run/secrets/tokens/toolkit-internal"))
        );
        // Kube inbound validator is built elsewhere (needs kube).
        assert!(cfg.build_authenticator().is_none());
        assert!(cfg.shared_secret().is_none());
    }
}
