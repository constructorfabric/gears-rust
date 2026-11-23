use super::*;
use serde_json::json;
use wire::authenticate_response::Outcome;
use wire::{AuthenticateResponse, Failure, Identity};

const SUBJECT: &str = "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa";
const TENANT: &str = "00000000-0000-0000-0000-000000000000";

fn identity() -> Identity {
    Identity {
        subject_id: SUBJECT.into(),
        subject_type: None,
        subject_tenant_id: TENANT.into(),
        token_scopes: vec![],
    }
}

fn mapped(identity: Identity) -> Result<SecurityContext, AuthNError> {
    decode(
        AuthenticateResponse {
            outcome: Some(Outcome::Identity(identity)),
        },
        "test-input",
    )
}

#[test]
fn the_private_request_debug_has_no_bearer_field() {
    let request = wire::AuthenticateRequest {
        bearer_token: "synthetic-debug-marker".into(),
    };
    let rendered = format!("{request:?}");
    assert!(
        !rendered.contains(&request.bearer_token),
        "synthetic marker exposed"
    );
    assert!(!rendered.contains("bearer_token"), "request field exposed");
    assert_eq!(rendered, "AuthenticateRequest { .. }");
}

#[test]
fn the_fixture_accepts_only_a_root_http_endpoint_at_a_literal_loopback_address() {
    for value in [
        "http://127.0.0.1",
        "http://[::1]/",
        "http://127.0.0.1:1024",
        "http://127.0.0.2:1024/",
        "http://[::1]:1024",
    ] {
        assert!(fixture_endpoint(value).is_ok(), "{value}");
    }
    for value in [
        "",
        "relative",
        "/relative",
        "https://127.0.0.1:1024",
        "http://localhost:1024",
        "http://192.0.2.1:1024",
        "http://127.0.0.1:1024/path",
        "http://127.0.0.1:1024/?",
        "http://127.0.0.1:1024/#fragment",
        "http://user@127.0.0.1:1024",
        "http://127.0.0.1:noport",
        "http://[::1]:65536",
        "http://127.0.0.1:",
    ] {
        assert!(
            matches!(
                fixture_endpoint(value),
                Err(OopCompositionError::InvalidConfig {
                    field: "E2E_FLIGHT_CONTROL_ENDPOINT"
                })
            ),
            "{value}"
        );
    }
}

#[test]
fn a_complete_identity_preserves_nil_tenant_absent_subject_type_and_empty_scopes() {
    let context = mapped(identity()).expect("valid identity");
    assert_eq!(
        serde_json::to_value(&context).expect("serialize context"),
        json!({"subject_id":SUBJECT,"subject_tenant_id":TENANT,"subject_type":null,"token_scopes":[]})
    );
    let mut metadata = tonic::metadata::MetadataMap::new();
    toolkit_transport_grpc::internal_auth::attach_internal_token_grpc(
        &mut metadata,
        context.bearer_token().expect("original bearer"),
    )
    .expect("test token metadata");
    assert!(
        metadata
            .get("x-toolkit-internal-token")
            .is_some_and(|value| value == "test-input"),
        "original bearer restored"
    );
}

#[test]
fn an_identity_accepts_each_exact_byte_limit_and_rejects_each_malformed_field() {
    let max_type = "a".repeat(1024);
    let max_scopes = vec!["b".repeat(256); 64];
    let good = Identity {
        subject_type: Some(max_type.clone()),
        token_scopes: max_scopes.clone(),
        ..identity()
    };
    assert_eq!(
        serde_json::to_value(mapped(good.clone()).expect("valid boundary"))
            .expect("serialize context"),
        json!({"subject_id":SUBJECT,"subject_tenant_id":TENANT,"subject_type":max_type,"token_scopes":max_scopes})
    );
    for invalid in [
        Identity {
            subject_type: Some("t".repeat(1025)),
            ..good.clone()
        },
        Identity {
            token_scopes: vec!["s".into(); 65],
            ..good.clone()
        },
        Identity {
            token_scopes: vec!["s".repeat(257)],
            ..good.clone()
        },
        Identity {
            token_scopes: vec![String::new()],
            ..good.clone()
        },
        Identity {
            subject_id: SUBJECT.replace('-', ""),
            ..good.clone()
        },
        Identity {
            subject_id: "x".repeat(36),
            ..good.clone()
        },
        Identity {
            subject_tenant_id: "x".repeat(36),
            ..good.clone()
        },
        Identity {
            subject_tenant_id: "invalid".into(),
            ..good
        },
    ] {
        assert!(
            matches!(mapped(invalid), Err(AuthNError::Other(reason)) if reason == "invalid fixture response")
        );
    }
}

