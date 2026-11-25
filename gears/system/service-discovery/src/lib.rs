//! Service Discovery
//!
//! System gear for service discovery.
//! This gear provides `DirectoryService` for gRPC service registration and discovery.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::ServiceDiscoveryGear;

// === INTERNAL MODULES (pub for integration tests) ===
pub mod api;
#[cfg(any(
    test,
    feature = "storage-k8s",
    feature = "storage-postgres",
    feature = "storage-redis"
))]
/// Shared-cache-backed directory and storage plugin management.
mod cache_directory_backend;
#[cfg(not(any(
    test,
    feature = "storage-k8s",
    feature = "storage-postgres",
    feature = "storage-redis"
)))]
#[path = "cache_directory_backend_stub.rs"]
/// Storage selection when no shared backend is compiled in.
mod cache_directory_backend;
mod config;
mod directory_backend;
mod directory_client;
pub mod domain;
mod local_directory_client;
mod replica;
mod server;

// === RE-EXPORTS ===
pub use cf_system_sdks::directory::{
    DirectoryGrpcClient, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};
