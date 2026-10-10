//! Wire `type` / `subject` vocabulary for precondition violations under
//! [`CanonicalError::FailedPrecondition`].
//!
//! Each violation lands in
//! `CanonicalError::FailedPrecondition.ctx.violations[]` and reaches SDK
//! consumers as [`ServiceGatewayError::FailedPrecondition`]: its `type` as
//! `precondition_type`, its `subject` as `subject`. Dispatch on
//! `precondition_type`; the `subject` names what failed the precondition.
//!
//! [`CanonicalError::FailedPrecondition`]: toolkit_canonical_errors::CanonicalError::FailedPrecondition
//! [`ServiceGatewayError::FailedPrecondition`]: crate::ServiceGatewayError::FailedPrecondition

// ---------------------------------------------------------------------------
// `violations[].type`
// ---------------------------------------------------------------------------

/// A state precondition that may clear without changing the request. Two
/// sources:
///
/// * a guard plugin rejected the call (status 404) without naming a
///   resource; the `subject` is the plugin's `error_code`;
/// * an upstream's credential reference is not provisioned yet or not
///   shared with the tenant; the `subject` is `auth.config.secret_ref`.
pub const STATE: &str = "STATE";

/// The upstream or route is managed by the types registry, so the
/// Management API and the SDK cannot update or delete it. The `subject` is
/// [`MANAGED_BY`]. Retrying cannot help: the row changes only with its
/// types-registry instance, at the next OAGW boot. The canonical error's
/// `resource_type` tells an upstream ([`crate::gts::UPSTREAM_SCHEMA`]) from a
/// route ([`crate::gts::ROUTE_SCHEMA`]).
pub const REGISTRY_MANAGED: &str = "REGISTRY_MANAGED";

// ---------------------------------------------------------------------------
// `violations[].subject`
// ---------------------------------------------------------------------------

/// The `subject` of a [`REGISTRY_MANAGED`] violation: the row's owner.
pub const MANAGED_BY: &str = "managed_by";
