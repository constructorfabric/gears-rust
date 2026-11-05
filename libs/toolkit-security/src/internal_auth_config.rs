//! Declarative configuration for the platform (internal) authentication plane.
//!
//! [`InternalAuthConfig`] is the single, transport-agnostic config surface used
//! by every participant in the platform plane. A participant may use either or
//! both directions:
//!
//! - **Inbound** — validating the `X-ToolKit-Internal-Token` callers present:
//!   `OoP` gears (their own routes), `grpc-hub` (the RPCs it serves, e.g.
//!   `DirectoryService`) and the `api-gateway` (co-hosted internal routes).
//! - **Outbound** — the credential a participant presents on its own
//!   platform-plane calls: `OoP` gears (`DirectoryService` registration and
//!   heartbeat) and the `api-gateway` (`DirectoryService` polls).
//!
//! Two providers are supported:
//!
//! - [`InternalAuthConfig::SharedSecret`] — a single pre-shared token. No
//!   Kubernetes required; ideal for local demos, single-node deployments, and
//!   tests. It is **symmetric**: the same `secret` is both accepted (inbound)
//!   and attached (outbound).
//! - [`InternalAuthConfig::Kube`] — projected `ServiceAccount` tokens
//!   (Profile 3). The two directions are unrelated, so each has its own block:
//!   `verify` (inbound: `TokenReview` audiences) and `present` (outbound: the
//!   projected-token file to read and rotate). Either may be omitted, but not
//!   both. A missing block is an error only for the code that needs it:
//!   [`build_authenticator`](InternalAuthConfig::build_authenticator) refuses a
//!   config without `verify`, and the outbound builder in
//!   `toolkit-transport-grpc` refuses one without `present`.
//!
//! This crate builds only the dependency-light shared-secret validator
//! ([`build_authenticator`](InternalAuthConfig::build_authenticator)); the
//! `TokenReview` validator and the outbound interceptor are constructed by the
//! `kube` / transport layers, which read the accessors here.

use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::de::IgnoredAny;
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
///   verify:                      # inbound; omit if this participant validates nothing
///     audiences: ["toolkit-internal"]
///   present:                     # outbound; omit if this participant never calls out
///     token_path: /var/run/secrets/tokens/toolkit-internal
/// ```
///
/// The flat kube form (`audiences` / `token_path` next to `provider`) is no
/// longer accepted; it fails to deserialize with an error naming the new
/// blocks.
#[derive(Clone, Deserialize)]
#[serde(try_from = "RawInternalAuthConfig")]
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
        peer_name: String,
    },
    /// Projected Kubernetes `ServiceAccount` tokens (Profile 3).
    ///
    /// Deserialization requires at least one block. A value built in code may
    /// have neither; it is then refused by both the inbound and the outbound
    /// builders rather than treated as "no authentication".
    Kube {
        /// Inbound: how to validate the tokens callers present.
        verify: Option<KubeVerify>,
        /// Outbound: the token this participant presents on its own calls.
        present: Option<KubePresent>,
    },
}

/// Inbound half of [`InternalAuthConfig::Kube`]: validate callers' projected
/// tokens via the Kubernetes `TokenReview` API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KubeVerify {
    /// Expected token audiences for `TokenReview`.
    ///
    /// **Required, and must not be empty.** An empty list disables audience
    /// binding twice over in `toolkit-k8s-auth`: no audience is sent to the
    /// API server, *and* the client-side comparison against the response is
    /// skipped. Any `ServiceAccount` token the API server accepts — from any
    /// workload in the cluster, issued for any audience — would then
    /// authenticate as a platform peer.
    #[serde(deserialize_with = "non_empty_audiences")]
    pub audiences: Vec<String>,
}

/// Outbound half of [`InternalAuthConfig::Kube`]: the projected token this
/// participant presents on its own platform-plane calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KubePresent {
    /// Projected-token file to read, and re-read on rotation.
    pub token_path: PathBuf,
}

/// Wire shape of [`InternalAuthConfig`], validated by its `TryFrom` impl.
///
/// Unknown keys are rejected at every level, so a misspelled block (e.g.
/// `prsent:`) fails the config instead of reading as an omitted one. serde only
/// allows `deny_unknown_fields` on the whole enum, so it covers `shared_secret`
/// too.
#[derive(Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
enum RawInternalAuthConfig {
    SharedSecret {
        secret: SecretString,
        #[serde(default = "default_peer_name")]
        peer_name: String,
    },
    Kube {
        #[serde(default)]
        verify: Option<KubeVerify>,
        #[serde(default)]
        present: Option<KubePresent>,
        // The removed flat form, captured only to reject it with a pointer to
        // the new blocks. Ignoring it silently would turn a deployed
        // `{audiences, token_path}` config into "no blocks".
        #[serde(default)]
        audiences: Option<IgnoredAny>,
        #[serde(default)]
        token_path: Option<IgnoredAny>,
    },
}

