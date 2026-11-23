//! The **outbound** platform-plane credential, built from `internal_auth` config
//! (`cpt-cf-adr-two-plane-auth`).
//!
//! This is the single config → credential mapping. It yields a transport-agnostic
//! [`InternalTokenProvider`]; transports adapt it instead of resolving config on
//! their own:
//!
//! - REST and generated gRPC clients take the provider as is (via `ClientTuning`
//!   / `GearCtx`).
//! - Hand-written tonic clients (the `DirectoryService` client, cluster-sdk) wrap
//!   it with [`internal_auth_interceptor`].
//!
//! Every carrier therefore applies the same attach policy,
//! [`InternalTokenProvider::resolve_for_attach`]: `Available` attaches, an
//! `Unavailable` source warns, and `NotConfigured` is silent.

use std::path::PathBuf;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_security::{InternalAuthConfig, InvalidOutboundAuth, OutboundSpec};
use toolkit_transport_grpc::{
    DEFAULT_REFRESH_INTERVAL, InternalAuthInterceptor, ServiceAccountTokenReader,
};

use crate::runtime::config::{CredentialState, InternalTokenProvider};

/// Failure to build the outbound platform-plane credential from an
/// [`InternalAuthConfig`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OutboundCredentialError {
    /// The config cannot supply an outbound credential (no kube `present`
    /// block, an empty token path, or an unusable shared secret), per
    /// [`InternalAuthConfig::outbound`].
    #[error(transparent)]
    Config(#[from] InvalidOutboundAuth),
    /// The projected service-account token could not be read (or was empty) on
    /// the initial load.
    #[error("cannot read projected service-account token at {}: {source}", path.display())]
    TokenUnreadable {
        /// The configured `present.token_path`.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// Build the outbound platform-plane [`InternalTokenProvider`] from `cfg`. Call
/// it only from code that sends platform-plane calls: it always yields a
/// credential, or an error.
///
/// - `shared_secret` → a provider that always yields the secret.
/// - `kube` with a `present` block → a provider backed by a
///   [`ServiceAccountTokenReader`]: the token at `present.token_path` is read
///   once, then re-read every [`DEFAULT_REFRESH_INTERVAL`] until a child of
///   `cancel` is cancelled (or every clone of the provider is dropped). A failed
///   or empty re-read keeps the previous token.
/// - `kube` without a `present` block → an error.
///
/// # Errors
/// - [`OutboundCredentialError::Config`] when [`InternalAuthConfig::outbound`]
///   refuses the config: a `shared_secret` that is empty or the redaction
///   placeholder, or `kube` without a `present` block or with an empty path.
/// - [`OutboundCredentialError::TokenUnreadable`] when `present.token_path`
///   cannot be read, or is empty, on the initial load.
pub async fn build_internal_token_provider(
    cfg: &InternalAuthConfig,
    cancel: &CancellationToken,
) -> Result<InternalTokenProvider, OutboundCredentialError> {
    build_with_interval(cfg, cancel, DEFAULT_REFRESH_INTERVAL).await
}

/// [`build_internal_token_provider`] with a custom refresh `interval` (for tests).
async fn build_with_interval(
    cfg: &InternalAuthConfig,
    cancel: &CancellationToken,
    interval: Duration,
) -> Result<InternalTokenProvider, OutboundCredentialError> {
    // `OutboundSpec` is exhaustive, so a new provider variant fails to compile
    // here rather than silently fall through to "no outbound credential".
    match cfg.outbound()? {
        OutboundSpec::SharedSecret(secret) => Ok(InternalTokenProvider::from_token(secret.clone())),
        OutboundSpec::ProjectedToken(token_path) => {
            let reader = ServiceAccountTokenReader::with_cancellation(
                token_path,
                interval,
                cancel.child_token(),
            )
            .await
            .map_err(|source| OutboundCredentialError::TokenUnreadable {
                path: token_path.to_path_buf(),
                source,
            })?;
            // The initial read succeeded and re-reads keep the previous value on
            // failure, so the reader should never yield `None` here. If it ever
            // does, that is a broken source (`Unavailable`, warned), not an
            // opt-out (`NotConfigured`, silent).
            let token_fn = reader.token_provider();
            Ok(InternalTokenProvider::new(move || match token_fn() {
                Some(token) => CredentialState::Available(token),
                None => CredentialState::Unavailable(
                    "projected service-account token is currently unavailable \
                     (file empty or not yet read)"
                        .into(),
                ),
            }))
        }
    }
}

/// Adapt `provider` to a tonic client interceptor that attaches the credential
/// to every outgoing request, under the shared attach policy
/// ([`InternalTokenProvider::resolve_for_attach`]):
///
/// - `None` / `NotConfigured` → attach nothing, silently (Profile 1).
/// - `Available` → attach the token.
/// - `Unavailable` → attach nothing but warn, naming `client`. A broken
///   credential source is not an intentional opt-out, and failing the call
///   instead would take the fleet down when a token file goes briefly empty.
///
/// The provider is invoked per request, so a rotating token is picked up
/// without rebuilding the channel. `client` names the caller in the warning:
/// the interceptor runs before the method is known, so there is no per-RPC
/// label.
#[must_use]
pub fn internal_auth_interceptor(
    provider: Option<&InternalTokenProvider>,
    client: &'static str,
) -> InternalAuthInterceptor {
    match provider {
        Some(provider) => {
            let provider = provider.clone();
            InternalAuthInterceptor::new(move || {
                InternalTokenProvider::resolve_for_attach(Some(&provider), client)
            })
        }
        None => InternalAuthInterceptor::disabled(),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use secrecy::{ExposeSecret, SecretString};
    use tonic::service::Interceptor as _;
    use toolkit_security::{InvalidSharedSecret, KubePresent, KubeVerify};

    fn unique_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("pc-{tag}-{}-{n}", std::process::id()))
    }

    fn kube(token_path: Option<PathBuf>) -> InternalAuthConfig {
        InternalAuthConfig::Kube {
            verify: Some(KubeVerify {
                audiences: vec!["toolkit-internal".to_owned()],
            }),
            present: token_path.map(|token_path| KubePresent { token_path }),
        }
    }

    fn shared_secret(secret: &str) -> InternalAuthConfig {
        InternalAuthConfig::SharedSecret {
            secret: secret.to_owned().into(),
            peer_name: "toolkit-internal".to_owned(),
        }
    }

    /// The token `provider` currently hands out, if `Available`.
    fn available(provider: &InternalTokenProvider) -> Option<String> {
        match provider.current() {
            CredentialState::Available(token) => Some(token.expose_secret().to_owned()),
            CredentialState::NotConfigured | CredentialState::Unavailable(_) => None,
        }
    }

    /// The token `interceptor` attaches to a request, if any.
    fn attached(interceptor: &mut InternalAuthInterceptor) -> Option<String> {
        let request = interceptor
            .call(tonic::Request::new(()))
            .expect("interceptor runs");
        toolkit_transport_grpc::extract_internal_token_grpc(request.metadata())
            .ok()
            .map(|t| t.expose_secret().to_owned())
    }

    #[tokio::test]
    async fn shared_secret_yields_the_secret() {
        let provider =
            build_internal_token_provider(&shared_secret("shared-tok"), &CancellationToken::new())
                .await
                .expect("shared secret builds");
        assert_eq!(available(&provider).as_deref(), Some("shared-tok"));
    }

    #[tokio::test]
    async fn unusable_shared_secret_is_refused() {
        for (secret, expected) in [
            ("", InvalidSharedSecret::Empty),
            (
                toolkit_security::REDACTED_PLACEHOLDER,
                InvalidSharedSecret::RedactedPlaceholder,
            ),
        ] {
            let err =
                build_internal_token_provider(&shared_secret(secret), &CancellationToken::new())
                    .await
                    .expect_err("an unusable secret must not become a credential");
            match err {
                OutboundCredentialError::Config(InvalidOutboundAuth::SharedSecret(got)) => {
                    assert_eq!(got, expected);
                }
                other => panic!("expected an invalid shared secret, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn kube_without_present_block_is_refused() {
        let err = build_internal_token_provider(&kube(None), &CancellationToken::new())
            .await
            .expect_err("a caller that sends platform calls must not get an empty credential");
        assert!(
            matches!(
                err,
                OutboundCredentialError::Config(InvalidOutboundAuth::MissingKubePresent)
            ),
            "got {err:?}"
        );
        assert!(err.to_string().contains("present"), "got: {err}");
    }

    #[tokio::test]
    async fn kube_with_present_block_reads_the_projected_token() {
        let dir = unique_dir("kube");
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        let path = dir.join("token");
        tokio::fs::write(&path, "kube.sa.jwt").await.expect("write");

        let provider = build_internal_token_provider(&kube(Some(path)), &CancellationToken::new())
            .await
            .expect("builds rotating provider");
        assert_eq!(available(&provider).as_deref(), Some("kube.sa.jwt"));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn unreadable_token_path_is_named_in_the_error() {
        let path = unique_dir("kube-missing").join("token");
        let err =
            build_internal_token_provider(&kube(Some(path.clone())), &CancellationToken::new())
                .await
                .expect_err("a missing token file must error");
        match &err {
            OutboundCredentialError::TokenUnreadable { path: p, source } => {
                assert_eq!(p, &path);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected TokenUnreadable, got {other:?}"),
        }
        assert!(
            err.to_string().contains(&path.display().to_string()),
            "error must name the token file: {err}"
        );
    }

    #[tokio::test]
    async fn refresh_task_stops_rereading_after_cancellation() {
        const INTERVAL: Duration = Duration::from_millis(20);

        let dir = unique_dir("kube-cancel");
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        let path = dir.join("token");
        tokio::fs::write(&path, "token-v1").await.expect("write");

        let cancel = CancellationToken::new();
        let provider = build_with_interval(&kube(Some(path.clone())), &cancel, INTERVAL)
            .await
            .expect("builds rotating provider");

        // Before cancellation the task re-reads the file (rotation is seen).
        tokio::fs::write(&path, "token-v2").await.expect("rotate");
        let mut observed = None;
        for _ in 0..100 {
            tokio::time::sleep(INTERVAL).await;
            observed = available(&provider);
            if observed.as_deref() == Some("token-v2") {
                break;
            }
        }
        assert_eq!(observed.as_deref(), Some("token-v2"), "rotation observed");

        // After cancellation it never re-reads, even though the provider (the
        // only strong holder) is still alive.
        cancel.cancel();
        // Let an in-flight read (if any) finish so the task observes the cancel.
        tokio::time::sleep(INTERVAL * 3).await;
        tokio::fs::write(&path, "token-v3").await.expect("rotate");
        tokio::time::sleep(INTERVAL * 10).await;
        assert_eq!(
            available(&provider).as_deref(),
            Some("token-v2"),
            "the file must not be re-read after cancellation"
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    /// The interceptor is a thin adapter: it attaches exactly what the provider
    /// hands out, so the gRPC carrier and the provider never disagree.
    #[tokio::test]
    async fn interceptor_attaches_the_providers_token() {
        let provider =
            build_internal_token_provider(&shared_secret("shared-tok"), &CancellationToken::new())
                .await
                .expect("shared secret builds");
        let mut interceptor = internal_auth_interceptor(Some(&provider), "test");
        assert_eq!(attached(&mut interceptor).as_deref(), Some("shared-tok"));
    }

    #[test]
    fn interceptor_follows_the_attach_policy() {
        let mut none = internal_auth_interceptor(None, "test");
        assert_eq!(attached(&mut none), None, "no provider attaches nothing");

        let not_configured = InternalTokenProvider::new(|| CredentialState::NotConfigured);
        let mut interceptor = internal_auth_interceptor(Some(&not_configured), "test");
        assert_eq!(attached(&mut interceptor), None);

        let unavailable =
            InternalTokenProvider::new(|| CredentialState::Unavailable("file empty".into()));
        let mut interceptor = internal_auth_interceptor(Some(&unavailable), "test");
        assert_eq!(
            attached(&mut interceptor),
            None,
            "unavailable sends no token"
        );

        let rotating = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = std::sync::Arc::clone(&rotating);
        let rotating = InternalTokenProvider::new(move || {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            CredentialState::Available(SecretString::from(format!("token-{n}")))
        });
        let mut interceptor = internal_auth_interceptor(Some(&rotating), "test");
        assert_eq!(attached(&mut interceptor).as_deref(), Some("token-0"));
        assert_eq!(
            attached(&mut interceptor).as_deref(),
            Some("token-1"),
            "the provider is resolved per request"
        );
    }
}
