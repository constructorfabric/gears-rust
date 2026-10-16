//! A cache decorator fixture makes discarded read options observable in returned content.

use async_trait::async_trait;
use gts::{GtsId, GtsInstanceId, GtsTypeId};
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::testing_platform::MockTypesRegistry;
use crate::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionOperation,
    EntityField, FieldSelection, IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse,
    Operation, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection, ReadOptions,
    RegisterEntitiesRequest, RegistrationOperation, TypesRegistryApi, TypesRegistryApiExt,
};

struct CachedReads {
    cached: MockTypesRegistry,
    current: MockTypesRegistry,
    types: Vec<GtsTypeId>,
    instances: Vec<GtsInstanceId>,
}

impl CachedReads {
    fn new(count: usize) -> Self {
        let client = Self {
            cached: MockTypesRegistry::new(),
            current: MockTypesRegistry::new(),
            types: (0..count)
                .map(|n| {
                    GtsTypeId::try_new(&format!("gts.cf.test.options.t{n:03}.v1~")).expect("type")
                })
                .collect(),
            instances: (0..count)
                .map(|n| {
                    GtsInstanceId::try_new(&format!(
                        "gts.cf.test.options.t{n:03}.v1~cf.test.options.one.v1"
                    ))
                    .expect("instance")
                })
                .collect(),
        };
        for (backend, revision) in [(&client.cached, 1), (&client.current, 2)] {
            for id in &client.types {
                backend.seed(
                    id.as_ref(),
                    json!({
                        "$id": format!("gts://{}", id.as_ref()),
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "type": "object",
                        "revision": revision
                    }),
                );
            }
            for id in &client.instances {
                backend.seed(id.as_ref(), json!({"revision": revision}));
            }
        }
        client
    }

    fn backend(&self, fresh: bool) -> &MockTypesRegistry {
        if fresh { &self.current } else { &self.cached }
    }
}

#[async_trait]
impl PlatformTypesRegistryApi for CachedReads {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        PlatformTypesRegistryApi::batch_get_entities(self.backend(request.fresh), ctx, request)
            .await
    }
    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        PlatformTypesRegistryApi::list_entities(&self.current, ctx, request).await
    }
    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        self.current.register_entities(ctx, key, request).await
    }
    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.current.delete_entities(ctx, key, request).await
    }
    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        self.current.get_operation(ctx, operation_id).await
    }
}

#[async_trait]
impl TypesRegistryApi for CachedReads {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        TypesRegistryApi::batch_get_entities(self.backend(request.fresh), ctx, request).await
    }
    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        TypesRegistryApi::list_entities(&self.current, ctx, request).await
    }
}

fn uuid(id: &str) -> Uuid {
    GtsId::try_new(id).expect("identifier").to_uuid()
}

fn options(fresh: bool) -> ReadOptions {
    ReadOptions {
        projection: Projection::Select(FieldSelection::with(&[EntityField::Content])),
        fresh,
    }
}

fn assert_content_projection(schema: &crate::TypeSchema) {
    assert!(schema.content.is_some());
    assert!(schema.origin.is_none(), "origin was not selected");
    assert!(
        schema.resolved_schema.is_none(),
        "resolved schema was not selected"
    );
    assert!(
        schema.effective_traits.is_none(),
        "traits were not selected"
    );
    assert!(
        schema.effective_traits_schema.is_none(),
        "trait schema was not selected"
    );
    assert!(schema.provenance.is_none(), "provenance was not selected");
}

fn assert_instance_content_projection(instance: &crate::Instance) {
    assert!(instance.content.is_some());
    assert!(instance.origin.is_none(), "origin was not selected");
}

