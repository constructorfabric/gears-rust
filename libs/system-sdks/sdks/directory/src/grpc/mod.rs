//! Gear Orchestrator gRPC Layer
//!
//! This crate provides gRPC transport for the gear orchestrator.
//! It includes generated protobuf types and client/server implementations.
mod client;

// Generated protobuf types for DirectoryService
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::empty_structs_with_brackets,
    warnings
)] // protoc problem
pub mod directory {
    tonic::include_proto!("gear_orchestrator.v1.directory");
}

// Re-export common types for DirectoryService
pub use directory::directory_service_client::DirectoryServiceClient;
pub use directory::directory_service_server::{DirectoryService, DirectoryServiceServer};
// The generated `InstanceState` enum is re-exported under an alias so it does
// not collide with the domain `api::InstanceState` at the crate root.
pub use directory::InstanceState as ProtoInstanceState;
pub use directory::{
    DeregisterInstanceRequest, GetOpenApiSpecRequest, GetOpenApiSpecResponse, GrpcServiceEndpoint,
    HeartbeatRequest, InstanceInfo, ListAllInstancesRequest, ListAllInstancesResponse,
    ListInstancesRequest, ListInstancesResponse, RegisterInstanceRequest,
    ResolveGrpcServiceRequest, ResolveGrpcServiceResponse, ResolveRestServiceRequest,
    ResolveRestServiceResponse,
};

// Re-export the gRPC client implementation
pub use client::DirectoryGrpcClient;

/// Service name constant for `DirectoryService`
pub const DIRECTORY_SERVICE_NAME: &str =
    <DirectoryServiceServer<()> as tonic::server::NamedService>::NAME;
