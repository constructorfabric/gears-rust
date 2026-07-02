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
//! - [`policy`] — the [`RouteAuthPolicy`] abstraction that decides, per
//!   `(method, path)`, whether the tenant-plane middleware requires a JWT, plus
//!   the built-in [`MatchitRouteAuthPolicy`] most gears can use directly.
//! - [`common`] — shared, transport-agnostic helpers: RFC 6750
//!   `WWW-Authenticate` challenge rendering and nested-path resolution.
//! - [`security`] — the supporting extractors that pull the tenant-plane bearer
//!   token and the platform-plane internal token out of inbound request headers.
//! - [`request_id`] — `X-Request-Id` generation/propagation into extensions.
//! - [`access_log`] — structured per-request access log with byte counting.
//! - [`http_metrics`] — OpenTelemetry HTTP server metrics. A no-op meter is used
//!   until the consumer installs a meter provider, so it is near-zero cost.
//! - [`mime_validation`] — per-route `Content-Type` allow-listing.
//! - [`license_validation`] — per-route feature-entitlement gating.
//! - [`rate_limit`] — per-route token-bucket + in-flight limiting.
//! - [`scope_enforcement`] — coarse-grained, pre-PDP token-scope checks.
//!
//! The last four operate on lookup tables/rules the consuming gear builds from
//! its own operation specs and config, and render rejections under a
//! caller-supplied GTS scope (via [`toolkit_canonical_errors::CanonicalError`]'s
//! `scoped_*` constructors) so this crate stays free of any gear-specific error
//! identity, config, or spec type.
//!
//! Keeping these out of `toolkit-http` (the outbound HTTP client) keeps the
//! client crate free of `axum` and the canonical-error stack, and gives every
//! gear a single place to depend on for the server-side auth planes.

pub mod access_log;
pub mod auth;
pub mod common;
pub mod http_metrics;
pub mod license_validation;
pub mod mime_validation;
pub mod policy;
pub mod rate_limit;
pub mod request_id;
pub mod scope_enforcement;
pub mod security;

pub use auth::{SecurityContextLayerState, internal_auth_middleware, security_context_middleware};
pub use common::{BearerChallenge, append_bearer_challenge, resolve_path};
pub use policy::{AuthRequirement, MatchitRouteAuthPolicy, RouteAuthPolicy};
pub use security::{
    InternalTokenHttpError, SecurityContextHttpError, extract_bearer_http,
    extract_internal_token_http,
};
