//! service-principal — a thin REST facade over the `ServicePrincipalClientV1` SPI
//! for managing tenant-scoped machine identities (confidential OAuth
//! `client_credentials` clients).
//!
//! REST → PDP authorization (own RBAC resource type) → the SPI resolved lazily
//! from `ClientHub` → canonical error mapping. No storage, no business logic.

pub mod api;
pub mod domain;
pub mod module;

// Compiled for its link-time `AuthzPermissionV1` inventory registration.
pub(crate) mod gts;

pub use module::ServicePrincipalGear;
