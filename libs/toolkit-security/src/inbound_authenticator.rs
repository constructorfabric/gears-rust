//! Building the **inbound** platform-plane validator from `internal_auth`
//! config, shared by every participant that validates platform tokens
//! (api-gateway, grpc-hub, the `OoP` bootstrap).
//!
//! This crate cannot depend on `kube`, so the Kubernetes `TokenReview` validator
//! is supplied by the caller as a [`KubeAuthenticatorBuilder`] (from
//! `toolkit-k8s-auth`, behind the caller's `k8s-auth` feature). A caller built
//! without that feature passes `None`, and `provider: kube` is then a hard error
//! rather than a silent enforcement downgrade.

use std::future::Future;
use std::pin::Pin;

use crate::authenticator::DynInternalAuthenticator;
use crate::internal_auth_config::{BuiltAuthenticator, InternalAuthConfig, InvalidInternalAuth};

/// A boxed error from the caller-supplied Kubernetes backend.
pub type KubeBackendError = Box<dyn std::error::Error + Send + Sync>;

/// Builds the Kubernetes `TokenReview` validator from `verify.audiences` as
/// `(first, rest)`, i.e. non-empty. Supplied by a crate that can depend on `kube`, e.g.
/// `toolkit_k8s_auth::kube_authenticator_builder`, which also fixes the cache
/// settings.
pub type KubeAuthenticatorBuilder = Box<
    dyn FnOnce(
            (String, Vec<String>),
        ) -> Pin<
            Box<dyn Future<Output = Result<DynInternalAuthenticator, KubeBackendError>> + Send>,
        > + Send,
>;

