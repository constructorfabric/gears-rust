//! Test helpers shared across crate-internal integration tests for the
//! GTS Types Registry SDK adapter. Promoted to `pub(crate)` (compiled
//! under `#[cfg(test)]` only) so service-level tests in
//! `domain/tenant/service/service_tests.rs` can wire the production
//! [`super::GtsTenantTypeChecker`] against a slow / failing fake
//! without re-stubbing the full ~13-method `TypesRegistryClient`
//! trait at every call site.

#![cfg(test)]

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

/// Minimal `TypesRegistryClient` fake whose `get_type_schemas_by_uuid`
/// sleeps for `delay` before returning an empty map. Any other trait
/// method is `unreachable!()` — service-level integration tests
/// exercise only the type-checker barrier path. Use with
/// `#[tokio::test(start_paused = true)]` to verify the production
/// timeout boundary fires deterministically.
pub struct SlowRegistry {
    pub delay: Duration,
}

impl SlowRegistry {
    pub fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

#[async_trait]
impl TypesRegistryClient for SlowRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!()
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!()
    }
    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        unreachable!()
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        unreachable!()
    }
    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        unreachable!()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        tokio::time::sleep(self.delay).await;
        HashMap::new()
    }
    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        unreachable!()
    }
    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!()
    }
    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        unreachable!()
    }
    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        unreachable!()
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        unreachable!()
    }
    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        unreachable!()
    }
    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        unreachable!()
    }
}
