pub mod binding;
pub mod contract;
pub mod grpc;
pub mod validation;

pub use binding::{HttpBindingIr, HttpFieldBinding, HttpMethod, HttpMethodBindingIr};
pub use contract::{
    ContractIr, FieldIr, FieldRole, Idempotency, InputShape, MethodIr, MethodKind, PrimitiveType,
    ServiceIr, TypeRef,
};
pub use grpc::{GrpcBindingIr, GrpcIdempotency, GrpcMethodBindingIr, validate_grpc_binding};
pub use validation::{ValidationError, validate_contract, validate_http_binding};
