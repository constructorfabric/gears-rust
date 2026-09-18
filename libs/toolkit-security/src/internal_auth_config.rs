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
use crate::shared_secret::{InvalidSharedSecret, SharedSecretInternalAuthenticator};

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
#[derive(Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum InternalAuthConfig {
    /// A single pre-shared secret (dev / single-node). See the [module
    /// docs](self).
    SharedSecret {
        /// The shared token accepted (inbound) and attached (outbound).
        ///
        /// A `SecretString` rather than a `String` so redaction is structural:
        /// it zeroizes on drop and renders as `[REDACTED]` in any `{:?}` sink,
        /// instead of depending on the hand-written `Debug` and `Serialize`
        /// impls below staying correct as fields are added.
        secret: SecretString,
        /// Caller label assigned to validated peers (inbound side only).
        #[serde(default = "default_peer_name")]
        peer_name: String,
    },
    /// A projected Kubernetes `ServiceAccount` token (Profile 3).
    Kube {
        /// Expected token audiences for `TokenReview` (inbound).
        ///
        /// **Required, and must not be empty.** An empty list disables audience
        /// binding twice over in `toolkit-k8s-auth`: no audience is sent to the
        /// API server, *and* the client-side comparison against the response is
        /// skipped. Any `ServiceAccount` token the API server accepts — from
        /// any workload in the cluster, issued for any audience — would then
        /// authenticate as a platform peer.
        ///
        /// It used to default to empty, so a config that simply omitted the
        /// field got that silently.
        #[serde(deserialize_with = "non_empty_audiences")]
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

/// Deserialize `audiences`, refusing an absent or empty list.
///
/// Failing here rather than at first use is deliberate: an unbound audience is
/// a cluster-wide authentication weakness, and a service that comes up and
/// accepts every `ServiceAccount` token is far worse than one that refuses to
/// start with a config error naming the field.
fn non_empty_audiences<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    let audiences = Vec::<String>::deserialize(deserializer)?;
    if audiences.is_empty() {
        return Err(D::Error::custom(
            "internal_auth provider=kube requires a non-empty `audiences` list; an empty one \
             disables audience verification and accepts any ServiceAccount token in the cluster",
        ));
    }
    Ok(audiences)
}

/// Manual [`Debug`] that never renders the shared secret. The derived impl would
/// print `secret` verbatim, leaking the platform-plane credential into any
/// `{:?}` sink (config tracing, panic messages, error context). All other
/// fields — including `peer_name` and the `Kube` variant — are shown as-is.
impl std::fmt::Debug for InternalAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SharedSecret { peer_name, .. } => f
                .debug_struct("SharedSecret")
                .field("secret", &"<redacted>")
                .field("peer_name", peer_name)
                .finish(),
            Self::Kube {
                audiences,
                token_path,
            } => f
                .debug_struct("Kube")
                .field("audiences", audiences)
                .field("token_path", token_path)
                .finish(),
        }
    }
}

/// Manual [`Serialize`] that never emits the shared secret in plaintext.
///
/// A derived `Serialize` would write `secret` verbatim, leaking the
/// platform-plane credential whenever a containing config is serialized — most
/// notably `AppConfig::to_yaml` behind `--print-config`. Instead the secret is
/// replaced with a `<redacted>` placeholder; every other field (and the
/// internally-tagged `provider` shape) is preserved so the output still round-
/// trips structurally.
///
/// This is safe because the config is only ever *deserialized* to obtain the
/// real secret; serialization is used for diagnostics, never to transmit the
/// credential.
impl Serialize for InternalAuthConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::SharedSecret { peer_name, .. } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("provider", "shared_secret")?;
                map.serialize_entry("secret", "<redacted>")?;
                map.serialize_entry("peer_name", peer_name)?;
                map.end()
            }
            Self::Kube {
                audiences,
                token_path,
            } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("provider", "kube")?;
                map.serialize_entry("audiences", audiences)?;
                map.serialize_entry("token_path", token_path)?;
                map.end()
            }
        }
    }
}

/// Why a platform-plane authenticator could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InvalidInternalAuth {
    /// The configured shared secret is unusable.
    #[error(transparent)]
    SharedSecret(#[from] InvalidSharedSecret),
    /// `provider: kube` was configured with no audiences. An empty list
    /// disables audience verification on both sides — nothing is sent to the
    /// API server, and the client-side check against the response is skipped —
    /// so any `ServiceAccount` token the cluster issues would authenticate.
    #[error(
        "internal_auth provider=kube requires a non-empty `audiences` list; an empty one \
         disables audience verification and accepts any ServiceAccount token in the cluster"
    )]
    EmptyKubeAudiences,
}

