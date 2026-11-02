#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! gRPC Hub Gear
//!
//! This gear builds and hosts the single `tonic::Server` instance for the process.

// === MODULE DEFINITION ===
pub mod dynamic_routes;
pub mod gear;
pub use gear::{GrpcHub, GrpcHubConfig};
