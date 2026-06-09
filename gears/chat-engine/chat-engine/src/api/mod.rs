//! API surface for `cf-chat-engine`.
//!
//! Phase 14 assembles the REST surface: DTO layer
//! ([`rest::dto`]), RFC-9457 error mapping ([`rest::error`]), NDJSON
//! streaming helper + [`rest::WebhookEmitter`] trait ([`rest::mod`]),
//! per-feature handlers ([`rest::handlers`]) and the [`OperationBuilder`]
//! route registration ([`rest::routes`]).
//!
//! `register_routes` is re-exported at the crate `api` boundary so
//! `module.rs` (Phase 15) can mount everything in a single call.
//
// @cpt-cf-chat-engine-api-root:p14

pub mod rest;

pub use rest::register_routes;
