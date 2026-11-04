#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod contract;
pub mod error;
pub mod ext;
pub mod field;
pub mod gts;
pub mod item_failure;
pub mod models;
#[cfg(any(feature = "rest-client", feature = "rest-server"))]
mod paths;
pub mod precondition;
mod publication;
pub mod reason;
mod reconcile;
#[cfg(feature = "rest-server")]
mod rest_binding;
#[cfg(feature = "rest-client")]
mod rest_client;

#[cfg(any(test, feature = "test-util"))]
#[expect(
    clippy::expect_used,
    reason = "test support: a malformed fixture identifier is a bug in the test"
)]
pub mod testing_platform;

pub use contract::{
    PlatformTypesRegistryApi, TypesRegistryApi, platform_types_registry_api_ir,
    types_registry_api_ir,
};
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

pub use publication::{PublicationError, publish_startup_entities};
pub use reconcile::{
    ReconcileOptions, ReconcileOutcome, ReconcilePendingCause, ReconcileReport, ReconcileStop,
    Reconciliation, STARTUP_PUBLICATION_DEADLINE,
};
#[cfg(feature = "rest-server")]
pub use rest_binding::{
    platform_types_registry_api_rest_http_binding, types_registry_api_rest_http_binding,
};
#[cfg(feature = "rest-client")]
pub use rest_client::{
    PLATFORM_BASE_PATH, PlatformTypesRegistryApiRestClient,
    PlatformTypesRegistryApiRestResolvingClient, TENANT_BASE_PATH, TypesRegistryApiRestClient,
    TypesRegistryApiRestResolvingClient,
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
