//! SDK for the api-contracts example module.
//!
//! Provides the [`PaymentApi`] trait contract, REST + gRPC projection traits,
//! domain models, and error types. The base trait is transport-agnostic;
//! projection traits carry HTTP / gRPC annotations consumed by their
//! respective macros.

pub mod contract;
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

pub use contract::{PaymentApi, PaymentStream, payment_api_ir};
pub use error::{PaymentError, PaymentResourceError};
pub use rest::{PaymentApiRest, payment_api_rest_http_binding};

#[cfg(feature = "rest-client")]
pub use rest::PaymentApiRestClient;

#[cfg(feature = "grpc-client")]
pub use grpc::{PaymentApiGrpc, PaymentApiGrpcClient, payment_api_grpc_binding};
