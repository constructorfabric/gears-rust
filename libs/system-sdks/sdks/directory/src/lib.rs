//! Gear Orchestrator Contracts
//!
//! Domain contracts and client interfaces for gear orchestration.
//! This crate provides the `DirectoryClient` trait and related types that
//! define the contract for service discovery and instance management.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod api;
#[cfg(feature = "grpc")]
mod grpc;

pub use api::{
    DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound, GrpcServiceInfo,
    RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};
#[cfg(feature = "grpc")]
pub use grpc::*;
