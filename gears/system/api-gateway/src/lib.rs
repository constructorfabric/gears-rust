#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! API Gateway Gear
//!
//! Main API Gateway gear — owns the HTTP server (`rest_host`) and collects
//! typed operation specs to emit a single `OpenAPI` document.

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::ApiGateway;

// === INTERNAL MODULES ===
mod assets;
mod config;
mod cors;
pub mod middleware;
mod proxy;
mod router_cache;
mod web;

// === RE-EXPORTS ===
pub use config::{ApiGatewayConfig, CorsConfig};
