//! Types Registry SDK
//!
//! This crate provides the public API for the `types-registry` gear. It carries two surfaces
//! until T29 moves every consumer onto the new one.
//!
//! **New surface** (SPEC §10.1, D15):
//! - [`PlatformTypesRegistryApi`] — the toolkit contract; [`PlatformTypesRegistryApiExt`] —
//!   helpers composed from it
//! - [`entity_models`] — the serde-free request, snapshot and operation models
//! - [`publication`] — publisher identity and per-identifier publication state
//!
//! **Old surface** (`legacy`, deleted in T29 with no shim):
//! - `TypesRegistryClient` trait for inter-gear communication. Per
//!   [ADR 0005][adr] every fallible method (and every per-item `Result` it
//!   returns) carries [`toolkit_canonical_errors::CanonicalError`].
//! - `GtsTypeSchema` / `GtsInstance` typed entity models
//! - `TypeSchemaQuery` / `InstanceQuery` for filtering
//!
//! **Shared:**
//! - `GtsTypeId` / `GtsInstanceId` typed identifiers
//! - [`TypesRegistryError`] — opt-in `From<CanonicalError>` projection (see
//!   [`error`]) plus its co-located wire vocabulary ([`field`],
//!   [`precondition`], [`gts`])
//!
//! ## Usage of the old surface
//!
//! Consumers obtain the client from `ClientHub`:
//! ```ignore
//! use types_registry_sdk::{TypeSchemaQuery, TypesRegistryClient};
//!
//! let client = hub.get::<dyn TypesRegistryClient>()?;
//!
//! let schema = client.get_type_schema(gts_id!("acme.core.events.user.v1~")).await?;
//! let schemas = client
//!     .list_type_schemas(TypeSchemaQuery::default().with_pattern(format!("{GTS_ID_PREFIX}acme.*")))
//!     .await?;
//! ```
//!
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod contract;
pub mod entity_models;
pub mod error;
pub mod ext;
pub mod field;
pub mod gts;
pub mod item_failure;
pub mod precondition;
pub mod publication;

/// An in-memory `PlatformTypesRegistryApi` for consumer and SDK tests.
#[cfg(any(test, feature = "test-util"))]
#[expect(
    clippy::expect_used,
    reason = "test support: a malformed fixture identifier is a bug in the test"
)]
pub mod testing_platform;

pub use contract::PlatformTypesRegistryApi;
pub use entity_models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, CandidateStatus, Cursor,
    DeleteEntitiesRequest, DeleteItem, DeletionItemResult, DeletionOperation, EntityField,
    EntityFilter, EntityKey, EntityKind, EntityLookup, EntitySnapshot, FieldSelection,
    IdempotencyKey, JsonDocument, LifecycleFilter, LifecycleStatus, ListEntitiesRequest,
    ListEntitiesResponse, Operation, OperationStatus, Origin, PageRequest, Projection, Provenance,
    PublisherContext, PublisherVersion, RegisterEntitiesRequest, RegisterItem,
    RegistrationItemResult, RegistrationOperation, Validator,
};
pub use error::{FieldIssue, TypesRegistryError};
pub use ext::PlatformTypesRegistryApiExt;
pub use gts::{OPERATION_RESOURCE_TYPE, TYPE_RESOURCE_TYPE};
pub use item_failure::AdmissionFailure;
pub use publication::{
    GtsDeclaration, PendingReason, PublicationState, PublicationStatus, PublisherVersionError,
    RejectionReason, SupersededEntity,
};

// The old surface, deleted in T29 (see `legacy`). Re-exported at its original paths.
mod legacy;

#[cfg(feature = "test-util")]
pub use legacy::testing;
pub use legacy::{api, models};

pub use api::TypesRegistryClient;
pub use models::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, RegisterSummary, TypeSchemaQuery,
    is_type_schema_id,
};

// Re-export the underlying gts identifier types so consumers don't need a
// direct dependency on `gts` for typed IDs. Leading `::` selects the external
// `gts` crate over this crate's local `gts` gear (the canonical-error vocab).
pub use ::gts::{GtsInstanceId, GtsTypeId};
