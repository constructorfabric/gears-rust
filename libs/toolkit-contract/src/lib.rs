#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod contract;
pub mod descriptor;
pub mod error;
pub mod grpc_repr;
pub mod http;
pub mod ir;
pub mod policy;
pub mod runtime;
pub mod wiring;

#[cfg(feature = "openapi")]
pub mod openapi;

#[cfg(feature = "grpc-client")]
pub mod grpc;

pub use contract::{Contract, ServiceContract};
pub use descriptor::{ContractDescriptor, ContractKind, MethodDescriptor, ServiceDescriptor};
pub use error::ContractError;
pub use grpc_repr::{
    GrpcRepr, GrpcReprScalar, SecurityContextMarker, UnknownEnumDiscriminant, ViaStringParseError,
    assert_security_context,
};
pub use ir::{
    ContractIr, FieldIr, FieldRole, GrpcBindingIr, GrpcIdempotency, GrpcMethodBindingIr,
    HttpBindingIr, HttpFieldBinding, HttpMethod, HttpMethodBindingIr, Idempotency, InputShape,
    MethodIr, MethodKind, PrimitiveType, ServiceIr, TypeRef, ValidationError, validate_contract,
    validate_grpc_binding, validate_http_binding,
};
pub use toolkit_contract_macros::{
    ContractError, ProtoBridge, consumes, contract, grpc_contract, provides, rest_contract,
};
pub use policy::{Policy, PolicyContext, PolicyStack, TracingPolicy};
pub use wiring::{ClientTuning, ClientWiring, ReconnectSettings, RetrySettings};

/// Re-export of `tracing` for macro-generated client code.
///
/// Generated REST/gRPC clients emit per-method spans through this path
/// (`#support::__tracing::…`) so SDK crates that only depend on
/// `toolkit-contract` do not need a direct `tracing` dependency. Not part of
/// the stable public API.
#[doc(hidden)]
pub use tracing as __tracing;

// Wire envelope: re-export `Problem` from the canonical-errors leaf so all
// downstream crates have a single import path to the RFC 9457 envelope.
#[cfg(feature = "canonical-errors")]
pub use toolkit_canonical_errors::{Problem, ProblemCategory};
