//! Platform-plane (workload) authentication over gRPC.
//!
//! System-initiated calls (e.g. a gear registering with `DirectoryService`)
//! carry the platform-plane credential in the `x-toolkit-internal-token`
//! metadata key — **never** in `authorization`, to avoid colliding with the
//! tenant-plane user JWT.
//!
//! This module provides the **outbound** path used by gears calling system
//! services:
//! - [`attach_internal_token_grpc`] / [`extract_internal_token_grpc`] — the
//!   metadata helpers (symmetric with `attach_secctx` / `extract_secctx`).
//! - [`InternalAuthInterceptor`] — a tonic **client** interceptor that attaches
//!   the gear's current internal credential to every outgoing request.
//!
//! Inbound server-side validation (e.g. protecting `DirectoryService`
//! register/resolve RPCs) is **async** — it cannot use tonic's synchronous
//! `Interceptor` trait — and is provided as a Tower layer,
//! [`InternalAuthGrpcLayer`](crate::internal_auth_server::InternalAuthGrpcLayer),
//! installed on the tonic `Server`. It reuses the same
//! `toolkit_security::InternalAuthenticator` trait as the outbound path.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use tokio_util::sync::CancellationToken;
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::service::Interceptor;
use tonic::{Request, Status};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{
    InternalAuthConfig, InvalidSharedSecret, KubePresent, validate_shared_secret,
};

use crate::sa_token::{DEFAULT_REFRESH_INTERVAL, ServiceAccountTokenReader};

/// Attach a platform-plane internal `token` to outgoing gRPC metadata under the
/// `x-toolkit-internal-token` key.
///
/// # Errors
///
/// Returns `Status::internal` if the token cannot be represented as an ASCII
/// metadata value.
pub fn attach_internal_token_grpc(
    meta: &mut MetadataMap,
    token: &SecretString,
) -> Result<(), Status> {
    // Expose the secret only here, at the transport boundary, and never log it.
    let value = MetadataValue::try_from(token.expose_secret())
        .map_err(|_| Status::internal("invalid internal token metadata value"))?;
    meta.insert(INTERNAL_TOKEN_HEADER, value);
    Ok(())
}

/// Extract the raw platform-plane internal token from gRPC metadata.
///
/// Surrounding whitespace is trimmed and an empty token is rejected. The token
/// is opaque to the transport — it is validated by the receiving
/// `InternalAuthenticator`.
///
/// # Errors
///
/// Returns `Status::unauthenticated` if the metadata is absent, not valid
/// ASCII, or carries an empty token.
pub fn extract_internal_token_grpc(meta: &MetadataMap) -> Result<SecretString, Status> {
    let value = meta
        .get(INTERNAL_TOKEN_HEADER)
        .ok_or_else(|| Status::unauthenticated("missing internal token metadata"))?;
    let raw = value
        .to_str()
        .map_err(|_| Status::unauthenticated("invalid internal token metadata"))?;

    let token = raw.trim();
    if token.is_empty() {
        return Err(Status::unauthenticated("empty internal token"));
    }

    Ok(SecretString::from(token))
}

/// Source of the current platform-plane credential.
///
/// Invoked on every outgoing request so the interceptor always attaches the
/// *current* token across rotation (a projected K8s SA token is re-read by the
/// reader at the bootstrap layer). Returning `None` attaches no header — used
/// for Profile 1 (`InternalCredential::None`).
type TokenProvider = Arc<dyn Fn() -> Option<SecretString> + Send + Sync>;

/// tonic **client** interceptor that attaches the gear's platform-plane
/// credential to outgoing system calls.
///
/// Use via the generated client's `with_interceptor`, e.g.
/// `DirectoryServiceClient::with_interceptor(channel, interceptor)`.
#[derive(Clone)]
pub struct InternalAuthInterceptor {
    token_provider: TokenProvider,
}

impl InternalAuthInterceptor {
    /// Build an interceptor whose credential is resolved by `provider` on each
    /// call (supports rotation; `None` attaches no header).
    #[must_use]
    pub fn new(provider: impl Fn() -> Option<SecretString> + Send + Sync + 'static) -> Self {
        Self {
            token_provider: Arc::new(provider),
        }
    }

    /// Build an interceptor that always attaches the given static `token`.
    ///
    /// Suitable for a non-rotating credential (e.g. a Profile 2 bootstrap
    /// token); prefer [`InternalAuthInterceptor::new`] for rotating SA tokens.
    #[must_use]
    pub fn from_token(token: SecretString) -> Self {
        Self::new(move || Some(token.clone()))
    }

