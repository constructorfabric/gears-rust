//! GTS resource types for canonical entity and operation errors.
//!
//! Resource marker literals must match these constants and the registry's markers.

use toolkit_canonical_errors::resource_error;
use toolkit_gts::gts_id;

/// The canonical GTS resource type for types-registry entities. Lands in
/// `CanonicalError` `resource_type` / the wire `context.resource_type`.
pub const TYPE_RESOURCE_TYPE: &str = gts_id!("cf.core.types_registry.entity.v1~");

/// The canonical GTS resource type for admission operations, whose
/// `resource_name` is an operation UUID rather than an entity key.
pub const OPERATION_RESOURCE_TYPE: &str = gts_id!("cf.core.types_registry.operation.v1~");

/// Typed view of a types-registry `resource_type`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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

/// SDK entity-error scope; its literal must equal [`TYPE_RESOURCE_TYPE`].
#[resource_error(gts_id!("cf.core.types_registry.entity.v1~"))]
pub(crate) struct TypeResource;

/// Operation error scope; its literal must equal [`OPERATION_RESOURCE_TYPE`].
#[resource_error(gts_id!("cf.core.types_registry.operation.v1~"))]
pub(crate) struct OperationResource;
