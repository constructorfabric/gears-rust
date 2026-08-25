//! Compile + wiring coverage for platform-plane (`PlatformSecurityContext`)
//! gRPC client codegen (issue #4577).
//!
//! There is no macro-based gRPC SDK with a security-context parameter that
//! compiles in CI without `protoc` (the one example needs a newer protoc), so
//! this test hand-writes a minimal tonic-stub module and drives the
//! `#[toolkit::grpc_contract]` codegen against it. It proves the generated
//! client:
//!   - accepts a `PlatformSecurityContext` first argument,
//!   - excludes it from the wire payload (the synthesized `From<String>` request
//!     bridge is emitted for the `body` param, not the context),
//!   - and builds (`new` / `connect`) with an [`InternalTokenProvider`] on the
//!     config — the runtime source of the `X-ToolKit-Internal-Token` credential.
//!
//! The RPC itself is not invoked (no server); the value here is that the
//! platform-plane generated method body type-checks and links.

#![cfg(feature = "grpc-client")]
// The hand-written tonic-stub client mimics tonic-prost-build output shapes
// (an `async fn` RPC returning `Result<_, Status>`); the stub body neither
// awaits nor documents errors, which is fine for a compile-only stand-in.
#![allow(
    clippy::unwrap_used,
    clippy::unused_async,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use tonic::metadata::MetadataMap;
use toolkit_contract::grpc_repr::{GrpcRepr, TryFromProto, ViaStringParseError};
use toolkit_contract::runtime::config::{ClientConfig, InternalTokenProvider};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::{contract, grpc_contract};
use toolkit_security::PlatformSecurityContext;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

/// The stub RPC records the metadata of the request it received here, so the
/// test can assert the generated client attached the platform credential under
/// the correct key. A process-wide `static` is used because the generated
/// client constructs the stub itself via `RegClient::new(channel)` — there is
/// no seam to inject a per-test capture handle.
static CAPTURED_METADATA: Mutex<Option<MetadataMap>> = Mutex::new(None);

#[derive(Debug, thiserror::Error)]
pub enum RegError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

/// Response DTO. `GrpcRepr` satisfies the macro's return-type repr guard;
/// `TryFromProto` is how the generated client decodes the wire response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegResult {
    pub accepted: bool,
}

impl GrpcRepr for RegResult {}

impl TryFromProto<stubs::RegisterResponse> for RegResult {
    fn try_from_proto_wire(proto: stubs::RegisterResponse) -> Result<Self, ViaStringParseError> {
        Ok(Self {
            accepted: proto.accepted,
        })
    }
}

/// Hand-written stand-in for the tonic-generated stubs the macro references via
/// `stubs_module = "crate::stubs"`. Shaped to match tonic-prost-build output:
/// `<service_snake>_client::<Service>Client<Channel>` with `new` + the RPC
/// method, plus `<Method>Request` / response messages.
pub mod stubs {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct RegisterRequest {
        // Field name must match the contract's wire param ident (`body`) so the
        // macro's synthesized `From<String>` bridge (`Self { body: value }`)
        // compiles.
        pub body: String,
    }

    #[derive(Clone, Debug, Default)]
    pub struct RegisterResponse {
        pub accepted: bool,
    }

    pub mod reg_client {
        use tonic::transport::Channel;

        #[derive(Clone)]
        pub struct RegClient<C> {
            _channel: C,
        }

        impl RegClient<Channel> {
            pub fn new(channel: Channel) -> Self {
                Self { _channel: channel }
            }

            pub async fn register(
                &mut self,
                request: tonic::Request<super::RegisterRequest>,
            ) -> Result<tonic::Response<super::RegisterResponse>, tonic::Status> {
                // Capture the metadata the generated client attached, then fail:
                // the test asserts on the captured metadata, not the response.
                *super::super::CAPTURED_METADATA.lock().unwrap() = Some(request.metadata().clone());
                Err(tonic::Status::unimplemented("test stub"))
            }
        }
    }
}

#[contract(gear = "reg", version = "v1")]
pub trait RegApi: Send + Sync {
    async fn register(
        &self,
        ctx: PlatformSecurityContext,
        body: String,
    ) -> Result<RegResult, RegError>;
}

#[grpc_contract(package = "reg.v1", service = "Reg", stubs_module = "crate::stubs")]
pub trait RegApiGrpc: RegApi {
    #[rpc(name = "Register")]
    async fn register(
        &self,
        ctx: PlatformSecurityContext,
        body: String,
    ) -> Result<RegResult, RegError>;
}

/// The generated platform-plane gRPC client builds with an internal-token
/// provider on its config and is usable as `Arc<dyn RegApi>`. Construction uses
/// a lazy channel (no I/O); the RPC is not invoked.
#[tokio::test]
async fn platform_grpc_client_builds_with_internal_token_provider() {
    let channel = tonic::transport::Channel::from_static("http://127.0.0.1:1").connect_lazy();
    let config = ClientConfig::new("http://127.0.0.1:1").with_internal_token_provider(
        InternalTokenProvider::from_token(SecretString::from("sa.jwt.token")),
    );

    let client = RegApiGrpcClient::new(channel, config);
    // Both the base trait (real bodies) and the projection trait (delegating
    // defaults) must be object-safe views of the generated client.
    let _as_base: Arc<dyn RegApi> = Arc::new(client);
}

/// The generated platform-plane gRPC method attaches the runtime credential as
/// `x-toolkit-internal-token` metadata — and NOT as `authorization`. Drives the
/// method against the capturing stub and inspects the metadata it received.
#[tokio::test]
async fn platform_grpc_client_attaches_internal_token_metadata() {
    *CAPTURED_METADATA.lock().unwrap() = None;

    let channel = tonic::transport::Channel::from_static("http://127.0.0.1:1").connect_lazy();
    let config = ClientConfig::new("http://127.0.0.1:1").with_internal_token_provider(
        InternalTokenProvider::from_token(SecretString::from("sa.jwt.token")),
    );
    let client = RegApiGrpcClient::new(channel, config);

    // The stub returns `unimplemented`, so the call errors — but only AFTER it
    // has captured the metadata the client attached, which is what we assert on.
    let _result = RegApi::register(
        &client,
        PlatformSecurityContext::new(toolkit_security::PlatformIdentity::Shared {
            name: "test-caller".to_owned(),
        }),
        "payload".to_owned(),
    )
    .await;

    let captured = CAPTURED_METADATA.lock().unwrap();
    let meta = captured.as_ref().expect("stub captured request metadata");
    assert_eq!(
        meta.get(INTERNAL_TOKEN_HEADER).unwrap().to_str().unwrap(),
        "sa.jwt.token",
        "platform-plane gRPC call must carry x-toolkit-internal-token"
    );
    assert!(
        meta.get("authorization").is_none(),
        "platform-plane credential must never ride authorization metadata"
    );
}
