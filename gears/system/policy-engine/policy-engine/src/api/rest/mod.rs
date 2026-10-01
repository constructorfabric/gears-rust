//! Policy Administration REST API.
//!
//! Every route is a thin projection over [`PolicyManagementClientV1`]: no
//! domain logic lives here, and there is no decision or evaluate endpoint.
//! [`routes::register_routes`] is the single entry point that wires every
//! operation onto the platform's `OperationBuilder`.
//!
//! - [`dto`] — wire DTOs, prefixed `PolicyEngine…Dto`, mapped from and to the
//!   SDK's management models. Content (a document's source) appears only in
//!   the version-detail and replace-content DTOs.
//! - [`error`] — canonical errors for conditions the transport itself
//!   detects, before a request ever reaches [`PolicyManagementClientV1`].
//! - [`handlers`] — one async function per operation.
//! - [`routes`] — route registration.
//!
//! [`PolicyManagementClientV1`]: policy_engine_sdk::management::PolicyManagementClientV1

pub mod dto;
pub mod error;
pub mod handlers;
pub mod routes;

pub use routes::register_routes;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "routes_tests.rs"]
mod routes_tests;
