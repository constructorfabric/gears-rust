//! Public API surface for the usage-collector module (REST).
//!
//! PEP authorization is enforced inside the domain layer (the shared
//! [`crate::domain::authz::authorize`] helper, called with the per-resource
//! constants from [`crate::domain::authz::usage_type`]), so REST handlers
//! pass `&SecurityContext` through but never construct an `AccessRequest`.

pub mod rest;
