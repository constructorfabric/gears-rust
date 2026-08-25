#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![warn(warnings)]

//! `cf-gears-toolkit-gateway`: the gateway-provider abstraction for `OoP` edge
//! routing.
//!
//! This crate defines [`GatewayProvider`] — the trait the `OoP` bootstrap uses
//! to register a gear's public routes with an edge gateway — plus the typed
//! inputs it accepts ([`GearName`], [`OpenApiSpec`], [`Endpoint`]) and its error
//! type ([`GatewayError`]).
//!
//! It is a **leaf** crate: it does not depend on `cf-gears-toolkit`, so both
//! `cf-gears-toolkit` (the `OoP` bootstrap) and the `api-gateway` gear can depend
//! on it without introducing a dependency cycle.
//!
//! # Route visibility
//! Providers expose only the operations a gear marks **exposed** on the
//! visibility axis. `cf-gears-toolkit` emits this as the
//! [`API_VISIBILITY_EXTENSION`] `OpenAPI` vendor extension (value
//! [`API_VISIBILITY_EXPOSED`]) from `OperationSpec.exposed`; this crate mirrors
//! the well-known key as a constant so it needs no dependency on
//! `cf-gears-toolkit`.

mod error;
mod forward;
mod provider;
mod registry;
mod toolkit_provider;
mod types;

pub use error::GatewayError;
pub use forward::{Forwarder, proxy_fallback};
pub use provider::{GatewayProvider, InstanceSpec};
pub use registry::{InstanceRoutes, ProxyRegistry, RouteMatch, RouteTemplate};
pub use toolkit_provider::ToolKitGatewayProvider;
pub use types::{Endpoint, GearInstance, GearName, OpenApiSpec};

/// The `OpenAPI` vendor-extension key marking an operation as externally visible
/// (registered at the edge gateway).
///
/// Mirrors the key emitted by `cf-gears-toolkit` from `OperationSpec.exposed`.
pub const API_VISIBILITY_EXTENSION: &str = "x-toolkit-visibility";

/// The [`API_VISIBILITY_EXTENSION`] value indicating an exposed (externally visible)
/// route.
pub const API_VISIBILITY_EXPOSED: &str = "exposed";
