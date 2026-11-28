use std::future::{Future, poll_fn, ready};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use anyhow::{Context as _, Result};
use http_body_util::{BodyExt as _, Full};
use prost::Message as _;
use tokio::sync::Notify;
use tokio::time::{advance, timeout};
use tonic::body::Body;
use tonic::codegen::{Bytes, Service as _};
use tonic::metadata::MetadataValue;
use tonic::{Code, Response};
use toolkit_security::PlatformIdentity;
use uuid::{Uuid, uuid};

use super::*;

const SUBJECT: Uuid = uuid!("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
const TENANT: Uuid = uuid!("00000000-0000-0000-0000-000000000002");
const BOUND: Duration = Duration::from_secs(2);
const GATE_BOUND: Duration = Duration::from_secs(7);
const METHOD: &str = "/test_flight_control.v1.BearerValidation/Authenticate";
const PLANTED: &str = "planted-private-bearer-unit-test";

#[derive(Clone, Copy)]
enum Markers {
    Neither,
    Peer,
    Platform,
    Outbound,
    Both,
}

fn request(bearer: &str, markers: Markers) -> Request<AuthenticateRequest> {
    let mut request = Request::new(AuthenticateRequest {
        bearer_token: bearer.into(),
    });
    if matches!(markers, Markers::Peer | Markers::Outbound | Markers::Both) {
        request.extensions_mut().insert(PeerAuthenticated {
            name: "e2e-platform".into(),
        });
    }
    if matches!(markers, Markers::Platform | Markers::Both) {
        request
            .extensions_mut()
            .insert(PlatformSecurityContext::new(PlatformIdentity::Shared {
                name: "e2e-platform".into(),
            }));
    }
    if matches!(markers, Markers::Outbound) {
        request
            .extensions_mut()
            .insert(PlatformSecurityContext::outbound_marker());
    }
    request
}

struct Resolves<F>(F);

impl<F> BearerAuthenticator for Resolves<F>
where
    F: Fn(&str) -> Result<SecurityContext, AuthNError> + Send + Sync,
{
    fn authenticate(
        &self,
        bearer: &str,
    ) -> impl Future<Output = Result<SecurityContext, AuthNError>> + Send {
        ready((self.0)(bearer))
    }
}

fn stub<F>(resolve: F) -> Arc<DynBearerAuthenticator>
where
    F: Fn(&str) -> Result<SecurityContext, AuthNError> + Send + Sync + 'static,
{
    Arc::new(DynBearerAuthenticator::new(Resolves(resolve)))
}

fn identity() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(TENANT)
        .subject_type("user")
        .token_scopes(vec!["read:data".into(), "write:data".into()])
        .bearer_token(PLANTED)
        .build()
        .unwrap_or_else(|_| panic!("fixed identity"))
}

