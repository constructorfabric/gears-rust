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
    ContractError, ProtoBridge, contract, grpc_contract, provides, rest_contract,
};
pub use policy::{Policy, PolicyContext, PolicyStack, TracingPolicy};
pub use wiring::{ClientTuning, ClientWiring, ReconnectSettings, RetrySettings};

// Wire envelope: re-export `Problem` from the canonical-errors leaf so all
// downstream crates have a single import path to the RFC 9457 envelope.
#[cfg(feature = "canonical-errors")]
pub use toolkit_canonical_errors::{Problem, ProblemCategory};
