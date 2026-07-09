//! Types Registry SDK
//!
//! This crate provides the public API for the `types-registry` gear:
//! - `TypesRegistryClient` trait for inter-gear communication. Per
//!   [ADR 0005][adr] every fallible method (and every per-item `Result` it
//!   returns) carries [`toolkit_canonical_errors::CanonicalError`].
//! - `GtsTypeSchema` / `GtsInstance` typed entity models
//! - `TypeSchemaQuery` / `InstanceQuery` for filtering
//! - `GtsTypeId` / `GtsInstanceId` typed identifiers
//! - [`TypesRegistryError`] — opt-in `From<CanonicalError>` projection (see
//!   [`error`]) plus its co-located wire vocabulary ([`field`],
//!   [`precondition`], [`gts`])
//!
//! ## Usage
//!
//! Consumers obtain the client from `ClientHub`:
//! ```ignore
//! use types_registry_sdk::{TypeSchemaQuery, TypesRegistryClient};
//!
//! let client = hub.get::<dyn TypesRegistryClient>()?;
//!
//! let schema = client.get_type_schema("gts.acme.core.events.user.v1~").await?;
//! let schemas = client
//!     .list_type_schemas(TypeSchemaQuery::default().with_pattern("gts.acme.*"))
//!     .await?;
//! ```
//!
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod error;
pub mod field;
pub mod gts;
pub mod models;
pub mod precondition;

#[cfg(feature = "test-util")]
pub mod testing;

pub use api::TypesRegistryClient;
pub use error::{FieldIssue, TypesRegistryError};
pub use gts::TYPE_RESOURCE_TYPE;
pub use models::{
    GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, RegisterSummary,
    TypeSchemaQuery, is_type_schema_id,
};

// Re-export the underlying gts identifier types so consumers don't need a
// direct dependency on `gts` for typed IDs. Leading `::` selects the external
// `gts` crate over this crate's local `gts` gear (the canonical-error vocab).
pub use ::gts::GtsInstanceId;
