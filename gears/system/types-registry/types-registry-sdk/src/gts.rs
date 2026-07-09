//! GTS resource-type vocabulary for the types-registry canonical surface.
//!
//! [`TYPE_RESOURCE_TYPE`] is the canonical GTS resource type that tags every
//! types-registry `NotFound` / `AlreadyExists` / `InvalidArgument` /
//! `FailedPrecondition` error — it MUST equal the literal in the impl crate's
//! `#[resource_error("…")]` marker (`api::rest::error`) and the SDK-internal
//! [`TypeResource`] marker below. The round-trip tests in [`crate::error`] pin
//! the equality (the proc-macro cannot reference the const directly).
//!
//! [`TypeResource`] is the SDK-internal `#[resource_error]` marker used by the
//! client-side `try_new` constructors in [`crate::models`] to build the
//! in-process `InvalidArgument` errors they emit (those constructors never cross
//! a wire boundary — see ADR 0005 "Non-Canonical Methods" — but emitting them as
//! `CanonicalError` keeps the SDK on a single error type end-to-end).

use toolkit_canonical_errors::resource_error;

/// The canonical GTS resource type for types-registry entities. Lands in
/// `CanonicalError` `resource_type` / the wire `context.resource_type`.
pub const TYPE_RESOURCE_TYPE: &str = "gts.cf.types_registry.registry.type.v1~";

/// SDK-internal canonical-error scope. Mirrors the impl crate's
/// `#[resource_error]` marker so the SDK's `try_new` constructors emit the same
/// `resource_type` the REST ladder does. Its literal MUST equal
/// [`TYPE_RESOURCE_TYPE`] — pinned by `gts_resource_type_round_trips`.
#[resource_error("gts.cf.types_registry.registry.type.v1~")]
pub(crate) struct TypeResource;