    /// Build an interceptor that attaches nothing (Profile 1 /
    /// `InternalCredential::None`).
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(|| None)
    }
}

/// Failure to resolve the outbound platform-plane credential from an
/// [`InternalAuthConfig`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OutboundCredentialError {
    /// `provider=kube` has no `present` block. Only code that sends
    /// platform-plane calls builds an outbound credential, so for it this is a
    /// configuration error rather than "send nothing".
    #[error(
        "internal_auth provider=kube has no `present:` block, but this participant sends \
         platform-plane calls; set `present.token_path`"
    )]
    MissingKubePresent,
    /// The `shared_secret` would be attached as-is but can never authenticate:
    /// it is empty or the `<redacted>` placeholder a serialized config writes.
    /// Refused here, as the inbound validator refuses it, instead of surfacing
    /// as a remote `Unauthenticated`.
    #[error("internal_auth provider=shared_secret is unusable as an outbound credential: {0}")]
    InvalidSharedSecret(#[from] InvalidSharedSecret),
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

/// The outbound platform-plane credential source resolved from an
/// [`InternalAuthConfig`] — the single place that maps provider config to an
/// outbound credential.
///
/// Callers that need more than an interceptor (e.g. the `OoP` bootstrap, which
/// also derives a transport-agnostic token provider from the same source) match
/// on the variants; everyone else uses
/// [`interceptor`](Self::interceptor) or [`build_outbound_interceptor`].
pub enum OutboundCredentialSource {
    /// A static shared secret (`provider=shared_secret`).
    Static(SecretString),
    /// A rotating projected service-account token (`provider=kube` with a
    /// `present` block). Its refresh task is bound to the caller's
    /// [`CancellationToken`].
    Rotating(ServiceAccountTokenReader),
}

impl OutboundCredentialSource {
    /// Resolve the outbound credential source from `cfg`.
    ///
    /// For `kube`, the projected token at `present.token_path` is read once and
    /// a refresh task re-reads it every [`DEFAULT_REFRESH_INTERVAL`] until a
    /// child of `cancel` is cancelled (or every interceptor/provider built from
    /// it is dropped).
    ///
    /// # Errors
    /// - [`OutboundCredentialError::InvalidSharedSecret`] for a `shared_secret`
    ///   that is empty or the redaction placeholder.
    /// - [`OutboundCredentialError::MissingKubePresent`] for `kube` without a
    ///   `present` block.
    /// - [`OutboundCredentialError::TokenUnreadable`] when `present.token_path`
    ///   cannot be read, or is empty, on the initial load.
    pub async fn from_config(
        cfg: &InternalAuthConfig,
        cancel: &CancellationToken,
    ) -> Result<Self, OutboundCredentialError> {
        Self::from_config_with_interval(cfg, cancel, DEFAULT_REFRESH_INTERVAL).await
    }

    /// [`from_config`](Self::from_config) with a custom refresh `interval`
    /// (for tests).
    async fn from_config_with_interval(
        cfg: &InternalAuthConfig,
        cancel: &CancellationToken,
        interval: Duration,
    ) -> Result<Self, OutboundCredentialError> {
        // Exhaustive on purpose: a new provider variant must fail to compile
        // here rather than silently fall through to "no outbound credential".
        match cfg {
            InternalAuthConfig::SharedSecret { secret, .. } => {
                validate_shared_secret(secret)?;
                Ok(Self::Static(secret.clone()))
            }
            InternalAuthConfig::Kube {
                present: Some(KubePresent { token_path }),
                ..
            } => {
                let reader = ServiceAccountTokenReader::with_cancellation(
                    token_path,
                    interval,
                    cancel.child_token(),
                )
                .await
                .map_err(|source| OutboundCredentialError::TokenUnreadable {
                    path: token_path.clone(),
                    source,
                })?;
                Ok(Self::Rotating(reader))
            }
            InternalAuthConfig::Kube { present: None, .. } => {
                Err(OutboundCredentialError::MissingKubePresent)
            }
        }
    }

    /// Build the outbound gRPC interceptor for this source.
    #[must_use]
    pub fn interceptor(&self) -> InternalAuthInterceptor {
        match self {
            Self::Static(token) => InternalAuthInterceptor::from_token(token.clone()),
            Self::Rotating(reader) => reader.interceptor(),
        }
    }
}

impl fmt::Debug for OutboundCredentialSource {
    // Manual impl: never format the credential itself.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static(_) => f.write_str("Static(<redacted>)"),
            Self::Rotating(reader) => f
                .debug_struct("Rotating")
                .field("path", &reader.path())
                .finish(),
        }
    }
}

