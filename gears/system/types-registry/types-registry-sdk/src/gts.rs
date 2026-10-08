//! GTS resource-type vocabulary for the types-registry canonical surface.
//!
//! [`TYPE_RESOURCE_TYPE`] is the canonical GTS resource type that tags every
//! types-registry `NotFound` / `AlreadyExists` / `InvalidArgument` /
//! `FailedPrecondition` error about an entity — it MUST equal the literal in
//! the impl crate's `TypeRegistryError` `#[resource_error("…")]` marker
//! (`api::rest::error`) and the SDK-internal `TypeResource` marker below. The
//! round-trip tests in [`crate::error`] pin the equality (the proc-macro cannot
//! reference the const directly).
//!
//! [`OPERATION_RESOURCE_TYPE`] tags the errors that name an admission operation
//! instead: an unknown operation (`404`) and an `Idempotency-Key` already bound
//! to another request (`409`). Their `resource_name` is the operation UUID. It
//! MUST equal the impl crate's `OperationError` marker, which that crate's
//! error tests pin.
//!
//! `TypeResource` is the SDK-internal `#[resource_error]` marker used by the
//! client-side `try_new` constructors in [`crate::models`] to build the
//! in-process `InvalidArgument` errors they emit (those constructors never cross
//! a wire boundary — see ADR 0005 "Non-Canonical Methods" — but emitting them as
//! `CanonicalError` keeps the SDK on a single error type end-to-end).

use toolkit_canonical_errors::resource_error;
use toolkit_gts::gts_id;

/// The canonical GTS resource type for types-registry entities. Lands in
/// `CanonicalError` `resource_type` / the wire `context.resource_type`.
pub const TYPE_RESOURCE_TYPE: &str = gts_id!("cf.core.types_registry.entity.v1~");

/// The canonical GTS resource type for admission operations, whose
/// `resource_name` is an operation UUID rather than an entity key.
pub const OPERATION_RESOURCE_TYPE: &str = gts_id!("cf.core.types_registry.operation.v1~");

/// Typed view of a types-registry `resource_type` (ADR 0005 rule 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// [`TYPE_RESOURCE_TYPE`]: `resource_name` is an entity key.
    Entity,
    /// [`OPERATION_RESOURCE_TYPE`]: `resource_name` is an operation UUID.
    Operation,
    /// A resource type this build does not know; preserved verbatim.
    Unknown(String),
}

impl Resource {
    /// Project a wire `resource_type`.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            TYPE_RESOURCE_TYPE => Self::Entity,
            OPERATION_RESOURCE_TYPE => Self::Operation,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render back to the wire `resource_type`. Inverse of [`Self::from_wire`].
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Entity => TYPE_RESOURCE_TYPE,
            Self::Operation => OPERATION_RESOURCE_TYPE,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl core::fmt::Display for Resource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// SDK-internal canonical-error scope. Mirrors the impl crate's
/// `#[resource_error]` marker so the SDK's `try_new` constructors emit the same
/// `resource_type` the REST ladder does. Its literal MUST equal
/// [`TYPE_RESOURCE_TYPE`] — pinned by `gts_resource_type_round_trips`.
#[resource_error(gts_id!("cf.core.types_registry.entity.v1~"))]
pub(crate) struct TypeResource;

/// Operation error scope; its literal must equal [`OPERATION_RESOURCE_TYPE`].
#[resource_error(gts_id!("cf.core.types_registry.operation.v1~"))]
pub(crate) struct OperationResource;