impl TryFrom<RawInternalAuthConfig> for InternalAuthConfig {
    type Error = String;

    fn try_from(raw: RawInternalAuthConfig) -> Result<Self, Self::Error> {
        match raw {
            RawInternalAuthConfig::SharedSecret { secret, peer_name } => {
                Ok(Self::SharedSecret { secret, peer_name })
            }
            RawInternalAuthConfig::Kube {
                audiences,
                token_path,
                ..
            } if audiences.is_some() || token_path.is_some() => Err(
                "internal_auth provider=kube no longer accepts top-level `audiences` / \
                 `token_path`: move `audiences` under `verify:` (inbound) and `token_path` \
                 under `present:` (outbound)"
                    .to_owned(),
            ),
            RawInternalAuthConfig::Kube {
                verify: None,
                present: None,
                ..
            } => Err(
                "internal_auth provider=kube needs at least one of `verify:` (inbound \
                 audiences) or `present:` (outbound token_path)"
                    .to_owned(),
            ),
            RawInternalAuthConfig::Kube {
                verify, present, ..
            } => Ok(Self::Kube { verify, present }),
        }
    }
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
            "internal_auth provider=kube requires a non-empty `verify.audiences` list; an empty \
             one disables audience verification and accepts any ServiceAccount token in the \
             cluster",
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
            Self::Kube { verify, present } => f
                .debug_struct("Kube")
                .field("verify", verify)
                .field("present", present)
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
            Self::Kube { verify, present } => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("provider", "kube")?;
                if let Some(verify) = verify {
                    map.serialize_entry("verify", verify)?;
                }
                if let Some(present) = present {
                    map.serialize_entry("present", present)?;
                }
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
        "internal_auth provider=kube requires a non-empty `verify.audiences` list; an empty one \
         disables audience verification and accepts any ServiceAccount token in the cluster"
    )]
    EmptyKubeAudiences,
    /// `provider: kube` has no `verify` block, but this participant validates
    /// inbound platform-plane tokens. Refused rather than running the inbound
    /// side unauthenticated.
    #[error(
        "internal_auth provider=kube has no `verify:` block, but this participant validates \
         inbound platform-plane tokens; set `verify.audiences`"
    )]
    MissingKubeVerify,
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
    /// The provider needs a backend this crate cannot construct: build it via a
    /// layer that depends on `kube` (e.g. `toolkit-k8s-auth`) with the carried
    /// `audiences`.
    RequiresExternalBackend {
        /// The kube `verify.audiences`, already checked to be non-empty. An
        /// empty list would disable audience binding, so callers take this
        /// value rather than re-reading (and defaulting) the config.
        audiences: Vec<String>,
    },
}

