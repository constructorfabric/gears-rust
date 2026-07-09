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
//! `Interceptor` trait — and its enforcement is installed at the bootstrap /
//! orchestrator wiring layer, reusing
//! [`extract_internal_token_grpc`] together with the
//! `toolkit_security::InternalAuthenticator` trait.

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::service::Interceptor;
use tonic::{Request, Status};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

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