/// Why the inbound platform-plane validator could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InboundAuthenticatorError {
    /// The config is unusable for validation (unusable shared secret, kube
    /// without `verify` or with empty audiences), per
    /// [`InternalAuthConfig::build_authenticator`].
    #[error(transparent)]
    Config(#[from] InvalidInternalAuth),
    /// `provider: kube` is configured, but the caller was built without a
    /// Kubernetes backend.
    #[error("internal_auth provider=kube requires the `k8s-auth` feature")]
    KubeUnsupported,
    /// The Kubernetes backend failed to build the `TokenReview` validator.
    #[error("failed to initialize Kubernetes TokenReview authenticator: {0}")]
    KubeInit(#[source] KubeBackendError),
}

/// Build the inbound platform-plane validator for `cfg`.
///
/// - `None` (Profile 1, no `internal_auth`) → `Ok(None)`: nothing to validate.
/// - `shared_secret` → built here.
/// - `kube` → built by `kube` from the validated audiences.
///
/// # Errors
/// - [`InboundAuthenticatorError::Config`] when
///   [`build_authenticator`](InternalAuthConfig::build_authenticator) refuses
///   the config. An unusable shared secret is reported as such rather than
///   falling through to the kube branch.
/// - [`InboundAuthenticatorError::KubeUnsupported`] for `provider: kube` with
///   no `kube` backend.
/// - [`InboundAuthenticatorError::KubeInit`] when the backend fails.
pub async fn build_inbound_authenticator(
    cfg: Option<&InternalAuthConfig>,
    kube: Option<KubeAuthenticatorBuilder>,
) -> Result<Option<DynInternalAuthenticator>, InboundAuthenticatorError> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };
    match cfg.build_authenticator()? {
        BuiltAuthenticator::Built(authenticator) => {
            tracing::info!("Initializing shared-secret platform-plane authenticator");
            Ok(Some(authenticator))
        }
        BuiltAuthenticator::RequiresExternalBackend { audiences } => {
            let kube = kube.ok_or(InboundAuthenticatorError::KubeUnsupported)?;
            tracing::info!("Initializing Kubernetes TokenReview platform-plane authenticator");
            let authenticator = kube(audiences)
                .await
                .map_err(InboundAuthenticatorError::KubeInit)?;
            Ok(Some(authenticator))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::internal_auth_config::{DEFAULT_INTERNAL_PEER_NAME, KubeVerify};
    use crate::shared_secret::SharedSecretInternalAuthenticator;
    use secrecy::SecretString;
    use std::sync::{Arc, Mutex};

    fn shared_secret(secret: &str) -> InternalAuthConfig {
        InternalAuthConfig::SharedSecret {
            secret: SecretString::from(secret),
            peer_name: DEFAULT_INTERNAL_PEER_NAME.to_owned(),
        }
    }

    fn kube(verify: Option<Vec<&str>>) -> InternalAuthConfig {
        InternalAuthConfig::Kube {
            verify: verify.map(|audiences| KubeVerify {
                audiences: audiences.into_iter().map(str::to_owned).collect(),
            }),
            present: None,
        }
    }

    /// Where [`recording_backend`] stores the audiences it was called with.
    type Seen = Arc<Mutex<Option<(String, Vec<String>)>>>;

    /// A kube backend that records the audiences it was called with and
    /// returns `result`.
    fn recording_backend(seen: Seen, result: Result<(), &'static str>) -> KubeAuthenticatorBuilder {
        Box::new(move |audiences| {
            *seen.lock().unwrap() = Some(audiences);
            Box::pin(async move {
                result.map_err(KubeBackendError::from).map(|()| {
                    DynInternalAuthenticator::new(
                        SharedSecretInternalAuthenticator::try_new(
                            SecretString::from("stub"),
                            DEFAULT_INTERNAL_PEER_NAME.to_owned(),
                        )
                        .unwrap(),
                    )
                })
            })
        })
    }

    #[tokio::test]
    async fn no_config_builds_nothing() {
        assert!(
            build_inbound_authenticator(None, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn shared_secret_is_built_without_a_kube_backend() {
        let auth = build_inbound_authenticator(Some(&shared_secret("s3cret")), None)
            .await
            .unwrap();
        assert!(auth.is_some());
    }

    #[tokio::test]
    async fn unusable_shared_secret_is_reported_as_such() {
        let err = build_inbound_authenticator(Some(&shared_secret("")), None)
            .await
            .expect_err("an empty secret must not validate");
        assert!(
            matches!(
                err,
                InboundAuthenticatorError::Config(InvalidInternalAuth::SharedSecret(_))
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn kube_without_a_backend_is_refused() {
        let err = build_inbound_authenticator(Some(&kube(Some(vec!["toolkit-internal"]))), None)
            .await
            .expect_err("no backend means no TokenReview validator");
        assert!(matches!(err, InboundAuthenticatorError::KubeUnsupported));
        assert!(err.to_string().contains("k8s-auth"), "got: {err}");
    }

    #[tokio::test]
    async fn kube_backend_gets_the_validated_audiences() {
        let seen = Arc::new(Mutex::new(None));
        let auth = build_inbound_authenticator(
            Some(&kube(Some(vec!["toolkit-internal"]))),
            Some(recording_backend(Arc::clone(&seen), Ok(()))),
        )
        .await
        .unwrap();
        assert!(auth.is_some());
        assert_eq!(
            *seen.lock().unwrap(),
            Some(("toolkit-internal".to_owned(), vec![]))
        );
    }

    #[tokio::test]
    async fn kube_backend_failure_is_reported() {
        let err = build_inbound_authenticator(
            Some(&kube(Some(vec!["toolkit-internal"]))),
            Some(recording_backend(
                Arc::new(Mutex::new(None)),
                Err("no in-cluster config"),
            )),
        )
        .await
        .expect_err("a backend failure must fail the build");
        assert!(matches!(err, InboundAuthenticatorError::KubeInit(_)));
        assert!(
            err.to_string().contains("no in-cluster config"),
            "got: {err}"
        );
    }

    /// The backend is never called with an unbound (empty) audience list or
    /// without a `verify` block: those are refused first.
    #[tokio::test]
    async fn invalid_kube_config_never_reaches_the_backend() {
        for (cfg, expected) in [(kube(None), "verify"), (kube(Some(vec![])), "audiences")] {
            let seen = Arc::new(Mutex::new(None));
            let err = build_inbound_authenticator(
                Some(&cfg),
                Some(recording_backend(Arc::clone(&seen), Ok(()))),
            )
            .await
            .expect_err("an unusable kube config must be refused");
            assert!(err.to_string().contains(expected), "got: {err}");
            assert!(seen.lock().unwrap().is_none(), "backend must not be called");
        }
    }
}
