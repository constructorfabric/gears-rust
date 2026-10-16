//! Legacy [`TypesRegistryClient`] test support (`test-util`).
//!
//! Seed [`MockTypesRegistryClient`] with its builders and pass it as
//! `Arc<dyn TypesRegistryClient>`. [`make_test_type_schema`] and [`make_test_instance`]
//! build fixtures with synthetic Type Schema chains.

// Test infrastructure: `expect`/`unwrap` are appropriate for synthetic-data
// builders and lock-poisoning paths inside a mock that is only used in tests.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::missing_panics_doc)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::api::TypesRegistryClient;
use super::models::{
    GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    is_type_schema_id,
};
use crate::field;
use crate::gts::TypeResource;
use gts::{GtsId, GtsInstanceId};

/// Canonical identifier validation error for consumer test clients.
#[must_use]
pub fn invalid_gts_id(message: impl Into<String>) -> CanonicalError {
    TypeResource::invalid_argument()
        .with_field_violation(field::GTS_ID_FIELD, message, field::INVALID_GTS_ID)
        .create()
}

/// Canonical entity `NotFound` error for consumer test clients.
#[must_use]
pub fn not_found(id: impl Into<String>) -> CanonicalError {
    let id = id.into();
    TypeResource::not_found(format!("no entity registered: {id}"))
        .with_resource(id)
        .create()
}

/// Opaque canonical `Internal` error for consumer test clients.
#[must_use]
pub fn internal(message: impl Into<String>) -> CanonicalError {
    CanonicalError::internal(message).create()
}

/// In-memory legacy client seeded with [`Self::with_type_schemas`] and
/// [`Self::with_instances`]. Non-empty registration calls panic; empty ones return
/// an empty result. List calls ignore filters and record queries for assertions.
#[derive(Default)]
pub struct MockTypesRegistryClient {
    type_schemas: Vec<GtsTypeSchema>,
    instances: Vec<GtsInstance>,
    list_error: Option<CanonicalError>,
    received_type_schema_queries: Mutex<Vec<TypeSchemaQuery>>,
    received_instance_queries: Mutex<Vec<InstanceQuery>>,
}

impl MockTypesRegistryClient {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the given type-schemas to the registry.
    #[must_use]
    pub fn with_type_schemas(mut self, items: impl IntoIterator<Item = GtsTypeSchema>) -> Self {
        self.type_schemas.extend(items);
        self
    }

    /// Adds the given instances to the registry.
    #[must_use]
    pub fn with_instances(mut self, items: impl IntoIterator<Item = GtsInstance>) -> Self {
        self.instances.extend(items);
        self
    }

    /// Configures the registry so that every `list_*` call fails with the
    /// given error. Useful for testing error-propagation paths.
    #[must_use]
    pub fn with_list_error(mut self, err: CanonicalError) -> Self {
        self.list_error = Some(err);
        self
    }

    /// Number of times [`list_type_schemas`](Self::list_type_schemas) was
    /// called.
    #[must_use]
    pub fn list_type_schema_calls(&self) -> usize {
        self.received_type_schema_queries
            .lock()
            .expect("MockTypesRegistryClient: type-schema query log poisoned")
            .len()
    }

    /// Number of times [`list_instances`](Self::list_instances) was called.
    #[must_use]
    pub fn list_instance_calls(&self) -> usize {
        self.received_instance_queries
            .lock()
            .expect("MockTypesRegistryClient: instance query log poisoned")
            .len()
    }

    /// Snapshot of every [`TypeSchemaQuery`] passed to
    /// [`list_type_schemas`](Self::list_type_schemas), in call order.
    #[must_use]
    pub fn received_type_schema_queries(&self) -> Vec<TypeSchemaQuery> {
        self.received_type_schema_queries
            .lock()
            .expect("MockTypesRegistryClient: type-schema query log poisoned")
            .clone()
    }

    /// Snapshot of every [`InstanceQuery`] passed to
    /// [`list_instances`](Self::list_instances), in call order.
    #[must_use]
    pub fn received_instance_queries(&self) -> Vec<InstanceQuery> {
        self.received_instance_queries
            .lock()
            .expect("MockTypesRegistryClient: instance query log poisoned")
            .clone()
    }
}

