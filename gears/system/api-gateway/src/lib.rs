#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! API Gateway Gear
//!
//! Main API Gateway gear — owns the HTTP server (`rest_host`) and collects
//! typed operation specs to emit a single `OpenAPI` document.

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::ApiGateway;

// === INTERNAL MODULES ===
/// Embedded static assets (docs UI bundles).
mod assets;
/// Gateway configuration types.
mod config;
/// CORS layer construction from [`CorsConfig`].
mod cors;
/// HTTP middleware stack: auth, throttling, metrics, access logging and related layers.
pub mod middleware;
mod proxy;
mod router_cache;
/// Built-in web endpoints: health checks and the API docs page.
mod web;

// === RE-EXPORTS ===
pub use config::{ApiGatewayConfig, CorsConfig};