/// Build the **outbound** platform-plane interceptor from an
/// [`InternalAuthConfig`]. Call it only from code that sends platform-plane
/// calls: it always yields a credential, or an error.
///
/// - `shared_secret` → a static interceptor attaching the shared secret
///   ([`OutboundCredentialError::InvalidSharedSecret`] if it is empty or the
///   redaction placeholder).
/// - `kube` with a `present` block → a rotating interceptor backed by a
///   [`ServiceAccountTokenReader`] whose refresh task stops when `cancel` (via
///   a child token) is cancelled.
/// - `kube` without a `present` block →
///   [`OutboundCredentialError::MissingKubePresent`].
///
/// # Errors
/// See [`OutboundCredentialSource::from_config`].
pub async fn build_outbound_interceptor(
    cfg: &InternalAuthConfig,
    cancel: &CancellationToken,
) -> Result<InternalAuthInterceptor, OutboundCredentialError> {
    Ok(OutboundCredentialSource::from_config(cfg, cancel)
        .await?
        .interceptor())
}

impl Interceptor for InternalAuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = (self.token_provider)() {
            attach_internal_token_grpc(request.metadata_mut(), &token)?;
        }
        Ok(request)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn attach_extract_round_trip() {
        let mut meta = MetadataMap::new();
        attach_internal_token_grpc(&mut meta, &SecretString::from("sa.jwt.token"))
            .expect("attach succeeds");

        let token = extract_internal_token_grpc(&meta).expect("token extracted");
        assert_eq!(token.expose_secret(), "sa.jwt.token");
    }

    #[test]
    fn attach_uses_dedicated_key_not_authorization() {
        let mut meta = MetadataMap::new();
        attach_internal_token_grpc(&mut meta, &SecretString::from("sa.jwt.token"))
            .expect("attach succeeds");

        assert!(meta.get(INTERNAL_TOKEN_HEADER).is_some());
        assert!(meta.get("authorization").is_none());
    }

    #[test]
    fn extract_missing_metadata() {
        let meta = MetadataMap::new();
        let err = extract_internal_token_grpc(&meta).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn extract_empty_token() {
        let mut meta = MetadataMap::new();
        meta.insert(INTERNAL_TOKEN_HEADER, MetadataValue::from_static("   "));
        let err = extract_internal_token_grpc(&meta).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn interceptor_attaches_provided_token() {
        let mut interceptor = InternalAuthInterceptor::from_token(SecretString::from("sa.tok"));
        let request = interceptor
            .call(Request::new(()))
            .expect("interceptor succeeds");
        assert_eq!(
            extract_internal_token_grpc(request.metadata())
                .unwrap()
                .expose_secret(),
            "sa.tok"
        );
    }

    #[test]
    fn interceptor_disabled_attaches_nothing() {
        let mut interceptor = InternalAuthInterceptor::disabled();
        let request = interceptor
            .call(Request::new(()))
            .expect("interceptor succeeds");
        assert!(request.metadata().get(INTERNAL_TOKEN_HEADER).is_none());
    }

    fn unique_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ia-{tag}-{}-{n}", std::process::id()))
    }

    fn kube(token_path: Option<PathBuf>) -> InternalAuthConfig {
        InternalAuthConfig::Kube {
            verify: Some(toolkit_security::KubeVerify {
                audiences: vec!["toolkit-internal".to_owned()],
            }),
            present: token_path.map(|token_path| KubePresent { token_path }),
        }
    }

    fn attached_token(interceptor: &mut InternalAuthInterceptor) -> Option<String> {
        let request = interceptor
            .call(Request::new(()))
            .expect("interceptor runs");
        extract_internal_token_grpc(request.metadata())
            .ok()
            .map(|t| t.expose_secret().to_owned())
    }

    #[tokio::test]
    async fn build_from_shared_secret_attaches_static_token() {
        let cfg = InternalAuthConfig::SharedSecret {
            secret: "shared-tok".to_owned().into(),
            peer_name: "toolkit-internal".to_owned(),
        };
        let mut interceptor = build_outbound_interceptor(&cfg, &CancellationToken::new())
            .await
            .expect("builds shared-secret interceptor");
        assert_eq!(
            attached_token(&mut interceptor).as_deref(),
            Some("shared-tok")
        );
    }

    #[tokio::test]
    async fn build_from_unusable_shared_secret_errors() {
        for (secret, expected) in [
            ("", InvalidSharedSecret::Empty),
            (
                toolkit_security::REDACTED_PLACEHOLDER,
                InvalidSharedSecret::RedactedPlaceholder,
            ),
        ] {
            let cfg = InternalAuthConfig::SharedSecret {
                secret: secret.to_owned().into(),
                peer_name: "toolkit-internal".to_owned(),
            };
            let err = build_outbound_interceptor(&cfg, &CancellationToken::new())
                .await
                .err()
                .expect("an unusable secret must not become a credential");
            match err {
                OutboundCredentialError::InvalidSharedSecret(got) => assert_eq!(got, expected),
                other => panic!("expected InvalidSharedSecret, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn build_from_kube_without_present_block_errors() {
        let err = build_outbound_interceptor(&kube(None), &CancellationToken::new())
            .await
            .err()
            .expect("a caller that sends platform calls must not get a disabled interceptor");
        assert!(
            matches!(err, OutboundCredentialError::MissingKubePresent),
            "got {err:?}"
        );
        assert!(err.to_string().contains("present"), "got: {err}");
    }

    #[tokio::test]
    async fn build_from_kube_with_token_path_reads_projected_token() {
        let dir = unique_dir("kube");
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        let path = dir.join("token");
        tokio::fs::write(&path, "kube.sa.jwt").await.expect("write");

        let mut interceptor =
            build_outbound_interceptor(&kube(Some(path)), &CancellationToken::new())
                .await
                .expect("builds rotating interceptor");
        assert_eq!(
            attached_token(&mut interceptor).as_deref(),
            Some("kube.sa.jwt")
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn build_from_kube_with_unreadable_token_path_names_the_path() {
        let path = unique_dir("kube-missing").join("token");
        let err = build_outbound_interceptor(&kube(Some(path.clone())), &CancellationToken::new())
            .await
            .err()
            .expect("a missing token file must error");
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
        let source = OutboundCredentialSource::from_config_with_interval(
            &kube(Some(path.clone())),
            &cancel,
            INTERVAL,
        )
        .await
        .expect("builds rotating source");
        let mut interceptor = source.interceptor();
        drop(source);

        // Before cancellation the task re-reads the file (rotation is seen).
        tokio::fs::write(&path, "token-v2").await.expect("rotate");
        let mut observed = None;
        for _ in 0..100 {
            tokio::time::sleep(INTERVAL).await;
            observed = attached_token(&mut interceptor);
            if observed.as_deref() == Some("token-v2") {
                break;
            }
        }
        assert_eq!(observed.as_deref(), Some("token-v2"), "rotation observed");

        // After cancellation it never re-reads, even though the interceptor
        // (the only strong holder) is still alive.
        cancel.cancel();
        // Let an in-flight read (if any) finish so the task observes the cancel.
        tokio::time::sleep(INTERVAL * 3).await;
        tokio::fs::write(&path, "token-v3").await.expect("rotate");
        tokio::time::sleep(INTERVAL * 10).await;
        assert_eq!(
            attached_token(&mut interceptor).as_deref(),
            Some("token-v2"),
            "the file must not be re-read after cancellation"
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[test]
    fn source_debug_redacts_the_secret() {
        let source = OutboundCredentialSource::Static(SecretString::from("super-secret"));
        assert!(!format!("{source:?}").contains("super-secret"));
    }

    #[test]
    fn interceptor_provider_reads_current_token() {
        // Provider returns a different token each call (rotation simulation).
        let counter = std::sync::atomic::AtomicU32::new(0);
        let counter = Arc::new(counter);
        let provider_counter = Arc::clone(&counter);
        let mut interceptor = InternalAuthInterceptor::new(move || {
            let n = provider_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(SecretString::from(format!("token-{n}")))
        });

        let first = interceptor.call(Request::new(())).unwrap();
        let second = interceptor.call(Request::new(())).unwrap();
        assert_eq!(
            extract_internal_token_grpc(first.metadata())
                .unwrap()
                .expose_secret(),
            "token-0"
        );
        assert_eq!(
            extract_internal_token_grpc(second.metadata())
                .unwrap()
                .expose_secret(),
            "token-1"
        );
    }
}
