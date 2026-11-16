//! api-contracts demo gear — `PaymentApi` implementation.

pub mod api;
pub mod config;
pub mod domain;
pub mod gear;
#[cfg(feature = "grpc-server")]
pub mod grpc_gear;

pub use gear::ApiContracts;
#[cfg(feature = "grpc-server")]
pub use grpc_gear::ApiContractsGrpc;

// `ApiContractsGrpc` declares the `grpc` capability, and a host with gRPC
// services but no hub refuses to start (`GrpcRequiresHub`). Linking the hub
// here makes every binary that links that gear link the hub as well.
#[cfg(feature = "grpc-server")]
use grpc_hub as _;
