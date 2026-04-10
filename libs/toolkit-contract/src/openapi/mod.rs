//! `OpenAPI` 3.1 generation from Contract IR + `HttpBinding` IR.
//!
//! The generator produces a `serde_json::Value` representing the `OpenAPI` 3.1
//! document. Schemas for request/response/error types are passed in as a
//! caller-supplied list — typically produced by `schemars::schema_for!` for
//! each domain type.

#[cfg(feature = "openapi")]
pub mod generator;

#[cfg(feature = "openapi")]
pub use generator::{SchemaEntry, generate_openapi_spec};

#[cfg(feature = "openapi-axum")]
pub mod axum_route;

#[cfg(feature = "openapi-axum")]
pub use axum_route::well_known_openapi_route;