#[tokio::test]
async fn a_private_request_requires_both_platform_markers_before_authentication() -> Result<()> {
    let lookups = AtomicUsize::new(0);
    let calls = Arc::new(AtomicUsize::new(0));
    let authenticator = stub({
        let calls = Arc::clone(&calls);
        move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(identity())
        }
    });
    let lookup = || {
        lookups.fetch_add(1, Ordering::Relaxed);
        Some(Arc::clone(&authenticator))
    };

    for markers in [
        Markers::Neither,
        Markers::Peer,
        Markers::Platform,
        Markers::Outbound,
    ] {
        let status = Bridge::authenticate_request(request(PLANTED, markers), lookup)
            .await
            .err()
            .context("expected missing platform authentication refusal")?;
        assert_eq!(status.code(), Code::Unauthenticated);
        assert_eq!(status.message(), "platform authentication required");
        assert!(status.details().is_empty());
        assert_eq!(lookups.load(Ordering::Relaxed), 0);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    let mut authorization = request(PLANTED, Markers::Both);
    authorization.metadata_mut().insert(
        AUTHORIZATION,
        MetadataValue::from_static("Bearer planted-private-bearer-unit-test"),
    );
    let mut security_context = request(PLANTED, Markers::Both);
    security_context
        .metadata_mut()
        .insert_bin(SECCTX_METADATA_KEY, MetadataValue::from_bytes(b"test"));
    for mixed in [authorization, security_context] {
        let status = Bridge::authenticate_request(mixed, lookup)
            .await
            .err()
            .context("expected mixed metadata refusal")?;
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "mixed authentication metadata");
        assert!(status.details().is_empty());
        assert_eq!(lookups.load(Ordering::Relaxed), 0);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    assert_eq!(
        Bridge::authenticate_request(request(PLANTED, Markers::Both), lookup).await?,
        AuthenticateResponse {
            outcome: Some(Outcome::Identity(Identity {
                subject_id: SUBJECT.to_string(),
                subject_type: Some("user".into()),
                subject_tenant_id: TENANT.to_string(),
                token_scopes: vec!["read:data".into(), "write:data".into()],
            })),
        },
    );
    assert_eq!(lookups.load(Ordering::Relaxed), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    Ok(())
}

#[tokio::test]
async fn a_private_response_preserves_identity_or_returns_a_typed_failure() -> Result<()> {
    let hub = Arc::new(ClientHub::new());
    let bridge = Bridge {
        hub: Arc::clone(&hub),
    };
    assert_eq!(
        bridge
            .authenticate(request(PLANTED, Markers::Both))
            .await?
            .into_inner(),
        AuthenticateResponse {
            outcome: Some(Outcome::Failure(Failure::Unavailable.into())),
        },
    );

    hub.register::<DynBearerAuthenticator>(stub(|bearer| match bearer {
        "known" => Ok(identity()),
        "nil" => Ok(SecurityContext::anonymous()),
        "invalid" => Err(AuthNError::InvalidToken),
        "offline" => Err(AuthNError::Unavailable),
        _ => Err(AuthNError::Other(PLANTED.into())),
    }));
    for (bearer, expected) in [
        (
            "known",
            Outcome::Identity(Identity {
                subject_id: SUBJECT.to_string(),
                subject_type: Some("user".into()),
                subject_tenant_id: TENANT.to_string(),
                token_scopes: vec!["read:data".into(), "write:data".into()],
            }),
        ),
        (
            "nil",
            Outcome::Identity(Identity {
                subject_id: Uuid::nil().to_string(),
                subject_type: None,
                subject_tenant_id: Uuid::nil().to_string(),
                token_scopes: vec![],
            }),
        ),
        ("invalid", Outcome::Failure(Failure::InvalidToken.into())),
        ("offline", Outcome::Failure(Failure::Unavailable.into())),
        (PLANTED, Outcome::Failure(Failure::Internal.into())),
    ] {
        let response = bridge.authenticate(request(bearer, Markers::Both)).await?;
        assert!(!format!("{response:?}").contains(PLANTED));
        assert_eq!(
            response.into_inner(),
            AuthenticateResponse {
                outcome: Some(expected)
            }
        );
    }

    assert_eq!(
        format!("{:?}", request(PLANTED, Markers::Both).get_ref()),
        "AuthenticateRequest { .. }",
    );
    Ok(())
}

#[tokio::test]
async fn a_private_request_observes_the_bearer_and_identity_bounds() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let authenticator = stub({
        let calls = Arc::clone(&calls);
        move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(identity())
        }
    });
    let lookups = AtomicUsize::new(0);
    let lookup = || {
        lookups.fetch_add(1, Ordering::Relaxed);
        Some(Arc::clone(&authenticator))
    };

    assert_bearer_limits(lookup, &lookups, &calls).await?;
    assert_identity_limits().await
}

