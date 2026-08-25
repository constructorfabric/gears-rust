//! SDK for the api-contracts example module.
//!
//! Provides the [`PaymentApi`] trait contract, REST + gRPC projection traits,
//! domain models, and error types. The base trait is transport-agnostic;
//! projection traits carry HTTP / gRPC annotations consumed by their
//! respective macros.

pub mod contract;
// v2 of the same contract, served in parallel with v1 (ADR-0007). Breaking
// changes go into a new major version; v1 above stays untouched.
//
// Note on deprecation: ADR-0007 §4 has vN's traits marked `#[deprecated]` with a
// sunset date once vN+1 ships. That is deliberately NOT done here — this crate is
// a *fixture* whose own server, consumer, and tests implement v1 on purpose to
// demonstrate the two versions running side by side, so deprecating v1 would only
// bury the example in `allow(deprecated)`. A real SDK shipping v2 should mark the
// v1 base and projection deprecated and record the sunset date.
pub mod contract_v2;
pub mod error;
// The gRPC projection trait + macro emit static `GrpcRepr` assertions on
// every DTO. Those impls land via `#[derive(ProtoBridge)]` which is gated
// on `grpc-client`. So the entire `grpc` module — trait, binding, client —
// is gated on the same feature; pulling the SDK with REST-only support
// must not require the gRPC dependency closure.
#[cfg(feature = "grpc-client")]
pub mod grpc;
pub mod models;
pub mod rest;
pub mod rest_v2;

pub use contract::{PaymentApi, PaymentStream, payment_api_ir};
pub use error::{PaymentError, PaymentResourceError};
pub use rest::{PaymentApiRest, payment_api_rest_http_binding};

#[cfg(feature = "rest-client")]
pub use rest::{PaymentApiRestClient, PaymentApiRestResolvingClient};

// v2 re-exports, mirroring the v1 set above. `#[toolkit::provides]` resolves
// `payment_api_v2_ir` / `PaymentApiV2RestClient` / `payment_api_v2_rest_http_binding`
// relative to the contract's parent module, so they must be visible here.
pub use contract_v2::{PaymentApiV2, payment_api_v2_ir};
pub use rest_v2::{PaymentApiV2Rest, payment_api_v2_rest_http_binding};

#[cfg(feature = "rest-client")]
pub use rest_v2::{PaymentApiV2RestClient, PaymentApiV2RestResolvingClient};

#[cfg(feature = "grpc-client")]
pub use grpc::{PaymentApiGrpc, PaymentApiGrpcClient, payment_api_grpc_binding};
