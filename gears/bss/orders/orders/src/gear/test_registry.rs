//! Recording registry double: validates initialization handling, not GTS conformance.
#![allow(unused_variables)]
use async_trait::async_trait;
use std::collections::HashMap;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

pub(super) struct Registry {
    pub mode: &'static str,
}
#[async_trait]
impl TypesRegistryClient for Registry {
    async fn register(
        &self,
        entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        if self.mode == "incomplete" {
            return Ok(vec![]);
        }
        if self.mode == "refused" {
            return Ok(vec![RegisterResult::Err {
                gts_id: None,
                error: CanonicalError::service_unavailable().create(),
            }]);
        }
        if self.mode == "hanging" {
            return std::future::pending().await;
        }
        Ok(entities
            .iter()
            .map(|v| RegisterResult::Ok {
                gts_id: v
                    .get("$id")
                    .or_else(|| v.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .expect("registered entities have identifiers")
                    .trim_start_matches("gts://")
                    .to_owned(),
            })
            .collect())
    }
    async fn register_type_schemas(
        &self,
        type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn register_instances(
        &self,
        instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_instance_by_uuid(&self, uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        panic!("unexpected registry read during scaffold initialization")
    }
    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        panic!("unexpected registry read during scaffold initialization")
    }
}
