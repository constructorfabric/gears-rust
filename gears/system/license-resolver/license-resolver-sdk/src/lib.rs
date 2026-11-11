//! License Resolver SDK
//!
//! This crate provides the public, transport-agnostic contract for the
//! `license_resolver` gear — a read-only, plugin-delegating resolver that
//! answers exactly one question: *is this resource licensed to this subject
//! right now?*
//!
//! - [`LicenseResolverApi`] — public API contract for consumers (the single
//!   check, in two forms), in process or over REST ([`rest`]).
//! - [`LicenseResolverPluginClient`] — plugin API trait for backend
//!   implementations (mirrors the public signature).
//! - [`IsLicensedRequest`], [`SelfLicenseCheckRequest`], [`LicenseDecision`] —
//!   the check inputs/output; [`LicenseCheckRequest`] — the plugin's input.
//! - [`Subject`], [`PrincipalSubject`], [`Resource`], [`LicenseCheckContext`] —
//!   the objects bundled into a request.
//! - [`LicenseResolverError`] — the typed error enum of the plugin API, its
//!   mapping to `CanonicalError`, and the violation vocabulary ([`field`]).
//! - [`LicenseResolverPluginSpecV1`] — GTS plugin spec used for discovery.
//! - [`gts`] — the licensing base types (`gts.cf.core.lic.subj.v1~` /
//!   `…res.v1~`) that consuming Gears derive their contract types from, and the
//!   well-known tenant Subject type ([`LicenseTenantSubjectV1`]).
//!
//! There is intentionally **no** listing or enumeration method: enumerating a
//! platform's licensing surface is served natively by the types registry
//! (every licensing contract derives from the base types), not by this API.
//!
//! ## Usage
//!
//! ```ignore
//! use license_resolver_sdk::LicenseResolverApi;
//! use toolkit_security::PlatformSecurityContext;
//!
//! let resolver = hub.get::<dyn LicenseResolverApi>()?;
//! let decision = resolver
//!     .is_licensed(PlatformSecurityContext::outbound_marker(), request)
//!     .await?;
//! if decision.granted { /* allow */ } else { /* deny */ }
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod error;
pub mod field;
pub mod gts;
pub mod models;
pub mod plugin_api;
pub mod rest;

pub use api::{LicenseResolverApi, license_resolver_api_ir};
pub use error::{FieldViolation, LicenseResolverError};
pub use gts::{
    LicenseResolverPluginSpecV1, LicenseResourceV1, LicenseSubjectV1, LicenseTenantSubjectV1,
};
pub use models::{
    IsLicensedRequest, LicenseCheckContext, LicenseCheckContextBuildError,
    LicenseCheckContextBuilder, LicenseCheckRequest, LicenseDecision, PrincipalSubject, Resource,
    SelfLicenseCheckRequest, Subject, SubjectInput,
};
pub use plugin_api::LicenseResolverPluginClient;
pub use rest::{LicenseResolverApiRest, license_resolver_api_rest_http_binding};
#[cfg(feature = "rest-client")]
pub use rest::{LicenseResolverApiRestClient, LicenseResolverApiRestResolvingClient};
