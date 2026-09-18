use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::{gts_id, gts_uri};
use types_registry_sdk::{
    GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient,
};
use uuid::Uuid;

use super::*;

const ROOT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~");

fn config(idp_provisioning: bool) -> RootTypeConfig {
    RootTypeConfig {
        gts_id: GtsTypeId::new(ROOT_TYPE),
        idp_provisioning,
    }
}

#[derive(Clone, Copy)]
enum Response {
    Success,
    Drift,
    OuterFailure,
    Empty,
    WrongId,
}

struct RecordingRegistry {
    response: Response,
    registrations: Mutex<Vec<Vec<Value>>>,
}

impl RecordingRegistry {
    fn new(response: Response) -> Self {
        Self {
            response,
            registrations: Mutex::new(Vec::new()),
        }
    }

    fn registrations(&self) -> Vec<Vec<Value>> {
        self.registrations.lock().clone()
    }
}

#[async_trait]
impl TypesRegistryClient for RecordingRegistry {
    async fn register(&self, _entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!("root-type registration uses the typed schema method")
    }

    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.registrations.lock().push(type_schemas);
        match self.response {
            Response::Success => Ok(vec![RegisterResult::Ok {
                gts_id: ROOT_TYPE.to_owned(),
            }]),
            Response::Drift => Ok(vec![RegisterResult::Err {
                gts_id: Some(ROOT_TYPE.to_owned()),
                error: CanonicalError::internal("schema already exists with different content")
                    .create(),
            }]),
            Response::OuterFailure => {
                Err(CanonicalError::internal("registry unavailable").create())
            }
            Response::Empty => Ok(Vec::new()),
            Response::WrongId => Ok(vec![RegisterResult::Ok {
                gts_id: gts_id!("cf.core.am.tenant_type.v1~cf.core.am.other.v1~").to_owned(),
            }]),
        }
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
        unreachable!()
    }

    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        unreachable!()
    }

    async fn register_instances(
        &self,
        _instances: Vec<Value>,
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

#[test]
fn desired_schema_is_the_am_owned_root_contract() {
    let desired = desired_root_schema(&config(true)).expect("desired schema");

    assert_eq!(
        desired["$id"],
        json!(gts_uri!(
            "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        ))
    );
    assert_eq!(
        desired["$schema"],
        json!("http://json-schema.org/draft-07/schema#")
    );
    assert_eq!(desired["type"], json!("object"));
    assert_eq!(
        desired.pointer("/allOf/0/$ref"),
        Some(&json!(gts_uri!("cf.core.am.tenant_type.v1~")))
    );
    assert_eq!(
        desired.pointer("/x-gts-traits/allowed_parent_types"),
        Some(&json!([]))
    );
    assert_eq!(
        desired.pointer("/x-gts-traits/idp_provisioning"),
        Some(&json!(true))
    );
}

#[tokio::test]
async fn registration_uses_the_existing_typed_client() {
    let registry = RecordingRegistry::new(Response::Success);
    let cfg = config(false);

    register_root_type(&registry, &cfg)
        .await
        .expect("root type registration");

    assert_eq!(
        registry.registrations(),
        vec![vec![desired_root_schema(&cfg).expect("desired schema")]]
    );
}

#[tokio::test]
async fn schema_drift_is_startup_fatal() {
    let registry = RecordingRegistry::new(Response::Drift);

    let error = register_root_type(&registry, &config(false))
        .await
        .expect_err("different content under the root id must fail");

    assert!(error.to_string().contains("conflicts"));
    assert!(error.to_string().contains(ROOT_TYPE));
}

#[tokio::test]
async fn registry_failure_is_startup_fatal() {
    let registry = RecordingRegistry::new(Response::OuterFailure);

    register_root_type(&registry, &config(false))
        .await
        .expect_err("registry failure must fail registration");
}

#[tokio::test]
async fn malformed_registration_response_is_startup_fatal() {
    for response in [Response::Empty, Response::WrongId] {
        let registry = RecordingRegistry::new(response);
        assert!(register_root_type(&registry, &config(false)).await.is_err());
    }
}
