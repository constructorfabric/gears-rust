//! Test utilities for [`TypesRegistryClient`] consumers.
//!
//! [`MockTypesRegistryClient`] is a hand-rolled, stateful mock backend: pre-populate
//! it with [`with_type_schemas`](MockTypesRegistryClient::with_type_schemas) /
//! [`with_instances`](MockTypesRegistryClient::with_instances), hand it to the code
//! under test as `Arc<dyn TypesRegistryClient>`, and let it answer `get_*` /
//! `list_*` calls against the in-memory data.
//!
//! Helper builders [`make_test_type_schema`] and [`make_test_instance`]
//! produce minimal valid values for tests where the schema / instance content
//! is not the focus of the assertion.
//!
//! Available with the `test-util` cargo feature.

// Test infrastructure: `expect`/`unwrap` are appropriate for synthetic-data
// builders and lock-poisoning paths inside a mock that is only used in tests.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::missing_panics_doc)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use crate::api::TypesRegistryClient;
use crate::field;
use crate::gts::TypeResource;
use crate::models::{
    GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    is_type_schema_id,
};
use gts::{GtsID, GtsInstanceId};

/// Builds the `InvalidArgument` canonical error the registry returns for a
/// malformed / kind-mismatched GTS id (reason [`field::INVALID_GTS_ID`]),
/// matching the real client's classification.
///
/// Exposed so consumer test fakes that implement [`TypesRegistryClient`] can
/// synthesize the same canonical envelopes the real client emits.
#[must_use]
pub fn invalid_gts_id(message: impl Into<String>) -> CanonicalError {
    TypeResource::invalid_argument()
        .with_field_violation(field::GTS_ID_FIELD, message, field::INVALID_GTS_ID)
        .create()
}

/// Builds the `NotFound` canonical error the registry returns for an
/// unregistered id / UUID, tagged with the types-registry resource type.
///
/// Exposed for consumer test fakes (see [`invalid_gts_id`]).
#[must_use]
pub fn not_found(id: impl Into<String>) -> CanonicalError {
    let id = id.into();
    TypeResource::not_found(format!("no entity registered: {id}"))
        .with_resource(id)
        .create()
}

/// Builds the opaque `Internal` canonical error the registry returns for an
/// infrastructure failure.
///
/// Exposed for consumer test fakes (see [`invalid_gts_id`]).
#[must_use]
pub fn internal(message: impl Into<String>) -> CanonicalError {
    CanonicalError::internal(message).create()
}

/// Stateful in-memory implementation of [`TypesRegistryClient`] for tests.
///
/// The mock is read-mostly: build it with the builder methods, then hand to
/// the code under test. `register_*` methods are **not implemented** —
/// calling any of them with a non-empty input panics. Pre-populate via
/// [`with_type_schemas`](Self::with_type_schemas) /
/// [`with_instances`](Self::with_instances) builders instead. Empty-input
/// `register_*` calls return an empty result vector to keep the trait
/// shape consistent for tests that pass `vec![]` defensively.
///
/// `list_*` methods return all stored entries verbatim and ignore the
/// query (callers wanting filtered results should pre-filter what they
/// put in); the query passed in is captured for assertions via
/// [`received_instance_queries`](Self::received_instance_queries).
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
        GtsID::new(type_id).map_err(|e| invalid_gts_id(format!("{e}")))?;
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
        GtsID::new(id).map_err(|e| invalid_gts_id(format!("{e}")))?;
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

/// Builds a synthetic [`GtsTypeSchema`] with the given `type_id` and an empty
/// JSON Schema body. Convenient for tests that need a `GtsTypeSchema` value
/// but don't care about the schema content.
///
/// For derived ids, the parent chain is built recursively by emitting one
/// synthetic schema per chain hop (root → ... → leaf). Chain-aware methods
/// like [`GtsTypeSchema::ancestors`] / [`GtsTypeSchema::effective_schema`]
/// therefore observe a complete chain matching `type_id`. Each synthetic
/// schema along the chain carries an empty body, so semantic content from
/// real schemas is not modelled — tests that rely on parent-body details
/// must construct the chain manually.
///
/// # Panics
///
/// Panics if `type_id` is not a valid GTS type-schema identifier (must end
/// with `~` and parse as a full GTS id).
#[must_use]
pub fn make_test_type_schema(type_id: &str) -> GtsTypeSchema {
    let parent = GtsTypeSchema::derive_parent_type_id(type_id)
        .map(|p| Arc::new(make_test_type_schema(p.as_ref())));
    GtsTypeSchema::try_new(GtsTypeId::new(type_id), serde_json::json!({}), None, parent)
        .expect("synthetic type-schema is valid")
}

/// Builds a synthetic [`GtsInstance`] with the given content body, attached
/// to a synthetic type-schema chain matching the instance id's prefix.
///
/// The instance's `id` must contain at least one `~`. The prefix (everything
/// up to and including the last `~`) is used as the parent type-schema's
/// `type_id`, and the full chain leading up to it is built via
/// [`make_test_type_schema`].
///
/// # Panics
///
/// Panics if `gts_id` doesn't contain a `~` (no chain prefix) or doesn't
/// parse as a valid GTS identifier.
#[must_use]
pub fn make_test_instance(gts_id: &str, content: Value) -> GtsInstance {
    let type_id = GtsInstance::derive_type_id(gts_id)
        .unwrap_or_else(|| panic!("synthetic gts_id {gts_id} has no chain prefix"));
    let type_schema = Arc::new(make_test_type_schema(type_id.as_ref()));
    let segment = &gts_id[type_id.as_ref().len()..];
    let id = GtsInstanceId::new(type_id.as_ref(), segment);
    GtsInstance::try_new(id, content, None, type_schema).expect("synthetic instance is valid")
}