/// What [`InternalAuthConfig::build_authenticator`] could do with the config.
///
/// An `Option` conflated two unrelated answers on `None`: "this provider needs
/// no validator" and "this provider's validator must be built by a layer that
/// can depend on `kube`". Nothing in the type said which, so all three callers
/// re-derived it with `is_kube()` afterwards and each had to remember to fail
/// on the fallthrough — a caller that forgot would have run an unauthenticated
/// platform plane.
#[derive(Debug)]
pub enum BuiltAuthenticator {
    /// The validator was built here.
    Built(DynInternalAuthenticator),
    /// The provider needs a backend this crate cannot construct. Build it with
    /// [`kube_audiences`](InternalAuthConfig::kube_audiences), via a layer that
    /// depends on `kube` (e.g. `toolkit-k8s-auth`).
    RequiresExternalBackend,
}

impl InternalAuthConfig {
    /// Build the **inbound** validator when it can be constructed without a
    /// heavier backend.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidInternalAuth::SharedSecret`] when the configured shared
    /// secret is unusable — empty, or the redaction placeholder from a
    /// serialized config. Returns [`InvalidInternalAuth::EmptyKubeAudiences`]
    /// when `provider: kube` has no configured audiences.
    pub fn build_authenticator(&self) -> Result<BuiltAuthenticator, InvalidInternalAuth> {
        match self {
            Self::SharedSecret { secret, peer_name } => {
                let auth =
                    SharedSecretInternalAuthenticator::try_new(secret.clone(), peer_name.clone())?;
                Ok(BuiltAuthenticator::Built(DynInternalAuthenticator::new(
                    auth,
                )))
            }
            // Checked here as well as during deserialization. The variant's
            // fields are public, so a config built in code never passes through
            // serde — and every caller reaches this before it reads
            // `kube_audiences`, which makes it the last point where an unbound
            // validator can still be refused.
            Self::Kube { audiences, .. } if audiences.is_empty() => {
                Err(InvalidInternalAuth::EmptyKubeAudiences)
            }
            Self::Kube { .. } => Ok(BuiltAuthenticator::RequiresExternalBackend),
        }
    }