#[test]
fn each_protocol_failure_maps_to_its_fixed_authentication_error() {
    for (failure, expected) in [
        (
            Some(Failure::InvalidToken as i32),
            "invalid or expired token",
        ),
        (
            Some(Failure::Unavailable as i32),
            "authentication backend unavailable",
        ),
        (
            Some(Failure::Internal as i32),
            "authentication failed: invalid fixture response",
        ),
        (Some(0), "authentication failed: invalid fixture response"),
        (Some(92), "authentication failed: invalid fixture response"),
        (None, "authentication failed: invalid fixture response"),
    ] {
        let response = AuthenticateResponse {
            outcome: failure.map(Outcome::Failure),
        };
        assert_eq!(
            decode(response, "test-input")
                .expect_err("expected denial")
                .to_string(),
            expected
        );
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use tonic::codegen::{BoxFuture, Service, http};
use tonic::{Request, Response, Status};

#[derive(Clone)]
struct ScriptedPeer {
    calls: Arc<AtomicUsize>,
    shutdown: tokio_util::sync::CancellationToken,
}

impl tonic::server::NamedService for ScriptedPeer {
    const NAME: &'static str = "test_flight_control.v1.BearerValidation";
}

impl Service<http::Request<tonic::body::Body>> for ScriptedPeer {
    type Response = http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        assert_eq!(
            request.uri().path(),
            "/test_flight_control.v1.BearerValidation/Authenticate"
        );
        let method = self.clone();
        Box::pin(async move {
            let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::default());
            Ok(grpc.unary(method, request).await)
        })
    }
}

impl tonic::server::UnaryService<wire::AuthenticateRequest> for ScriptedPeer {
    type Response = AuthenticateResponse;
    type Future = BoxFuture<Response<Self::Response>, Status>;
    fn call(&mut self, request: Request<wire::AuthenticateRequest>) -> Self::Future {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            assert!(
                request
                    .metadata()
                    .get("x-toolkit-internal-token")
                    .is_some_and(|s| s == "test-workload")
            );
            assert!(request.metadata().get("authorization").is_none());
            assert!(request.metadata().get("x-secctx-bin").is_none());
            assert!(request.metadata().get("grpc-encoding").is_none());
            assert_eq!(
                request
                    .metadata()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok()),
                Some("unit-test.request_1")
            );
            assert_eq!(
                request.metadata().contains_key("grpc-timeout"),
                request.get_ref().bearer_token.len() <= MAX_BEARER_BYTES,
                "the adapter bounds its outgoing call"
            );
            let token = request.into_inner().bearer_token;
            let result = match token.as_str() {
                "status" => return Err(Status::unauthenticated("test peer refusal")),
                "missing" => AuthenticateResponse { outcome: None },
                "wait" => {
                    const HELD_HANDLER_DEADLINE: Duration = Duration::from_secs(20);
                    tokio::time::timeout(HELD_HANDLER_DEADLINE, shutdown.cancelled())
                        .await
                        .expect("held handler exceeded its final deadline");
                    return Err(Status::cancelled("test peer stopped"));
                }
                "message-at-limit" | "message-over-limit" => {
                    use prost::Message;
                    let target = if token == "message-at-limit" {
                        131_072
                    } else {
                        131_073
                    };
                    let mut value = identity();
                    value.subject_type = Some("a".repeat(target));
                    let mut response = AuthenticateResponse {
                        outcome: Some(Outcome::Identity(value)),
                    };
                    let overhead = response.encoded_len() - target;
                    let Some(Outcome::Identity(identity)) = &mut response.outcome else {
                        panic!("fixture identity")
                    };
                    identity
                        .subject_type
                        .as_mut()
                        .expect("fixture field")
                        .truncate(target - overhead);
                    assert_eq!(response.encoded_len(), target);
                    response
                }
                _ => AuthenticateResponse {
                    outcome: Some(Outcome::Identity(identity())),
                },
            };
            Ok(Response::new(result))
        })
    }
}

