//! Legacy registry client for global GTS entities; calls require no security context.

use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::models::{GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery};

/// Legacy registry client, resolved through `ClientHub` as `dyn TypesRegistryClient`.
///
/// Calls require no security context. Method and item failures carry [`CanonicalError`];
/// convert to [`crate::TypesRegistryError`] for typed matching.
#[async_trait]
pub trait TypesRegistryClient: Send + Sync {
    // Generic batch register (kind detected from gts_id suffix per item).

    /// Register GTS entities (type-schemas or instances) in batch.
    ///
    /// Each item needs a valid GTS identifier in a configured ID field
    /// (`$id`, `gtsId`, `id`). Include dependencies in the batch or register them first.
    ///
    /// Per-item failures use [`RegisterResult::Err`]; success carries the canonical ID.
    /// Read the entity with [`Self::get_type_schema`] or [`Self::get_instance`].
    ///
    /// # Errors
    ///
    /// Call-level errors; individual failures are returned per item.
    async fn register(
        &self,
        entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError>;

    // Type Schema operations.

    /// Register GTS type-schemas in batch.
    ///
    /// Wrong-kind identifiers return per-item `InvalidArgument` errors. A missing
    /// parent may return [`ParentNotRegistered`](crate::TypesRegistryError::ParentNotRegistered);
    /// register the parent before retrying.
    ///
    /// # Errors
    ///
    /// Call-level errors; individual failures are returned per item.
    async fn register_type_schemas(
        &self,
        type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError>;

    /// Retrieve a registered GTS type-schema by its type id.
    ///
    /// # Errors
    ///
    /// * `NotFound` — no type-schema with this id is registered.
    /// * `InvalidArgument` (reason `INVALID_GTS_ID`) — id format is invalid,
    ///   kind-mismatched, or resolves to a non-type-schema entity.
    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError>;

    /// Retrieve a registered GTS type-schema by its deterministic UUID v5.
    ///
    /// # Errors
    ///
    /// * `NotFound` — no type-schema is registered with this UUID (also
    ///   returned when the UUID exists but points to an instance).
    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError>;

    /// Retrieve multiple type-schemas by id in one call.
    ///
    /// Returns a map keyed by the input ids; each value is a per-item
    /// `Result` carrying either the resolved schema or the per-item
    /// `CanonicalError` (`NotFound`, or `InvalidArgument` with reason
    /// `INVALID_GTS_ID`, …). Duplicate ids in the input collapse to a single
    /// entry. The map always has a value for every distinct input id.
    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>>;

    /// Retrieve multiple type-schemas by deterministic UUID v5 in one call.
    ///
    /// Same per-key semantics as [`Self::get_type_schemas`]: a map keyed
    /// by the input UUIDs, per-item failures carried in the inner
    /// `Result`. Duplicates collapse.
    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>>;

    /// List registered GTS type-schemas matching the query.
    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError>;

    // Instance operations.

    /// Register GTS instances in batch.
    ///
    /// Wrong-kind identifiers return per-item `InvalidArgument` errors. An unregistered
    /// Type Schema may return [`ParentNotRegistered`](crate::TypesRegistryError::ParentNotRegistered).
    ///
    /// # Errors
    ///
    /// Call-level errors; individual failures are returned per item.
    async fn register_instances(
        &self,
        instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError>;

    /// Retrieve a registered GTS instance by its instance id.
    ///
    /// # Errors
    ///
    /// * `NotFound` — no instance with this id is registered.
    /// * `InvalidArgument` (reason `INVALID_GTS_ID`) — id format is invalid,
    ///   kind-mismatched, or resolves to a non-instance entity.
    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError>;

    /// Retrieve a registered GTS instance by its deterministic UUID v5.
    ///
    /// # Errors
    ///
    /// * `NotFound` — no instance is registered with this UUID (also returned
    ///   when the UUID exists but points to a type-schema).
    async fn get_instance_by_uuid(&self, uuid: Uuid) -> Result<GtsInstance, CanonicalError>;

    /// Retrieve multiple instances by id in one call.
    ///
    /// Returns a map keyed by the input ids; each value is a per-item
    /// `Result` carrying either the resolved instance or the per-item
    /// `CanonicalError` (`NotFound`, or `InvalidArgument` with reason
    /// `INVALID_GTS_ID`, …). Duplicate ids in the input collapse to a single
    /// entry.
    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>>;

    /// Retrieve multiple instances by deterministic UUID v5 in one call.
    ///
    /// Same per-key semantics as [`Self::get_instances`]: a map keyed by
    /// the input UUIDs, per-item failures carried in the inner `Result`.
    /// Duplicates collapse.
    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>>;

    /// List registered GTS instances matching the query.
    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError>;
}
