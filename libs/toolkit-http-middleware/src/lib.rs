#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![warn(warnings)]

//! Server-side HTTP middleware for `ToolKit`.
//!
//! This crate provides the inbound request-handling layers a gear's HTTP server
//! installs at its edge. It does **not** run an HTTP server itself — the server
//! is owned by the `OoP` bootstrap (`toolkit::bootstrap::oop`) and by the
//! `api-gateway` gear's `rest_host`, which install these layers onto their
//! router.
//!
//! - [`auth`] — Axum middleware for two-plane authentication. It turns
//!   inbound credentials into a [`toolkit_security::SecurityContext`] (tenant
//!   plane) or a [`toolkit_security::PlatformSecurityContext`] (platform plane),
//!   rejecting invalid credentials with canonical RFC 9457 `problem+json`
//!   responses.
//! - [`security`] — the supporting extractors that pull the tenant-plane bearer
//!   token and the platform-plane internal token out of inbound request headers.
//!
//! Keeping these out of `toolkit-http` (the outbound HTTP client) keeps the
//! client crate free of `axum` and the canonical-error stack, and gives every
//! gear a single place to depend on for the server-side auth planes.

pub mod auth;
pub mod security;

pub use auth::{PublicRoute, internal_auth_middleware, security_context_middleware};
pub use security::{
    InternalTokenHttpError, SecurityContextHttpError, extract_bearer_http,
    extract_internal_token_http,
};