impl InternalAuthConfig {
    /// Build the **inbound** validator when it can be constructed without a
    /// heavier backend. Only participants that validate inbound tokens call
    /// this, so it is where a missing kube `verify` block is refused.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidInternalAuth::SharedSecret`] when the configured shared
    /// secret is unusable — empty, or the redaction placeholder from a
    /// serialized config. Returns [`InvalidInternalAuth::MissingKubeVerify`]
    /// when `provider: kube` has no `verify` block, and
    /// [`InvalidInternalAuth::EmptyKubeAudiences`] when its audience list is
    /// empty.
    pub fn build_authenticator(&self) -> Result<BuiltAuthenticator, InvalidInternalAuth> {
        match self {
            Self::SharedSecret { secret, peer_name } => {
                let auth =
                    SharedSecretInternalAuthenticator::try_new(secret.clone(), peer_name.clone())?;
                Ok(BuiltAuthenticator::Built(DynInternalAuthenticator::new(
                    auth,
                )))
            }
            Self::Kube { verify: None, .. } => Err(InvalidInternalAuth::MissingKubeVerify),
            // Checked here as well as during deserialization. The fields are
            // public, so a config built in code never passes through serde —
            // and the audiences callers build the validator with come out of
            // this check, so it is the last point where an unbound validator
            // can still be refused.
            Self::Kube {
                verify: Some(verify),
                ..
            } if verify.audiences.is_empty() => Err(InvalidInternalAuth::EmptyKubeAudiences),
            Self::Kube {
                verify: Some(verify),
                ..
            } => Ok(BuiltAuthenticator::RequiresExternalBackend {
                audiences: verify.audiences.clone(),
            }),
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

    /// The configured `TokenReview` audiences (`verify.audiences`) for the
    /// Kubernetes provider, if it validates inbound tokens.
    #[must_use]
    pub fn kube_audiences(&self) -> Option<&[String]> {
        match self {
            Self::Kube {
                verify: Some(verify),
                ..
            } => Some(&verify.audiences),
            Self::Kube { verify: None, .. } | Self::SharedSecret { .. } => None,
        }
    }

    /// The projected-token path (`present.token_path`) for the Kubernetes
    /// provider's outbound credential, if configured.
    #[must_use]
    pub fn kube_token_path(&self) -> Option<&Path> {
        match self {
            Self::Kube {
                present: Some(present),
                ..
            } => Some(&present.token_path),
            Self::Kube { present: None, .. } | Self::SharedSecret { .. } => None,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::internal_auth::{InternalAuthNError, InternalAuthenticator, PlatformIdentity};
    use secrecy::ExposeSecret;

    fn kube_both() -> InternalAuthConfig {
        InternalAuthConfig::Kube {
            verify: Some(KubeVerify {
                audiences: vec!["toolkit-internal".to_owned()],
            }),
            present: Some(KubePresent {
                token_path: PathBuf::from("/var/run/secrets/tokens/t"),
            }),
        }
    }

    fn parse(value: serde_json::Value) -> Result<InternalAuthConfig, serde_json::Error> {
        serde_json::from_value(value)
    }

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
        let rendered = format!("{:?}", kube_both());
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

        // The Kube variant carries no secret; all fields serialize normally,
        // in the nested shape, and round-trip.
        let json = serde_json::to_value(kube_both()).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "provider": "kube",
                "verify": { "audiences": ["toolkit-internal"] },
                "present": { "token_path": "/var/run/secrets/tokens/t" },
            })
        );
        let round = parse(json).expect("round-trip");
        assert_eq!(round.kube_audiences().map(<[String]>::len), Some(1));
        assert!(round.kube_token_path().is_some());
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
    fn deserializes_kube_with_both_blocks() {
        let cfg = parse(serde_json::json!({
            "provider": "kube",
            "verify": { "audiences": ["toolkit-internal"] },
            "present": { "token_path": "/var/run/secrets/tokens/toolkit-internal" }
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
        // says so — carrying the validated audiences — rather than returning a
        // bare `None` the caller has to interpret.
        match cfg.build_authenticator() {
            Ok(BuiltAuthenticator::RequiresExternalBackend { audiences }) => {
                assert_eq!(audiences, ["toolkit-internal".to_owned()]);
            }
            other => panic!("expected RequiresExternalBackend, got {other:?}"),
        }
        assert!(cfg.shared_secret().is_none());
    }

    #[test]
    fn kube_verify_only_validates_but_presents_nothing() {
        let cfg = parse(serde_json::json!({
            "provider": "kube",
            "verify": { "audiences": ["toolkit-internal"] }
        }))
        .unwrap();
        assert!(matches!(
            cfg.build_authenticator(),
            Ok(BuiltAuthenticator::RequiresExternalBackend { .. })
        ));
        assert_eq!(cfg.kube_token_path(), None);
    }

    #[test]
    fn kube_present_only_refuses_to_build_an_inbound_validator() {
        let cfg = parse(serde_json::json!({
            "provider": "kube",
            "present": { "token_path": "/var/run/secrets/tokens/toolkit-internal" }
        }))
        .unwrap();
        assert!(cfg.kube_token_path().is_some());
        assert!(cfg.kube_audiences().is_none());
        // A participant that validates inbound tokens must not come up with
        // its inbound side unauthenticated.
        let err = cfg
            .build_authenticator()
            .expect_err("no verify block means no inbound validator");
        assert!(matches!(err, InvalidInternalAuth::MissingKubeVerify));
        assert!(err.to_string().contains("verify"), "got: {err}");
    }

    #[test]
    fn kube_without_any_block_is_refused() {
        let err = parse(serde_json::json!({ "provider": "kube" }))
            .expect_err("a kube config with neither block does nothing");
        let msg = err.to_string();
        assert!(
            msg.contains("verify") && msg.contains("present"),
            "the error must name both blocks: {msg}"
        );
    }

    #[test]
    fn flat_kube_form_is_refused_with_a_migration_hint() {
        for legacy in [
            serde_json::json!({
                "provider": "kube",
                "audiences": ["toolkit-internal"],
                "token_path": "/var/run/secrets/tokens/toolkit-internal"
            }),
            serde_json::json!({ "provider": "kube", "audiences": ["toolkit-internal"] }),
            // Mixed: new block plus a leftover flat field is still refused.
            serde_json::json!({
                "provider": "kube",
                "verify": { "audiences": ["toolkit-internal"] },
                "token_path": "/var/run/secrets/tokens/toolkit-internal"
            }),
        ] {
            let err = parse(legacy.clone()).expect_err("the flat form was removed");
            let msg = err.to_string();
            assert!(
                msg.contains("verify:") && msg.contains("present:"),
                "the error must point to the new blocks for {legacy}: {msg}"
            );
        }
    }

    #[test]
    fn kube_verify_without_audiences_is_refused() {
        // An omitted list used to default to empty, which disables audience
        // binding entirely -- any ServiceAccount token in the cluster would
        // authenticate as a platform peer. Refusing the config is the point:
        // failing to start beats starting unbound.
        let err = parse(serde_json::json!({ "provider": "kube", "verify": {} }))
            .expect_err("verify without audiences must not deserialize");
        assert!(
            err.to_string().contains("audiences"),
            "the error must name the field: {err}"
        );

        let err = parse(serde_json::json!({ "provider": "kube", "verify": { "audiences": [] } }))
            .expect_err("an explicitly empty list is the same weakness");
        assert!(err.to_string().contains("audiences"), "got: {err}");
    }

    #[test]
    fn kube_blocks_reject_unknown_fields() {
        // A typo inside a block must not silently drop the setting.
        let err = parse(serde_json::json!({
            "provider": "kube",
            "present": { "tokenpath": "/var/run/secrets/tokens/toolkit-internal" }
        }))
        .expect_err("misspelled field");
        assert!(err.to_string().contains("tokenpath"), "got: {err}");
    }

    #[test]
    fn misspelled_kube_block_is_refused_rather_than_read_as_omitted() {
        // `prsent` must not parse as "no present block": that would surface
        // later as a misleading "set present.token_path" (or an inactive
        // gateway proxy) with nothing pointing at the typo.
        let err = parse(serde_json::json!({
            "provider": "kube",
            "verify": { "audiences": ["toolkit-internal"] },
            "prsent": { "token_path": "/var/run/secrets/tokens/toolkit-internal" }
        }))
        .expect_err("a misspelled block must not be ignored");
        assert!(err.to_string().contains("prsent"), "got: {err}");

        // The legacy keys are still declared, so they keep their migration
        // message instead of becoming a generic unknown-field error.
        let err = parse(serde_json::json!({
            "provider": "kube",
            "audiences": ["toolkit-internal"]
        }))
        .expect_err("the flat form was removed");
        assert!(err.to_string().contains("verify:"), "got: {err}");
    }

    #[test]
    fn shared_secret_rejects_unknown_fields() {
        let err = parse(serde_json::json!({
            "provider": "shared_secret",
            "secret": "s",
            "peer_nmae": "hello"
        }))
        .expect_err("a misspelled field must not be ignored");
        assert!(err.to_string().contains("peer_nmae"), "got: {err}");
    }

    #[test]
    fn kube_built_in_code_with_no_audiences_or_no_blocks_is_refused() {
        // The serde checks do not cover this: the fields are public, so a
        // config constructed in code never passes through deserialization.
        let unbound = InternalAuthConfig::Kube {
            verify: Some(KubeVerify {
                audiences: Vec::new(),
            }),
            present: None,
        };
        assert!(matches!(
            unbound.build_authenticator(),
            Err(InvalidInternalAuth::EmptyKubeAudiences)
        ));

        let empty = InternalAuthConfig::Kube {
            verify: None,
            present: None,
        };
        assert!(matches!(
            empty.build_authenticator(),
            Err(InvalidInternalAuth::MissingKubeVerify)
        ));

        // With an audience it is buildable again, by the layer that owns kube.
        assert!(matches!(
            kube_both().build_authenticator(),
            Ok(BuiltAuthenticator::RequiresExternalBackend { .. })
        ));
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