#[async_trait]
impl TypesRegistryClient for MockTypesRegistryClient {
    async fn register(&self, entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        assert!(
            entities.is_empty(),
            "MockTypesRegistryClient::register is not implemented; \
             pre-populate via `MockTypesRegistryClient::new().with_type_schemas(...).with_instances(...)`",
        );
        Ok(vec![])
    }

    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        assert!(
            type_schemas.is_empty(),
            "MockTypesRegistryClient::register_type_schemas is not implemented; \
             pre-populate via `MockTypesRegistryClient::new().with_type_schemas(...)`",
        );
        Ok(vec![])
    }

    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        if !is_type_schema_id(type_id) {
            return Err(invalid_gts_id(format!("{type_id} does not end with `~`")));
        }
        GtsId::try_new(type_id).map_err(|e| invalid_gts_id(format!("{e}")))?;
        self.type_schemas
            .iter()
            .find(|s| s.type_id == type_id)
            .cloned()
            .ok_or_else(|| not_found(type_id))
    }

    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        self.type_schemas
            .iter()
            .find(|s| s.type_uuid == type_uuid)
            .cloned()
            .ok_or_else(|| not_found(type_uuid.to_string()))
    }

    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        let mut out = HashMap::with_capacity(type_ids.len());
        for id in type_ids {
            let res = self.get_type_schema(&id).await;
            out.insert(id, res);
        }
        out
    }

    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        let mut out = HashMap::with_capacity(type_uuids.len());
        for uuid in type_uuids {
            let res = self.get_type_schema_by_uuid(uuid).await;
            out.insert(uuid, res);
        }
        out
    }

    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        self.received_type_schema_queries
            .lock()
            .expect("MockTypesRegistryClient: type-schema query log poisoned")
            .push(query);
        if let Some(ref err) = self.list_error {
            return Err(err.clone());
        }
        Ok(self.type_schemas.clone())
    }

    async fn register_instances(
        &self,
        instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        assert!(
            instances.is_empty(),
            "MockTypesRegistryClient::register_instances is not implemented; \
             pre-populate via `MockTypesRegistryClient::new().with_instances(...)`",
        );
        Ok(vec![])
    }

    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        if is_type_schema_id(id) {
            return Err(invalid_gts_id(format!(
                "{id} ends with `~` (looks like a type-schema id)",
            )));
        }
        GtsId::try_new(id).map_err(|e| invalid_gts_id(format!("{e}")))?;
        self.instances
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .ok_or_else(|| not_found(id))
    }

    async fn get_instance_by_uuid(&self, uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        self.instances
            .iter()
            .find(|e| e.uuid == uuid)
            .cloned()
            .ok_or_else(|| not_found(uuid.to_string()))
    }

    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            let res = self.get_instance(&id).await;
            out.insert(id, res);
        }
        out
    }

    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        let mut out = HashMap::with_capacity(uuids.len());
        for uuid in uuids {
            let res = self.get_instance_by_uuid(uuid).await;
            out.insert(uuid, res);
        }
        out
    }

    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        self.received_instance_queries
            .lock()
            .expect("MockTypesRegistryClient: instance query log poisoned")
            .push(query);
        if let Some(ref err) = self.list_error {
            return Err(err.clone());
        }
        Ok(self.instances.clone())
    }
}

/// Builds a Type Schema with empty bodies throughout its synthetic parent chain.
/// Construct the chain manually when tests depend on parent content.
///
/// # Panics
///
/// If `type_id` is not a valid GTS Type Schema identifier.
#[must_use]
pub fn make_test_type_schema(type_id: &str) -> GtsTypeSchema {
    let parent = GtsTypeSchema::derive_parent_type_id(type_id)
        .map(|p| Arc::new(make_test_type_schema(p.as_ref())));
    GtsTypeSchema::try_new(GtsTypeId::new(type_id), serde_json::json!({}), None, parent)
        .expect("synthetic type-schema is valid")
}

/// Builds an Instance with the given content and a synthetic Type Schema chain.
///
/// # Panics
///
/// If `gts_id` is invalid or lacks a Type Schema prefix.
#[must_use]
pub fn make_test_instance(gts_id: &str, content: Value) -> GtsInstance {
    let type_id = GtsInstance::derive_type_id(gts_id)
        .unwrap_or_else(|| panic!("synthetic gts_id {gts_id} has no chain prefix"));
    let type_schema = Arc::new(make_test_type_schema(type_id.as_ref()));
    let segment = &gts_id[type_id.as_ref().len()..];
    let id = GtsInstanceId::new(type_id.as_ref(), segment);
    GtsInstance::try_new(id, content, None, type_schema).expect("synthetic instance is valid")
}