async fn assert_bearer_limits(
    lookup: impl FnOnce() -> Option<Arc<DynBearerAuthenticator>> + Copy,
    lookups: &AtomicUsize,
    calls: &AtomicUsize,
) -> Result<()> {
    let mut accepted = 0;
    for (bearer, expected_bytes, valid) in [
        (String::new(), 0, false),
        ("a".into(), 1, true),
        ("a".repeat(16_384), 16_384, true),
        ("a".repeat(16_385), 16_385, false),
        ("\u{00e9}".repeat(8_192), 16_384, true),
        (format!("{}a", "\u{00e9}".repeat(8_192)), 16_385, false),
    ] {
        assert_eq!(bearer.len(), expected_bytes);
        let outcome = if valid {
            accepted += 1;
            Outcome::Identity(Identity {
                subject_id: SUBJECT.to_string(),
                subject_type: Some("user".into()),
                subject_tenant_id: TENANT.to_string(),
                token_scopes: vec!["read:data".into(), "write:data".into()],
            })
        } else {
            Outcome::Failure(Failure::InvalidToken.into())
        };
        let response =
            Bridge::authenticate_request(request(&bearer, Markers::Both), lookup).await?;
        assert_eq!(
            response,
            AuthenticateResponse {
                outcome: Some(outcome)
            }
        );
        assert_eq!(lookups.load(Ordering::Relaxed), accepted);
        assert_eq!(calls.load(Ordering::Relaxed), accepted);
    }
    Ok(())
}

