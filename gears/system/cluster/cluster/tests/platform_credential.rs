//! The platform-plane credential actually reaches the wire, and the identity the
//! transport stamps reaches the handler (`cpt-cf-adr-two-plane-auth`, DESIGN.md).
//!
//! Every method on the cluster contract takes a `PlatformSecurityContext`, so every
//! outbound call from a `RemoteClusterClient` must carry
//! `x-toolkit-internal-token`. That is attached at the *channel*, by an interceptor
//! built in `RemoteClusterClient::connect_lazy`, rather than at each call site —
//! which makes it impossible for one RPC to quietly go out unauthenticated.
//!
//! There are two halves, and this file asserts both from the **server** side:
//!
//! - **The outbound half** — provider → interceptor → metadata → the credential
//!   arriving at the platform-plane `InternalAuthGrpcLayer`. A recording
//!   authenticator wired into the layer captures the exact token string, so the
//!   whole outbound path is under test and not any one part of it.
//! - **The inbound half** — the layer validating that credential and stamping a
//!   `PlatformSecurityContext` into the request extensions, which the handler then
//!   reads (§4.6). A recording service captures what the extension carried, which
//!   is the seam `CallerResolver::resolve` and the whole `owns()` cross-check now
//!   rest on. (Authentication itself is the layer's job, tested in
//!   `toolkit-transport-grpc`; here we assert the *cluster handler* observes its
//!   result.)

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use std::sync::Arc;
use std::sync::Mutex;

use cluster_sdk::ClusterClient;
use cluster_sdk::grpc::stubs::profile as pstub;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status};
use toolkit::contract_support::runtime::config::{CredentialState, InternalTokenProvider};
use toolkit_security::authenticator::DynInternalAuthenticator;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::internal_auth::{
    InternalAuthNError, InternalAuthenticator, PlatformIdentity,
};
use toolkit_security::{PeerAuthenticated, PlatformSecurityContext};
use toolkit_transport_grpc::InternalAuthGrpcLayer;

mod common;
use common::served_gear::{PROFILE, ServedGear, Services, served_gear};

// The outbound half: the credential reaches the platform-plane layer

/// An authenticator that records what it was given and accepts everything.
///
/// Accepting unconditionally is the point: this is not a test of validation logic,
/// it is a probe on the credential's *arrival*. Rejecting would confound "the token
/// never arrived" with "the token arrived and was refused", which are exactly the
/// two outcomes these tests need to tell apart.
#[derive(Clone, Default)]
struct RecordingAuthenticator {
    seen: Arc<Mutex<Vec<String>>>,
}

impl RecordingAuthenticator {
    fn tokens(&self) -> Vec<String> {
        self.seen.lock().expect("not poisoned").clone()
    }
}

impl InternalAuthenticator for RecordingAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        self.seen
            .lock()
            .expect("not poisoned")
            .push(token.to_owned());
        Ok(PlatformIdentity::Shared {
            name: "recorded".to_owned(),
        })
    }
}

/// A cluster gear serving only the cache service, behind an **enforcing**
/// platform-plane layer wired to `recorder`.
///
/// Cache-only on purpose: `cache_backend` is lazy, so one `get` drives one real
/// RPC and nothing else needs serving.
struct Fixture {
    gear: ServedGear,
    recorder: RecordingAuthenticator,
}

impl Fixture {
    async fn start() -> Self {
        let recorder = RecordingAuthenticator::default();
        let gear = served_gear()
            .services(Services::CACHE)
            .authenticator(DynInternalAuthenticator::new(recorder.clone()))
            .start()
            .await;

        Self { gear, recorder }
    }

    async fn stop(self) {
        self.gear.stop().await;
    }
}

/// The whole outbound path, end to end: a configured provider puts its token in
/// the metadata of a real RPC, and the layer reads back exactly that token.
#[tokio::test]
async fn a_configured_provider_attaches_its_token_to_every_call() {
    let fixture = Fixture::start().await;
    let provider = InternalTokenProvider::from_token("s3cr3t-sa-token".into());

    let client = fixture.gear.client_with(Some(&provider));
    let cache = client.cache_backend(PROFILE).expect("a handle");

    cache
        .get("ledger")
        .await
        .expect("the call is authenticated");

    assert_eq!(
        fixture.recorder.tokens(),
        vec!["s3cr3t-sa-token".to_owned()],
        "the layer must see the provider's token, byte for byte"
    );

    fixture.stop().await;
}

/// The credential is resolved per call, not captured once at channel construction —
/// which lets a projected service-account token rotate under a long-lived
/// client without a reconnect.
#[tokio::test]
async fn the_provider_is_consulted_per_call_so_a_rotating_token_is_picked_up() {
    let fixture = Fixture::start().await;

    let rotating = Arc::new(Mutex::new("token-1".to_owned()));
    let seen_by_provider = Arc::clone(&rotating);
    let provider = InternalTokenProvider::new(move || {
        CredentialState::Available(
            seen_by_provider
                .lock()
                .expect("not poisoned")
                .clone()
                .into(),
        )
    });

    let client = fixture.gear.client_with(Some(&provider));
    let cache = client.cache_backend(PROFILE).expect("a handle");

    cache.get("ledger").await.expect("first call");
    *rotating.lock().expect("not poisoned") = "token-2".to_owned();
    cache.get("ledger").await.expect("second call");

    assert_eq!(
        fixture.recorder.tokens(),
        vec!["token-1".to_owned(), "token-2".to_owned()],
        "the second call must carry the rotated token, on the same channel"
    );

    fixture.stop().await;
}