macro_rules! plane_tests {
    ($module:ident, $contract:ident, $context:expr) => {
        mod $module {
            use super::*;

            #[tokio::test]
            async fn read_options_single_helpers_can_bypass_a_cache() {
                let client = CachedReads::new(1);
                let api: &dyn $contract = &client;
                let ctx = $context;
                for fresh in [false, true] {
                    let expected = Some(json!(if fresh { 2 } else { 1 }));
                    let schema = api
                        .get_type_schema(&ctx, &client.types[0], options(fresh))
                        .await
                        .expect("read");
                    assert_eq!(
                        schema
                            .content
                            .as_ref()
                            .and_then(|v| v.get("revision"))
                            .cloned(),
                        expected
                    );
                    assert_content_projection(&schema);
                    let schema = api
                        .get_type_schema_by_uuid(
                            &ctx,
                            uuid(client.types[0].as_ref()),
                            options(fresh),
                        )
                        .await
                        .expect("read");
                    assert_eq!(
                        schema
                            .content
                            .as_ref()
                            .and_then(|v| v.get("revision"))
                            .cloned(),
                        expected
                    );
                    assert_content_projection(&schema);
                    let instance = api
                        .get_instance(&ctx, &client.instances[0], options(fresh))
                        .await
                        .expect("read");
                    assert_eq!(
                        instance
                            .content
                            .as_ref()
                            .and_then(|v| v.get("revision"))
                            .cloned(),
                        expected
                    );
                    assert_instance_content_projection(&instance);
                    let instance = api
                        .get_instance_by_uuid(
                            &ctx,
                            uuid(client.instances[0].as_ref()),
                            options(fresh),
                        )
                        .await
                        .expect("read");
                    assert_eq!(
                        instance
                            .content
                            .as_ref()
                            .and_then(|v| v.get("revision"))
                            .cloned(),
                        expected
                    );
                    assert_instance_content_projection(&instance);
                }
                let ordinary = api
                    .get_type_schema(&ctx, &client.types[0], ReadOptions::default())
                    .await
                    .expect("default read");
                assert!(
                    ordinary.content.is_none(),
                    "default reads are document-free"
                );
                assert!(ordinary.resolved_schema.is_none());
                assert!(ordinary.effective_traits.is_none());
                assert!(ordinary.effective_traits_schema.is_none());
            }

            #[tokio::test]
            async fn read_options_batch_helpers_bypass_a_cache_for_every_chunk() {
                let client = CachedReads::new(crate::ext::MAX_BATCH_GET_KEYS + 1);
                let api: &dyn $contract = &client;
                let ctx = $context;
                let type_uuids: Vec<_> = client.types.iter().map(|id| uuid(id.as_ref())).collect();
                let instance_uuids: Vec<_> = client
                    .instances
                    .iter()
                    .map(|id| uuid(id.as_ref()))
                    .collect();
                for fresh in [false, true] {
                    let expected = json!(if fresh { 2 } else { 1 });
                    let schemas = api
                        .batch_get_type_schemas(&ctx, &client.types, options(fresh))
                        .await
                        .expect("read");
                    assert_eq!(schemas.len(), client.types.len());
                    assert!(schemas.values().all(|s| {
                        s.as_ref()
                            .and_then(|s| s.content.as_ref())
                            .and_then(|v| v.get("revision"))
                            == Some(&expected)
                    }));
                    for schema in schemas.values() {
                        assert_content_projection(schema.as_ref().expect("present"));
                    }
                    let schemas = api
                        .batch_get_type_schemas_by_uuid(&ctx, &type_uuids, options(fresh))
                        .await
                        .expect("read");
                    assert_eq!(schemas.len(), type_uuids.len());
                    assert!(schemas.values().all(|s| {
                        s.as_ref()
                            .and_then(|s| s.content.as_ref())
                            .and_then(|v| v.get("revision"))
                            == Some(&expected)
                    }));
                    for schema in schemas.values() {
                        assert_content_projection(schema.as_ref().expect("present"));
                    }
                    let instances = api
                        .batch_get_instances(&ctx, &client.instances, options(fresh))
                        .await
                        .expect("read");
                    assert_eq!(instances.len(), client.instances.len());
                    assert!(instances.values().all(|s| {
                        s.as_ref()
                            .and_then(|s| s.content.as_ref())
                            .and_then(|v| v.get("revision"))
                            == Some(&expected)
                    }));
                    for instance in instances.values() {
                        assert_instance_content_projection(instance.as_ref().expect("present"));
                    }
                    let instances = api
                        .batch_get_instances_by_uuid(&ctx, &instance_uuids, options(fresh))
                        .await
                        .expect("read");
                    assert_eq!(instances.len(), instance_uuids.len());
                    assert!(instances.values().all(|s| {
                        s.as_ref()
                            .and_then(|s| s.content.as_ref())
                            .and_then(|v| v.get("revision"))
                            == Some(&expected)
                    }));
                    for instance in instances.values() {
                        assert_instance_content_projection(instance.as_ref().expect("present"));
                    }
                }
                assert_eq!(
                    client
                        .current
                        .calls(crate::testing_platform::Call::BatchGet),
                    8,
                    "four helpers read two chunks each"
                );
            }
        }
    };
}

plane_tests!(
    platform,
    PlatformTypesRegistryApi,
    PlatformSecurityContext::outbound_marker()
);
plane_tests!(tenant, TypesRegistryApi, SecurityContext::anonymous());
