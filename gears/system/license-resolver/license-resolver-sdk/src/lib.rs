//! License Resolver SDK
//!
//! This crate provides the public, transport-agnostic contract for the
//! `license_resolver` gear — a read-only, plugin-delegating resolver that
//! answers exactly one question: *is this resource licensed to this subject
//! right now?*
//!
//! - [`PlatformLicenseResolverApi`] — the check for a subject and tenant the
//!   caller names (platform plane); [`LicenseResolverApi`] — the check for the
//!   caller itself (tenant plane). In process or over REST ([`rest`]).
//! - [`LicenseResolverPluginClient`] — plugin API trait for backend
//!   implementations (mirrors the public signature).
//! - [`PlatformIsLicensedRequest`], [`IsLicensedRequest`], [`LicenseDecision`] —
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
//! use license_resolver_sdk::PlatformLicenseResolverApi;
//! use toolkit_security::PlatformSecurityContext;
//!
//! let resolver = hub.get::<dyn PlatformLicenseResolverApi>()?;
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

toolkit_gts::declare_gts_inventory!();
pub mod models;
pub mod plugin_api;
pub mod rest;

pub use api::{
    LicenseResolverApi, PlatformLicenseResolverApi, license_resolver_api_ir,
    platform_license_resolver_api_ir,
};
pub use error::{FieldViolation, LicenseResolverError};
pub use gts::{
    LicenseResolverPluginSpecV1, LicenseResourceV1, LicenseSubjectV1, LicenseTenantSubjectV1,
};
pub use models::{
    IsLicensedRequest, LicenseCheckContext, LicenseCheckRequest, LicenseDecision,
    PlatformIsLicensedRequest, PrincipalSubject, Resource, Subject, SubjectInput,
};
pub use plugin_api::LicenseResolverPluginClient;
pub use rest::{
    LicenseResolverApiRest, PlatformLicenseResolverApiRest, license_resolver_api_rest_http_binding,
    platform_license_resolver_api_rest_http_binding,
};
#[cfg(feature = "rest-client")]
pub use rest::{
    LicenseResolverApiRestClient, LicenseResolverApiRestResolvingClient,
    PlatformLicenseResolverApiRestClient, PlatformLicenseResolverApiRestResolvingClient,
};