    /// The static outbound credential for the shared-secret provider, if any.
    #[must_use]
    pub fn shared_secret(&self) -> Option<SecretString> {
        match self {
            Self::SharedSecret { secret, .. } => Some(secret.clone()),
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
    use crate::internal_auth::{InternalAuthNError, InternalAuthenticator, PlatformIdentity};
    use secrecy::ExposeSecret;

    #[test]
    fn debug_redacts_shared_secret_but_keeps_other_fields() {
        let cfg = InternalAuthConfig::SharedSecret {
            secret: SecretString::from("super-secret-token"),
            peer_name: "hello".to_owned(),
        };
        let rendered = format!("{cfg:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "secret must never appear in Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "expected redaction marker");
        assert!(rendered.contains("hello"), "peer_name must be preserved");

        // The Kube variant carries no secret; all fields render normally.
        let kube = InternalAuthConfig::Kube {
            audiences: vec!["toolkit-internal".to_owned()],
            token_path: Some(PathBuf::from("/var/run/secrets/tokens/t")),
        };
        let rendered = format!("{kube:?}");
        assert!(rendered.contains("toolkit-internal"));
        assert!(rendered.contains("/var/run/secrets/tokens/t"));
    }

    #[test]
    fn serialize_redacts_shared_secret_but_keeps_other_fields() {
        let cfg = InternalAuthConfig::SharedSecret {
            secret: SecretString::from("super-secret-token"),
            peer_name: "hello".to_owned(),
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(
            !json.contains("super-secret-token"),
            "secret must never be serialized in plaintext: {json}"
        );
        assert!(json.contains("<redacted>"), "expected redaction marker");
        assert!(json.contains("hello"), "peer_name must be preserved");
        assert!(json.contains("shared_secret"), "provider tag preserved");

        // The redacted form still deserializes structurally (tag + fields).
        let round: InternalAuthConfig = serde_json::from_str(&json).expect("round-trip");
        assert!(matches!(round, InternalAuthConfig::SharedSecret { .. }));

        // The Kube variant carries no secret; all fields serialize normally.
        let kube = InternalAuthConfig::Kube {
            audiences: vec!["toolkit-internal".to_owned()],
            token_path: Some(PathBuf::from("/var/run/secrets/tokens/t")),
        };
        let json = serde_json::to_string(&kube).expect("serialize");
        assert!(json.contains("kube"));
        assert!(json.contains("toolkit-internal"));
        assert!(json.contains("/var/run/secrets/tokens/t"));
    }

    #[tokio::test]
    async fn deserializes_shared_secret_with_default_peer_name() {
        let cfg: InternalAuthConfig = serde_json::from_value(serde_json::json!({
            "provider": "shared_secret",
            "secret": "s"
        }))
        .unwrap();
        match &cfg {
            InternalAuthConfig::SharedSecret { secret, peer_name } => {
                assert_eq!(secret.expose_secret(), "s");
                assert_eq!(peer_name, DEFAULT_INTERNAL_PEER_NAME);
            }
            InternalAuthConfig::Kube { .. } => panic!("expected shared_secret"),
        }
        assert!(!cfg.is_kube());

        // Authenticate through what was built, rather than asserting only that
        // something was: `is_some()` passes even if `secret` and `peer_name`
        // were wired to the authenticator the wrong way round.
        let BuiltAuthenticator::Built(authenticator) =
            cfg.build_authenticator().expect("a valid secret")
        else {
            panic!("shared_secret builds its validator here");
        };
        let identity = authenticator
            .authenticate("s")
            .await
            .expect("the configured secret must authenticate");
        assert_eq!(
            identity,
            PlatformIdentity::Shared {
                name: DEFAULT_INTERNAL_PEER_NAME.to_owned()
            },
            "the caller must be labelled with the configured peer name"
        );

        let rejected = authenticator.authenticate("not-the-secret").await;
        assert!(
            matches!(rejected, Err(InternalAuthNError::InvalidToken)),
            "a wrong secret must be rejected, got {rejected:?}"
        );
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
        // Kube inbound validator is built elsewhere (needs kube), and the type
        // says so rather than returning a bare `None` the caller has to
        // interpret.
        assert!(matches!(
            cfg.build_authenticator(),
            Ok(BuiltAuthenticator::RequiresExternalBackend)
        ));
        assert!(cfg.shared_secret().is_none());
    }

    #[test]
    fn kube_built_in_code_with_no_audiences_is_refused() {
        // The serde check does not cover this: the variant's fields are public,
        // so a config constructed in code never passes through deserialization.
        let unbound = InternalAuthConfig::Kube {
            audiences: Vec::new(),
            token_path: None,
        };
        assert!(matches!(
            unbound.build_authenticator(),
            Err(InvalidInternalAuth::EmptyKubeAudiences)
        ));

        // With an audience it is buildable again, by the layer that owns kube.
        let bound = InternalAuthConfig::Kube {
            audiences: vec!["toolkit-internal".to_owned()],
            token_path: None,
        };
        assert!(matches!(
            bound.build_authenticator(),
            Ok(BuiltAuthenticator::RequiresExternalBackend)
        ));
    }

    #[test]
    fn kube_without_audiences_is_refused() {
        // An omitted list used to default to empty, which disables audience
        // binding entirely -- any ServiceAccount token in the cluster would
        // authenticate as a platform peer. Refusing the config is the point:
        // failing to start beats starting unbound.
        let err =
            serde_json::from_value::<InternalAuthConfig>(serde_json::json!({ "provider": "kube" }))
                .expect_err("kube without audiences must not deserialize");
        assert!(
            err.to_string().contains("audiences"),
            "the error must name the field: {err}"
        );

        let err = serde_json::from_value::<InternalAuthConfig>(
            serde_json::json!({ "provider": "kube", "audiences": [] }),
        )
        .expect_err("an explicitly empty list is the same weakness");
        assert!(err.to_string().contains("audiences"), "got: {err}");
    }

    #[test]
    fn kube_with_audiences_and_no_token_path_is_inbound_only() {
        let cfg: InternalAuthConfig = serde_json::from_value(
            serde_json::json!({ "provider": "kube", "audiences": ["toolkit-internal"] }),
        )
        .unwrap();

        assert!(cfg.is_kube());
        assert_eq!(
            cfg.kube_token_path(),
            None,
            "no token path means inbound-only: nothing is minted outbound"
        );
    }

    #[test]
    fn rejects_an_unknown_provider() {
        // A typo in `provider` must fail the config rather than silently
        // selecting a default plane.
        let result: Result<InternalAuthConfig, _> =
            serde_json::from_value(serde_json::json!({ "provider": "kubernetes" }));
        assert!(
            result.is_err(),
            "an unrecognised provider must not deserialize"
        );
    }

    #[test]
    fn shared_secret_accessors_return_none_for_kube_fields() {
        let cfg = InternalAuthConfig::SharedSecret {
            secret: SecretString::from("s"),
            peer_name: "hello".to_owned(),
        };
        // Outbound credential is the shared secret itself.
        assert_eq!(
            cfg.shared_secret().map(|s| s.expose_secret().to_owned()),
            Some("s".to_owned())
        );
        // Kube-only accessors are None for the shared-secret provider.
        assert!(cfg.kube_audiences().is_none());
        assert!(cfg.kube_token_path().is_none());
    }
}
