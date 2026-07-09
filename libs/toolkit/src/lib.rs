#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! # `ToolKit` - Declarative Gear System
//!
//! A unified crate for building modular applications with declarative gear definitions.
//!
//! ## Features
//!
//! - **Declarative**: Use `#[gear(...)]` attribute to declare gears
//! - **Auto-discovery**: Gears are automatically discovered via inventory
//! - **Type-safe**: Compile-time validation of capabilities
//! - **Phase-based lifecycle**: executed by `HostRuntime` (see `runtime/host_runtime.rs` docs)
//!
//! ## Golden Path: Stateless Handlers
//!
//! For optimal performance and readability, prefer stateless handlers that receive
//! `Extension<T>` and other extractors rather than closures that capture environment.
//!
//! ### Recommended Pattern
//!
//! ```rust,ignore
//! use axum::{Extension, Json};
//! use toolkit::api::{OperationBuilder, Problem};
//! use std::sync::Arc;
//!
//! async fn list_users(
//!     Extension(svc): Extension<Arc<UserService>>,
//! ) -> Result<Json<Vec<UserDto>>, Problem> {
//!     let users = svc.list_users().await.map_err(Problem::from)?;
//!     Ok(Json(users))
//! }
//!
//! pub fn router(service: Arc<UserService>) -> axum::Router {
//!     let op = OperationBuilder::get("/users-info/v1/users")
//!         .summary("List users")
//!         .handler(list_users)
//!         .json_response(200, "List of users")
//!         .standard_errors(&registry);
//!
//!     axum::Router::new()
//!         .route("/users-info/v1/users", axum::routing::get(list_users))
//!         .layer(Extension(service))
//!         .layer(op.to_layer())
//! }
//! ```
//!
//! ### Benefits
//!
//! - **Performance**: No closure captures or cloning on each request
//! - **Readability**: Clear function signatures show exactly what data is needed
//! - **Testability**: Easy to unit test handlers with mock state
//! - **Type Safety**: Compile-time verification of dependencies
//! - **Flexibility**: Individual service injection without coupling
//!
//! ## Basic Gear Example
//!
//! ```rust,ignore
//! use toolkit::{gear, Gear, DbGear, RestfulGear, StatefulGear};
//!
//! #[derive(Default)]
//! #[gear(name = "user", deps = ["database"], capabilities = [db, rest, stateful])]
//! pub struct UserGear;
//!
//! // Implement the declared capabilities...
//! ```

// When running tests, make ::toolkit resolve to this crate so macros work
#[cfg(test)]
extern crate self as toolkit;

pub use anyhow::Result;
pub use async_trait::async_trait;

// Re-export tokio so `#[gear(...)]`-generated code can reference
// `::toolkit::tokio::...` without requiring gears to add a direct tokio dep.
pub use tokio;

// Re-export inventory for user convenience
pub use inventory;

// Gear system exports
pub use crate::contracts::*;
pub use crate::contracts::{GrpcServiceCapability, RegisterGrpcServiceFn};

// Configuration gear
pub mod config;
pub use config::{ConfigError, ConfigProvider, gear_config_or_default, gear_config_required};

// Context gear
pub mod context;
pub use context::{GearContextBuilder, GearCtx};

// Gear system implementations for macro code
pub mod client_hub;
pub mod registry;

// Re-export main types
pub use client_hub::ClientHub;
pub use registry::GearRegistry;

// Re-export the macros from the proc-macro crate
pub use toolkit_macros::{ExpandVars, gear, lifecycle};

// Re-export var_expand gear so derive-generated impls resolve via ::toolkit::var_expand
pub use toolkit_utils::var_expand;

// Core gear contracts and traits
pub mod contracts;
// Type-safe API operation builder
pub mod api;
pub use api::{
    IntoCanonical, OpenApiInfo, OpenApiRegistry, OpenApiRegistryImpl, OperationBuilder,
    error_mapping_middleware,
};
pub use toolkit_odata::{Page, PageInfo};

// HTTP utilities
pub mod http;
pub use http::sse::SseBroadcaster;

// Telemetry utilities
pub mod telemetry;

pub mod backends;
pub mod lifecycle;
pub mod plugins;
pub mod runtime;

// Domain layer marker traits for DDD enforcement
pub mod domain;
pub use domain::{DomainErrorMarker, DomainModel};

// Directory API for service discovery
pub mod directory;
pub use directory::{
    DirectoryClient, LocalDirectoryClient, RegisterInstanceInfo, ServiceEndpoint,
    ServiceInstanceInfo,
};

// GTS schema support
pub mod gts;

// Security context scoping wrapper (re-exported from toolkit-sdk)
pub use toolkit_sdk::{Secured, WithSecurityContext};

pub use backends::{
    BackendKind, GearRuntimeBackend, InstanceHandle, LocalProcessBackend, OopBackend,
    OopGearConfig, OopSpawnConfig,
};
pub use lifecycle::{Lifecycle, Runnable, Status, StopReason, WithLifecycle};
pub use plugins::GtsPluginSelector;
pub use runtime::{
    DEFAULT_SHUTDOWN_DEADLINE, DbOptions, Endpoint, GearInstance, GearManager, OopGearSpawnConfig,
    OopSpawnOptions, RunOptions, ShutdownOptions, run,
};

#[cfg(feature = "bootstrap")]
pub mod bootstrap;
