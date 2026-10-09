//! Types Registry SDK
//!
//! Public API for `types-registry`, with a legacy surface until the T31 cutover.
//!
//! **New surface** (SPEC §10.1, D15):
//! - [`PlatformTypesRegistryApi`] — the platform toolkit contract; [`PlatformTypesRegistryApiExt`]
//!   — helpers composed from it
//! - [`TypesRegistryApi`] — the tenant toolkit contract, entity reads only;
//!   [`TypesRegistryApiExt`] — the same read helpers over it
//! - [`ext`] says which contract a client resolves and which helpers it imports
//! - [`models`] — serde-free requests, operations and [`Entity`] snapshots;
//!   read helpers return kind-typed [`TypeSchema`] / [`Instance`]
//! - [`PlatformTypesRegistryApiExt::reconcile_entities_and_await`] — reconcile explicit
//!   documents, returning [`Reconciliation`] and per-identifier [`ReconcileOutcome`]s
//!
//! **Old surface** (`legacy`, deleted in T31 with no shim; re-exported at the crate root):
//! - `TypesRegistryClient` trait for inter-gear communication. Per
//!   [ADR 0005][adr] every fallible method (and every per-item `Result` it
//!   returns) carries [`toolkit_canonical_errors::CanonicalError`].
//! - `GtsTypeSchema` / `GtsInstance` typed entity models; their new-surface counterparts are
//!   [`TypeSchema`] / [`Instance`]
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
pub mod error;
pub mod ext;
pub mod field;
pub mod gts;
pub mod item_failure;
pub mod models;
pub mod precondition;
pub mod reason;
mod reconcile;
mod submit;

/// An in-memory `PlatformTypesRegistryApi` for consumer and SDK tests.
#[cfg(any(test, feature = "test-util"))]
#[expect(
    clippy::expect_used,
    reason = "test support: a malformed fixture identifier is a bug in the test"
)]
pub mod testing_platform;

pub use contract::{PlatformTypesRegistryApi, TypesRegistryApi};
pub use error::{FieldIssue, TypesRegistryError};
pub use ext::{PlatformTypesRegistryApiExt, TypesRegistryApiExt};
pub use gts::{OPERATION_RESOURCE_TYPE, TYPE_RESOURCE_TYPE};
pub use item_failure::AdmissionFailure;
pub use models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, CandidateStatus, Cursor,
    DeleteEntitiesRequest, DeleteItem, DeletionItemResult, DeletionOperation, Entity, EntityField,
    EntityFilter, EntityKey, EntityKind, EntityLookup, FieldSelection, IdempotencyKey, Instance,
    JsonDocument, LifecycleFilter, LifecycleStatus, ListEntitiesRequest, ListEntitiesResponse,
    Operation, OperationStatus, Origin, PageRequest, Projection, Provenance, PublisherContext,
    PublisherVersion, PublisherVersionError, RegisterEntitiesRequest, RegisterItem,
    RegistrationItemResult, RegistrationOperation, TypeSchema, Validator,
};
pub use reconcile::{ReconcileOptions, ReconcileOutcome, ReconcilePendingCause, Reconciliation};

// The old surface, deleted in T31 (see `legacy`). Re-exported at the crate root and, for
// its mock, at `testing`.
mod legacy;

#[cfg(feature = "test-util")]
pub use legacy::testing;

pub use legacy::api::TypesRegistryClient;
pub use legacy::models::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, RegisterSummary, TypeSchemaQuery,
    is_type_schema_id,
};

// Re-export the underlying gts identifier types so consumers don't need a
// direct dependency on `gts` for typed IDs. Leading `::` selects the external
// `gts` crate over this crate's local `gts` gear (the canonical-error vocab).
pub use ::gts::{GtsInstanceId, GtsTypeId};