const PEER_TEARDOWN: Duration = Duration::from_secs(3);

async fn join_peer(
    mut server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) -> bool {
    if let Ok(result) = tokio::time::timeout(PEER_TEARDOWN, &mut server).await {
        result.expect("peer task").expect("stop peer");
        return false;
    }
    server.abort();
    let error = tokio::time::timeout(PEER_TEARDOWN, server)
        .await
        .expect("bounded abort and reap")
        .expect_err("aborted peer");
    assert!(error.is_cancelled());
    true
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_ignores_shutdown_is_aborted_and_reaped() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback peer");
    let addr = listener.local_addr().expect("bound address");
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(ScriptedPeer {
                calls: Arc::default(),
                shutdown: tokio_util::sync::CancellationToken::new(),
            })
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    assert!(join_peer(server).await);
    let _released = tokio::net::TcpListener::bind(addr)
        .await
        .expect("reaped server releases its listener");
}

#[tokio::test]
async fn each_bounded_bearer_reaches_the_peer_once_and_wire_failures_stay_unavailable() {
    use tokio_stream::wrappers::TcpListenerStream;
    use tokio_util::sync::CancellationToken;
    const WATCHDOG: Duration = Duration::from_secs(8);
    let listener = tokio::time::timeout(WATCHDOG, tokio::net::TcpListener::bind("127.0.0.1:0"))
        .await
        .expect("peer bind deadline")
        .expect("local peer");
    let endpoint = fixture_endpoint(&format!(
        "http://{}",
        listener.local_addr().expect("bound address")
    ))
    .expect("fixture endpoint");
    let calls = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(ScriptedPeer {
                calls: calls.clone(),
                shutdown: shutdown.clone(),
            })
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(listener),
                shutdown.clone().cancelled_owned(),
            ),
    );
    let client = FlightControlBearerAuthenticator {
        endpoint,
        provider: InternalTokenProvider::from_token("test-workload".into()),
        channel: OnceLock::new(),
    };
    for token in [String::new(), "x".repeat(16_385)] {
        assert!(matches!(
            tokio::time::timeout(WATCHDOG, client.authenticate(&token))
                .await
                .expect("invalid input watchdog"),
            Err(AuthNError::InvalidToken)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(client.channel.get().is_none());
    }
    let tokens = [
        "x".to_owned(),
        "x".repeat(16_384),
        "x".to_owned(),
        "missing".into(),
        "status".into(),
        "message-at-limit".into(),
        "message-over-limit".into(),
        "wait".into(),
    ];
    for (index, token) in tokens.into_iter().enumerate() {
        let result = tokio::time::timeout(
            WATCHDOG,
            toolkit_trace_context::with_request_id(
                Some("unit-test.request_1".into()),
                client.authenticate(&token),
            ),
        )
        .await;
        let Ok(result) = result else {
            shutdown.cancel();
            drop(client);
            assert!(!join_peer(server).await);
            assert_eq!(calls.load(Ordering::Relaxed), index + 1);
            panic!("independent test watchdog expired before the adapter returned");
        };
        match token.as_str() {
            "missing" | "message-at-limit" => {
                assert!(
                    matches!(result, Err(AuthNError::Other(s)) if s == "invalid fixture response")
                );
            }
            "status" | "message-over-limit" | "wait" => {
                assert!(matches!(result, Err(AuthNError::Unavailable)));
            }
            _ => {
                let actual =
                    serde_json::to_value(result.expect("wire identity")).expect("serialize");
                assert_eq!(
                    actual,
                    json!({"subject_id":SUBJECT,"subject_type":null,"subject_tenant_id":TENANT,"token_scopes":[]})
                );
            }
        }
        assert_eq!(calls.load(Ordering::Relaxed), index + 1);
    }
    for size in [131_072, 131_073] {
        use prost::Message as _;
        let message = wire::AuthenticateRequest {
            bearer_token: "x".repeat(size - 4),
        };
        assert_eq!(message.encoded_len(), size);
        let mut request = Request::new(message);
        workload_metadata(&client.provider, request.metadata_mut()).expect("fixture credential");
        request.metadata_mut().insert(
            "x-request-id",
            "unit-test.request_1".parse().expect("request ID"),
        );
        let response = tokio::time::timeout(WATCHDOG, client.client().authenticate(request))
            .await
            .expect("codec call watchdog");
        if size == 131_072 {
            assert_eq!(
                response.expect("exact encoding limit").into_inner(),
                AuthenticateResponse {
                    outcome: Some(Outcome::Identity(identity()))
                }
            );
        } else {
            assert_eq!(
                response.expect_err("encoding limit").code(),
                tonic::Code::Internal
            );
        }
        assert_eq!(calls.load(Ordering::Relaxed), 9);
    }
    drop(client);
    shutdown.cancel();
    assert!(!join_peer(server).await);
}

#[tokio::test]
async fn a_missing_or_invalid_workload_credential_fails_before_opening_a_channel() {
    for provider in [
        InternalTokenProvider::new(|| CredentialState::NotConfigured),
        InternalTokenProvider::new(|| {
            CredentialState::Unavailable("test source unavailable".into())
        }),
        InternalTokenProvider::from_token("".into()),
        InternalTokenProvider::from_token("invalid\nwire".into()),
    ] {
        let client = FlightControlBearerAuthenticator {
            endpoint: fixture_endpoint("http://127.0.0.1:1").expect("endpoint"),
            provider,
            channel: OnceLock::new(),
        };
        assert!(matches!(
            client.authenticate("test-input").await,
            Err(AuthNError::Unavailable)
        ));
        assert!(client.channel.get().is_none());
    }
}

#[test]
fn only_valid_trace_and_request_identifiers_reach_the_fixture() {
    const TRACE: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
    let longest = "r".repeat(128);
    for (trace, id, accepted_trace, accepted_id) in [
        (TRACE, longest.clone(), Some(TRACE), Some(longest.as_str())),
        (TRACE, "r".repeat(129), Some(TRACE), None),
        (TRACE, String::new(), Some(TRACE), None),
        ("bad", "valid._-1".into(), None, Some("valid._-1")),
        (
            "00-00000000000000000000000000000000-0000000000000001-01",
            "invalid value".into(),
            None,
            None,
        ),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(TRACEPARENT, trace.parse().expect("test header syntax"));
        headers.insert(X_REQUEST_ID, id.parse().expect("test header syntax"));
        headers.insert("x-unrelated", "test-value".parse().expect("test header"));
        let mut metadata = MetadataMap::new();
        attach_correlation(&mut metadata, &headers);
        let mut expected = HeaderMap::new();
        if let Some(trace) = accepted_trace {
            expected.insert(TRACEPARENT, trace.parse().expect("valid trace"));
        }
        if let Some(id) = accepted_id {
            expected.insert(X_REQUEST_ID, id.parse().expect("valid request ID"));
        }
        assert_eq!(metadata.into_headers(), expected);
    }
}

#[tokio::test]
async fn a_credential_that_arrives_after_the_deadline_never_opens_a_channel() {
    let provider = InternalTokenProvider::new(|| {
        std::thread::sleep(Duration::from_secs(5));
        CredentialState::Available("test-workload".into())
    });
    let client = FlightControlBearerAuthenticator {
        endpoint: fixture_endpoint("http://127.0.0.1:1").expect("test endpoint"),
        provider,
        channel: OnceLock::new(),
    };
    assert!(matches!(
        client.authenticate("test-input").await,
        Err(AuthNError::Unavailable)
    ));
    assert!(client.channel.get().is_none());
}

#[test]
fn the_private_authenticator_requires_valid_workload_metadata_at_construction() {
    for provider in [
        None,
        Some(InternalTokenProvider::from_token(" ".into())),
        Some(InternalTokenProvider::from_token("invalid\nwire".into())),
    ] {
        let context = OopCompositionContext {
            cancel: tokio_util::sync::CancellationToken::new(),
            internal_token_provider: provider,
        };
        let failure = authenticator("http://127.0.0.1", &context)
            .expect_err("invalid credentials at construction");
        assert_eq!(
            failure,
            OopCompositionError::InvalidConfig {
                field: "oop_http.internal_auth"
            }
        );
    }
}