/// No provider attaches nothing. Against a server that *enforces* the platform
/// plane the call is then rejected by the layer — which is the correct, loud
/// outcome, and the one that distinguishes "attached nothing" from "attached
/// something empty".
#[tokio::test]
async fn no_provider_attaches_nothing_and_an_enforcing_server_rejects() {
    let fixture = Fixture::start().await;

    let cache = fixture
        .gear
        .client()
        .cache_backend(PROFILE)
        .expect("a handle");

    let err = cache
        .get("ledger")
        .await
        .expect_err("an enforcing server must refuse an uncredentialed call");

    assert!(
        fixture.recorder.tokens().is_empty(),
        "the authenticator must never have been reached; got: {:?}",
        fixture.recorder.tokens()
    );
    let rendered = err.to_string();
    assert!(
        rendered.to_lowercase().contains("unauthenticated")
            || rendered.to_lowercase().contains("credential"),
        "expected an authentication failure, got: {rendered}"
    );

    fixture.stop().await;
}

// The inbound half: the stamped identity reaches the handler's extensions

/// One handler observation: the `PlatformSecurityContext` peer name and the
/// `PeerAuthenticated` name it read from the extensions (each `None` if absent).
type ObservedIdentity = (Option<String>, Option<String>);

/// A profile service whose handler records the identity it observes in the
/// request extensions, so a test can assert the platform-plane layer stamped it.
#[derive(Clone, Default)]
struct IdentityRecordingProfile {
    seen: Arc<Mutex<Vec<ObservedIdentity>>>,
}

#[tonic::async_trait]
impl pstub::cluster_profile_api_server::ClusterProfileApi for IdentityRecordingProfile {
    async fn describe_profiles(
        &self,
        request: Request<pstub::DescribeProfilesRequest>,
    ) -> Result<Response<pstub::DescribeProfilesResponse>, Status> {
        let ctx_name = request
            .extensions()
            .get::<PlatformSecurityContext>()
            .map(|c| c.identity().peer_name().to_owned());
        let peer_name = request
            .extensions()
            .get::<PeerAuthenticated>()
            .map(|p| p.name.clone());
        self.seen
            .lock()
            .expect("not poisoned")
            .push((ctx_name, peer_name));
        Ok(Response::new(pstub::DescribeProfilesResponse::default()))
    }
}

/// Maps token `t` to identity name `caller-<t>`, so a test can tell whose
/// credential the handler saw.
struct TokenNames;

impl InternalAuthenticator for TokenNames {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        Ok(PlatformIdentity::Shared {
            name: format!("caller-{token}"),
        })
    }
}

/// Drives one `DescribeProfiles` RPC carrying `token` at the raw generated client.
async fn describe_with_token(endpoint: &str, token: &str) {
    let channel = Channel::from_shared(endpoint.to_owned())
        .expect("a valid endpoint")
        .connect()
        .await
        .expect("connects");
    let mut client = pstub::cluster_profile_api_client::ClusterProfileApiClient::new(channel);
    let mut request = Request::new(pstub::DescribeProfilesRequest::default());
    request.metadata_mut().insert(
        INTERNAL_TOKEN_HEADER,
        token.parse().expect("an ASCII token"),
    );
    client
        .describe_profiles(request)
        .await
        .expect("the enforcing layer authenticates the token");
}

#[tokio::test]
async fn the_layer_stamps_the_identity_the_handler_reads_from_the_extensions() {
    // The inbound half over a real socket: the enforcing layer validates the
    // token, stamps `PlatformSecurityContext` / `PeerAuthenticated`, and the
    // cluster handler reads them back — with distinct tokens yielding distinct
    // identities, which is the property `CallerResolver` and `owns()` depend on.
    let recorder = IdentityRecordingProfile::default();
    let layer = InternalAuthGrpcLayer::new(DynInternalAuthenticator::new(TokenNames));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let addr = listener.local_addr().expect("has an address");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let handler = recorder.clone();
    let server = tokio::spawn(async move {
        Server::builder()
            .layer(layer)
            .add_service(pstub::cluster_profile_api_server::ClusterProfileApiServer::new(handler))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _stopped = shutdown_rx.await;
            })
            .await
            .expect("the server runs");
    });

    let endpoint = format!("http://{addr}");
    describe_with_token(&endpoint, "alpha").await;
    describe_with_token(&endpoint, "beta").await;

    let seen = recorder.seen.lock().expect("not poisoned").clone();
    assert_eq!(seen.len(), 2, "both RPCs reached the handler");
    assert_eq!(
        seen[0],
        (
            Some("caller-alpha".to_owned()),
            Some("caller-alpha".to_owned())
        ),
        "the handler must observe the alpha identity the layer stamped"
    );
    assert_eq!(
        seen[1],
        (
            Some("caller-beta".to_owned()),
            Some("caller-beta".to_owned())
        ),
        "a distinct token must reach the handler as a distinct identity"
    );

    let _stopped = shutdown.send(());
    let _joined = server.await;
}
