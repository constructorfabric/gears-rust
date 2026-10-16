//! Public API for Types Registry.
//!
//! - [`PlatformTypesRegistryApi`] provides reads, mutations and operation polling;
//!   [`PlatformTypesRegistryApiExt`] adds typed reads, pagination and reconciliation.
//! - [`TypesRegistryApi`] and [`TypesRegistryApiExt`] provide tenant-context reads.
//! - [`models`] defines requests, operations and projected entity snapshots.
//! - [`TypesRegistryError`] provides a typed view of canonical errors;
//!   [`AdmissionFailure`] decodes operation-item failures.
//! - [`ReconcileReport`] decodes incomplete reconciliation errors.
//! - [`GtsTypeId`] and [`GtsInstanceId`] are typed GTS identifiers.
//!
//! The legacy [`TypesRegistryClient`], its models and its `testing` mock remain
//! available at the crate root. New consumers should use the API contracts above.

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
pub use item_failure::{AdmissionFailure, AdmissionFailureReason, DependencyKind};
pub use models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, CandidateStatus, Cursor,
    DeleteEntitiesRequest, DeleteItem, DeletionItemResult, DeletionOperation, DeletionOutcome,
    Entity, EntityField, EntityFilter, EntityKey, EntityKind, EntityLookup, FieldSelection,
    IdempotencyKey, Instance, JsonDocument, LifecycleFilter, LifecycleStatus, ListEntitiesRequest,
    ListEntitiesResponse, Operation, OperationStatus, Origin, PageRequest, Projection, Provenance,
    PublisherContext, PublisherVersion, PublisherVersionError, ReadOptions,
    RegisterEntitiesRequest, RegisterItem, RegistrationItemResult, RegistrationOperation,
    RegistrationOutcome, TypeSchema, Validator,
};

pub use reconcile::{
    ReconcileOptions, ReconcileOutcome, ReconcilePendingCause, ReconcileReport, ReconcileStop,
    Reconciliation,
};

// Legacy API and model re-exports.
mod legacy;

#[cfg(feature = "test-util")]
pub use legacy::testing;

pub use legacy::api::TypesRegistryClient;
pub use legacy::models::{
    AncestorIter, GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, RegisterSummary,
    TypeSchemaQuery, is_type_schema_id,
};

// Re-export GTS types needed to construct requests.
pub use ::gts::{GtsId, GtsIdPattern, GtsIdSegment, GtsInstanceId, GtsTypeId};