async fn assert_identity_limits() -> Result<()> {
    for (subject_type, scopes, valid) in [
        (None, vec![], true),
        (Some(String::new()), vec!["s".into()], true),
        (Some("x".repeat(1_024)), vec!["s".into(); 64], true),
        (Some("x".repeat(1_025)), vec![], false),
        (
            Some("\u{00e9}".repeat(512)),
            vec!["\u{00e9}".repeat(128)],
            true,
        ),
        (Some(format!("{}x", "\u{00e9}".repeat(512))), vec![], false),
        (None, vec![String::new()], false),
        (None, vec!["x".repeat(256)], true),
        (None, vec!["x".repeat(257)], false),
        (None, vec![format!("{}x", "\u{00e9}".repeat(128))], false),
        (None, vec!["s".into(); 65], false),
    ] {
        let mut builder = SecurityContext::builder()
            .subject_id(SUBJECT)
            .subject_tenant_id(TENANT)
            .token_scopes(scopes.clone());
        if let Some(subject_type) = &subject_type {
            builder = builder.subject_type(subject_type);
        }
        let context = builder.build()?;
        let auth = stub(move |_| Ok(context.clone()));
        let response =
            Bridge::authenticate_request(request(PLANTED, Markers::Both), || Some(auth)).await?;
        let outcome = if valid {
            Outcome::Identity(Identity {
                subject_id: SUBJECT.to_string(),
                subject_type,
                subject_tenant_id: TENANT.to_string(),
                token_scopes: scopes,
            })
        } else {
            Outcome::Failure(Failure::Internal.into())
        };
        assert_eq!(
            response,
            AuthenticateResponse {
                outcome: Some(outcome)
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn only_valid_trace_and_request_ids_reach_authentication() -> Result<()> {
    const VALID_TRACE: &str = "00-11111111111111111111111111111111-2222222222222222-01";
    const VALID_ID: &str = "flight-control.req_1";
    let mut valid = request(PLANTED, Markers::Both);
    valid
        .metadata_mut()
        .insert(TRACEPARENT, MetadataValue::from_static(VALID_TRACE));
    valid
        .metadata_mut()
        .insert(X_REQUEST_ID, MetadataValue::from_static(VALID_ID));

    let mut invalid = request(PLANTED, Markers::Both);
    invalid.metadata_mut().insert(
        TRACEPARENT,
        MetadataValue::from_static("00-00000000000000000000000000000000-2222222222222222-01"),
    );
    invalid
        .metadata_mut()
        .insert(X_REQUEST_ID, MetadataValue::from_static("contains space"));

    let mut duplicate = request(PLANTED, Markers::Both);
    for _ in 0..2 {
        duplicate
            .metadata_mut()
            .append(TRACEPARENT, MetadataValue::from_static(VALID_TRACE));
        duplicate
            .metadata_mut()
            .append(X_REQUEST_ID, MetadataValue::from_static(VALID_ID));
    }

    let mut oversized = request(PLANTED, Markers::Both);
    oversized
        .metadata_mut()
        .insert(X_REQUEST_ID, MetadataValue::try_from("a".repeat(129))?);

    let boundary = "a".repeat(128);
    let mut maximum = request(PLANTED, Markers::Both);
    maximum
        .metadata_mut()
        .insert(X_REQUEST_ID, MetadataValue::try_from(boundary.as_str())?);

    let mut empty = request(PLANTED, Markers::Both);
    empty
        .metadata_mut()
        .insert(X_REQUEST_ID, MetadataValue::from_static(""));

    for (request, trace, id) in [
        (valid, Some(VALID_TRACE), Some(VALID_ID)),
        (maximum, None, Some(boundary.as_str())),
        (invalid, None, None),
        (duplicate, None, None),
        (oversized, None, None),
        (empty, None, None),
    ] {
        let metadata = TraceMetadata::new(request.metadata());
        assert_eq!(
            metadata
                .trace
                .get(TRACEPARENT)
                .and_then(|header| header.to_str().ok()),
            trace
        );
        let expected = id.map(str::to_owned);
        let auth = stub(move |_| {
            assert_eq!(toolkit_trace_context::current_request_id(), expected);
            Ok(SecurityContext::anonymous())
        });
        assert_eq!(
            Bridge::authenticate_request(request, || Some(auth)).await?,
            AuthenticateResponse {
                outcome: Some(Outcome::Identity(Identity {
                    subject_id: Uuid::nil().to_string(),
                    subject_type: None,
                    subject_tenant_id: Uuid::nil().to_string(),
                    token_scopes: vec![],
                })),
            },
        );
    }
    Ok(())
}

#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
    dropped: AtomicUsize,
}

struct DropProbe<'a>(&'a AtomicUsize);

impl Drop for DropProbe<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl BearerAuthenticator for Gate {
    async fn authenticate(&self, bearer: &str) -> Result<SecurityContext, AuthNError> {
        if bearer == PLANTED {
            let _probe = DropProbe(&self.dropped);
            self.entered.notify_one();
            assert!(timeout(GATE_BOUND, self.release.notified()).await.is_ok());
        }
        Ok(SecurityContext::anonymous())
    }
}

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[tokio::test(start_paused = true)]
async fn the_private_call_deadline_drops_the_authentication_future() -> Result<()> {
    let gate = Arc::new(Gate::default());
    let auth = Arc::new(DynBearerAuthenticator::from_arc(Arc::clone(&gate)));
    let lookup = || Some(Arc::clone(&auth));

    let mut pending = Box::pin(Bridge::authenticate_request(
        request(PLANTED, Markers::Both),
        lookup,
    ));
    assert!(poll_once(pending.as_mut()).await.is_pending());
    timeout(BOUND, gate.entered.notified()).await?;
    advance(Duration::from_millis(4_999)).await;
    assert!(poll_once(pending.as_mut()).await.is_pending());
    assert_eq!(gate.dropped.load(Ordering::Relaxed), 0);

    // Both branches are ready at equality. The deadline refuses even a ready identity.
    gate.release.notify_one();
    advance(Duration::from_millis(1)).await;
    assert_eq!(
        timeout(BOUND, pending).await??,
        AuthenticateResponse {
            outcome: Some(Outcome::Failure(Failure::Unavailable.into())),
        },
    );
    assert_eq!(gate.dropped.load(Ordering::Relaxed), 1);

    // A retained release permit would make the cancellation probe complete immediately.
    timeout(BOUND, gate.release.notified()).await?;
    let mut cancelled = Box::pin(Bridge::authenticate_request(
        request(PLANTED, Markers::Both),
        lookup,
    ));
    assert!(poll_once(cancelled.as_mut()).await.is_pending());
    timeout(BOUND, gate.entered.notified()).await?;
    drop(cancelled);
    assert_eq!(gate.dropped.load(Ordering::Relaxed), 2);

    assert_eq!(
        timeout(
            BOUND,
            Bridge::authenticate_request(request("valid", Markers::Both), lookup)
        )
        .await??,
        AuthenticateResponse {
            outcome: Some(Outcome::Identity(Identity {
                subject_id: Uuid::nil().to_string(),
                subject_type: None,
                subject_tenant_id: Uuid::nil().to_string(),
                token_scopes: vec![],
            })),
        },
    );
    Ok(())
}

struct CodecReply(usize);

#[tonic::async_trait]
impl BearerValidation for CodecReply {
    async fn authenticate(
        &self,
        _request: Request<AuthenticateRequest>,
    ) -> Result<Response<AuthenticateResponse>, Status> {
        let response = AuthenticateResponse {
            outcome: Some(Outcome::Identity(Identity {
                subject_id: format!("{PLANTED}{}", "a".repeat(self.0 - 8 - PLANTED.len())),
                subject_type: None,
                subject_tenant_id: String::new(),
                token_scopes: vec![],
            })),
        };
        assert_eq!(response.encoded_len(), self.0);
        Ok(Response::new(response))
    }
}

#[tokio::test]
async fn the_installed_server_caps_the_encoded_response_at_128_kib() -> Result<()> {
    for (size, grpc_status, encoded_frame) in
        [(131_072, Code::Ok, 131_077), (131_073, Code::OutOfRange, 0)]
    {
        let mut server = AuthnBridge::server(CodecReply(size));
        let request = tonic::codegen::http::Request::post(METHOD)
            .header("content-type", "application/grpc")
            .header("grpc-accept-encoding", "gzip")
            .body(Body::new(Full::new(Bytes::from_static(&[0, 0, 0, 0, 0]))))?;
        let response = timeout(BOUND, server.call(request)).await??;
        assert_eq!(response.status().as_u16(), 200);
        assert!(response.headers().get("grpc-encoding").is_none());
        let mut body = response.into_body();
        let first = timeout(BOUND, body.frame())
            .await?
            .context("gRPC body frame")??;
        let (length, completion) = match first.into_data() {
            Ok(data) => (
                data.len(),
                timeout(BOUND, body.frame())
                    .await?
                    .context("gRPC completion frame")??,
            ),
            Err(trailers) => (0, trailers),
        };
        let trailers = completion
            .into_trailers()
            .ok()
            .context("gRPC completion trailers")?;
        let status = Status::from_header_map(&trailers).context("gRPC completion status")?;
        assert_eq!(status.code(), grpc_status);
        if grpc_status == Code::Ok {
            assert_eq!(status.message(), "");
        }
        assert!(!status.message().contains(PLANTED));
        assert!(status.details().is_empty());
        assert_eq!(length, encoded_frame);
        assert!(tonic::codegen::Body::is_end_stream(&body));
    }
    Ok(())
}

#[cfg(all(feature = "core", not(feature = "event-broker")))]
#[test]
fn the_binary_links_exactly_the_core_gears_and_capabilities() -> Result<()> {
    let registry = toolkit::GearRegistry::discover_and_build()?;
    let mut gear_and_capabilities: Vec<_> = registry
        .gears()
        .iter()
        .map(|gear| {
            let mut capabilities = gear.caps().labels();
            capabilities.sort_unstable();
            (gear.name(), capabilities)
        })
        .collect();
    gear_and_capabilities.sort_unstable();
    assert_eq!(
        gear_and_capabilities,
        [
            ("api-gateway", vec!["rest", "rest_host", "stateful"]),
            ("authn-resolver", vec!["gts_publication"]),
            ("grpc-hub", vec!["grpc_hub", "stateful", "system"]),
            ("service-discovery", vec!["grpc", "rest", "system"]),
            ("static-authn-plugin", vec!["gts_publication"]),
            ("test-flight-control-authn", vec!["grpc"]),
            ("types-registry", vec!["db", "rest", "stateful", "system"]),
        ],
    );
    Ok(())
}
