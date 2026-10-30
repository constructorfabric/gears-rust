//! Identifier <-> Registry Reference resolution, on both contracts, both kinds and both
//! directions.
//!
//! A recording decorator makes every read observable: its request, and which context
//! object it was made under. It can also answer a Registry Reference other than the
//! identifier's own, to show the helpers return what the registry read, not a local hash.

use std::collections::HashMap;

use async_trait::async_trait;
use gts::{GtsId, GtsInstanceId, GtsTypeId};
use parking_lot::Mutex;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use super::MAX_BATCH_GET_KEYS;
use crate::testing_platform::{Call, Fault, MockTypesRegistry, ProtocolFault, Seed};
use crate::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, DeleteEntitiesRequest,
    DeletionOperation, EntityKey, EntityLookup, FieldSelection, IdempotencyKey,
    ListEntitiesRequest, ListEntitiesResponse, Operation, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, RegisterEntitiesRequest, RegistrationOperation,
    TypesRegistryApi, TypesRegistryApiExt,
};

const TYPE: &str = "gts.cf.test.resolve.thing.v1~";
const DELETED_TYPE: &str = "gts.cf.test.resolve.gone.v1~";
const UNREGISTERED_TYPE: &str = "gts.cf.test.resolve.absent.v1~";
/// Chained on [`TYPE`], so answering the Type Schema's identifier instead would show.
const INSTANCE: &str = "gts.cf.test.resolve.thing.v1~cf.test.resolve.one.v1";
const DELETED_INSTANCE: &str = "gts.cf.test.resolve.thing.v1~cf.test.resolve.two.v1";
const UNREGISTERED_INSTANCE: &str = "gts.cf.test.resolve.thing.v1~cf.test.resolve.absent.v1";

/// Records every batch read and the address of the context it was made under.
#[derive(Default)]
struct Recording {
    inner: MockTypesRegistry,
    reads: Mutex<Vec<BatchGetEntitiesRequest>>,
    contexts: Mutex<Vec<usize>>,
    /// When set, every found snapshot answers this Registry Reference.
    answered_uuid: Mutex<Option<Uuid>>,
}

impl Recording {
    fn seeded() -> Self {
        let recording = Self::default();
        recording.inner.seed(TYPE, json!({}));
        recording.inner.seed(INSTANCE, json!({}));
        for deleted in [DELETED_TYPE, DELETED_INSTANCE] {
            recording
                .inner
                .seed_entity(Seed::new(deleted, json!({})).deleted());
        }
        recording
    }

    fn reads(&self) -> Vec<BatchGetEntitiesRequest> {
        self.reads.lock().clone()
    }

    fn record<C>(&self, ctx: &C, request: &BatchGetEntitiesRequest) {
        self.reads.lock().push(request.clone());
        self.contexts.lock().push(address(ctx));
    }

    fn answer(&self, mut response: BatchGetEntitiesResponse) -> BatchGetEntitiesResponse {
        if let Some(uuid) = *self.answered_uuid.lock() {
            for lookup in response.0.values_mut() {
                if let EntityLookup::Found { entity, .. } = lookup {
                    entity.gts_uuid = uuid;
                }
            }
        }
        response
    }

    /// Every read selected the mandatory fields only, through the cache, under `ctx`
    /// itself — the very object the caller passed.
    fn assert_id_only_reads_under<C>(&self, ctx: &C) {
        for read in self.reads() {
            assert_eq!(
                read.projection,
                Projection::Select(FieldSelection::light()),
                "no document is read"
            );
            assert!(!read.fresh, "the cache is not bypassed");
        }
        assert!(
            self.contexts
                .lock()
                .iter()
                .all(|&seen| seen == address(ctx)),
            "every read is made under the caller's own context"
        );
    }
}

fn address<C>(ctx: &C) -> usize {
    std::ptr::from_ref(ctx) as usize
}

#[async_trait]
impl PlatformTypesRegistryApi for Recording {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.record(ctx, &request);
        let response =
            PlatformTypesRegistryApi::batch_get_entities(&self.inner, ctx, request).await?;
        Ok(self.answer(response))
    }
    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        PlatformTypesRegistryApi::list_entities(&self.inner, ctx, request).await
    }
    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        self.inner.register_entities(ctx, key, request).await
    }
    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.inner.delete_entities(ctx, key, request).await
    }
    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        self.inner.get_operation(ctx, operation_id).await
    }
}

#[async_trait]
impl TypesRegistryApi for Recording {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.record(ctx, &request);
        let response = TypesRegistryApi::batch_get_entities(&self.inner, ctx, request).await?;
        Ok(self.answer(response))
    }
    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        TypesRegistryApi::list_entities(&self.inner, ctx, request).await
    }
}

fn uuid(id: &str) -> Uuid {
    GtsId::try_new(id).expect("identifier").to_uuid()
}

fn type_id(id: &str) -> GtsTypeId {
    GtsTypeId::try_new(id).expect("a Type Schema identifier")
}

fn instance_id(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("an Instance identifier")
}

fn key(id: &str) -> BatchGetItem {
    BatchGetItem::from(EntityKey::GtsId(GtsId::try_new(id).expect("identifier")))
}

fn assert_kind<T: std::fmt::Debug>(
    result: Result<T, CanonicalError>,
    expected: fn(&CanonicalError) -> bool,
) {
    let error = result.expect_err("does not resolve");
    assert!(expected(&error), "{error:?}");
}

fn not_found(error: &CanonicalError) -> bool {
    matches!(error, CanonicalError::NotFound { .. })
}

fn invalid(error: &CanonicalError) -> bool {
    matches!(error, CanonicalError::InvalidArgument { .. })
}

fn unavailable(error: &CanonicalError) -> bool {
    matches!(error, CanonicalError::ServiceUnavailable { .. })
}

fn internal(error: &CanonicalError) -> bool {
    matches!(error, CanonicalError::Internal { .. })
}

fn types(count: usize) -> Vec<String> {
    (0..count)
        .map(|n| format!("gts.cf.test.resolve.t{n:03}.v1~"))
        .collect()
}

/// A registry whose second batch read fails.
fn failing_on_second_batch() -> Recording {
    let client = Recording::default();
    client.inner.inject(
        Fault::on(Call::BatchGet)
            .nth(2)
            .fail(CanonicalError::service_unavailable().create()),
    );
    client
}

macro_rules! plane_tests {
    ($module:ident, $contract:ident, $ext:ident, $context:expr) => {
        mod $module {
            use super::*;

            #[tokio::test]
            async fn active_and_deleted_entities_resolve_both_ways() {
                let client = Recording::seeded();
                let api: &dyn $contract = &client;
                let ctx = $context;

                for id in [TYPE, DELETED_TYPE] {
                    let resolved = $ext::resolve_type_uuid(api, &ctx, &type_id(id))
                        .await
                        .expect("registered");
                    assert_eq!(resolved, uuid(id));
                    let back = $ext::resolve_type_id(api, &ctx, resolved)
                        .await
                        .expect("registered");
                    assert_eq!(back, type_id(id));
                }
                for id in [INSTANCE, DELETED_INSTANCE] {
                    let resolved = $ext::resolve_instance_uuid(api, &ctx, &instance_id(id))
                        .await
                        .expect("registered");
                    assert_eq!(resolved, uuid(id));
                    let back = $ext::resolve_instance_id(api, &ctx, resolved)
                        .await
                        .expect("registered");
                    assert_eq!(back, instance_id(id), "the Instance's own identifier");
                }

                let ids = [type_id(TYPE), type_id(DELETED_TYPE)];
                let uuids = $ext::resolve_type_uuids(api, &ctx, &ids)
                    .await
                    .expect("read");
                assert_eq!(
                    uuids,
                    HashMap::from([
                        (type_id(TYPE), Some(uuid(TYPE))),
                        (type_id(DELETED_TYPE), Some(uuid(DELETED_TYPE))),
                    ])
                );
                let back = $ext::resolve_type_ids(api, &ctx, &[uuid(TYPE), uuid(DELETED_TYPE)])
                    .await
                    .expect("read");
                assert_eq!(
                    back,
                    HashMap::from([
                        (uuid(TYPE), Some(type_id(TYPE))),
                        (uuid(DELETED_TYPE), Some(type_id(DELETED_TYPE))),
                    ])
                );
                let ids = [instance_id(INSTANCE), instance_id(DELETED_INSTANCE)];
                let uuids = $ext::resolve_instance_uuids(api, &ctx, &ids)
                    .await
                    .expect("read");
                assert_eq!(
                    uuids,
                    HashMap::from([
                        (instance_id(INSTANCE), Some(uuid(INSTANCE))),
                        (instance_id(DELETED_INSTANCE), Some(uuid(DELETED_INSTANCE))),
                    ])
                );
                let back = $ext::resolve_instance_ids(
                    api,
                    &ctx,
                    &[uuid(INSTANCE), uuid(DELETED_INSTANCE)],
                )
                .await
                .expect("read");
                assert_eq!(
                    back,
                    HashMap::from([
                        (uuid(INSTANCE), Some(instance_id(INSTANCE))),
                        (uuid(DELETED_INSTANCE), Some(instance_id(DELETED_INSTANCE))),
                    ])
                );
                client.assert_id_only_reads_under(&ctx);
            }

            #[tokio::test]
            async fn an_unregistered_identifier_is_read_and_does_not_resolve() {
                let client = Recording::seeded();
                let api: &dyn $contract = &client;
                let ctx = $context;

                assert_kind(
                    $ext::resolve_type_uuid(api, &ctx, &type_id(UNREGISTERED_TYPE)).await,
                    not_found,
                );
                assert_kind(
                    $ext::resolve_instance_uuid(api, &ctx, &instance_id(UNREGISTERED_INSTANCE))
                        .await,
                    not_found,
                );
                let types = $ext::resolve_type_uuids(api, &ctx, &[type_id(UNREGISTERED_TYPE)])
                    .await
                    .expect("read");
                assert_eq!(types, HashMap::from([(type_id(UNREGISTERED_TYPE), None)]));
                let instances =
                    $ext::resolve_instance_uuids(api, &ctx, &[instance_id(UNREGISTERED_INSTANCE)])
                        .await
                        .expect("read");
                assert_eq!(
                    instances,
                    HashMap::from([(instance_id(UNREGISTERED_INSTANCE), None)])
                );

                let asked: Vec<Vec<BatchGetItem>> =
                    client.reads().into_iter().map(|read| read.items).collect();
                assert_eq!(
                    asked,
                    vec![
                        vec![key(UNREGISTERED_TYPE)],
                        vec![key(UNREGISTERED_INSTANCE)],
                        vec![key(UNREGISTERED_TYPE)],
                        vec![key(UNREGISTERED_INSTANCE)],
                    ],
                    "each lookup asks the registry"
                );
                client.assert_id_only_reads_under(&ctx);
            }

            #[tokio::test]
            async fn forward_helpers_answer_the_reference_the_registry_read() {
                let client = Recording::seeded();
                let answered = Uuid::from_u128(0xA5);
                *client.answered_uuid.lock() = Some(answered);
                let api: &dyn $contract = &client;
                let ctx = $context;

                let resolved = $ext::resolve_type_uuid(api, &ctx, &type_id(TYPE))
                    .await
                    .expect("registered");
                assert_eq!(resolved, answered);
                let resolved = $ext::resolve_instance_uuid(api, &ctx, &instance_id(INSTANCE))
                    .await
                    .expect("registered");
                assert_eq!(resolved, answered);
                let resolved = $ext::resolve_type_uuids(api, &ctx, &[type_id(TYPE)])
                    .await
                    .expect("read");
                assert_eq!(resolved[&type_id(TYPE)], Some(answered));
                let resolved = $ext::resolve_instance_uuids(api, &ctx, &[instance_id(INSTANCE)])
                    .await
                    .expect("read");
                assert_eq!(resolved[&instance_id(INSTANCE)], Some(answered));
            }

            #[tokio::test]
            async fn an_unknown_or_other_kind_reference_does_not_resolve() {
                let client = Recording::seeded();
                let api: &dyn $contract = &client;
                let ctx = $context;
                let unknown = Uuid::from_u128(0xF00D);

                for asked in [unknown, uuid(INSTANCE)] {
                    assert_kind($ext::resolve_type_id(api, &ctx, asked).await, not_found);
                }
                for asked in [unknown, uuid(TYPE)] {
                    assert_kind($ext::resolve_instance_id(api, &ctx, asked).await, not_found);
                }
                let types = $ext::resolve_type_ids(api, &ctx, &[unknown, uuid(INSTANCE)])
                    .await
                    .expect("read");
                assert_eq!(
                    types,
                    HashMap::from([(unknown, None), (uuid(INSTANCE), None)])
                );
                let instances = $ext::resolve_instance_ids(api, &ctx, &[unknown, uuid(TYPE)])
                    .await
                    .expect("read");
                assert_eq!(
                    instances,
                    HashMap::from([(unknown, None), (uuid(TYPE), None)])
                );
                client.assert_id_only_reads_under(&ctx);
            }

            #[tokio::test]
            async fn an_invalid_identifier_is_refused_before_any_read() {
                let client = Recording::seeded();
                let api: &dyn $contract = &client;
                let ctx = $context;
                let malformed_type = GtsTypeId::new("gts.not a valid id~");
                let instance_as_type = GtsTypeId::new(INSTANCE);
                let malformed_instance = GtsInstanceId::new("gts.not a valid id~", "x");
                let type_as_instance = GtsInstanceId::new(TYPE, "");

                for id in [&malformed_type, &instance_as_type] {
                    assert_kind($ext::resolve_type_uuid(api, &ctx, id).await, invalid);
                    let batch = [type_id(TYPE), id.clone()];
                    assert_kind($ext::resolve_type_uuids(api, &ctx, &batch).await, invalid);
                }
                for id in [&malformed_instance, &type_as_instance] {
                    assert_kind($ext::resolve_instance_uuid(api, &ctx, id).await, invalid);
                    let batch = [instance_id(INSTANCE), id.clone()];
                    assert_kind(
                        $ext::resolve_instance_uuids(api, &ctx, &batch).await,
                        invalid,
                    );
                }
                assert!(client.reads().is_empty(), "nothing is read");
            }

            #[tokio::test]
            async fn many_keys_are_read_in_bounded_deduplicated_batches() {
                let client = Recording::default();
                let ids = types(MAX_BATCH_GET_KEYS + 1);
                for id in &ids {
                    client.inner.seed(id, json!({}));
                }
                let api: &dyn $contract = &client;
                let ctx = $context;
                // Every key twice: duplicates are read once.
                let asked_ids: Vec<GtsTypeId> =
                    ids.iter().chain(&ids).map(|id| type_id(id)).collect();
                let asked_uuids: Vec<Uuid> = ids.iter().chain(&ids).map(|id| uuid(id)).collect();

                let uuids = $ext::resolve_type_uuids(api, &ctx, &asked_ids)
                    .await
                    .expect("read");
                let back = $ext::resolve_type_ids(api, &ctx, &asked_uuids)
                    .await
                    .expect("read");

                assert_eq!(uuids.len(), ids.len());
                assert_eq!(back.len(), ids.len());
                for id in &ids {
                    assert_eq!(uuids[&type_id(id)], Some(uuid(id)));
                    assert_eq!(back[&uuid(id)], Some(type_id(id)));
                }
                let sizes: Vec<usize> = client.reads().iter().map(|r| r.items.len()).collect();
                assert_eq!(sizes, vec![MAX_BATCH_GET_KEYS, 1, MAX_BATCH_GET_KEYS, 1]);
                client.assert_id_only_reads_under(&ctx);
            }

            #[tokio::test]
            async fn a_failed_batch_fails_the_call_and_reads_no_further() {
                let ids = types(2 * MAX_BATCH_GET_KEYS + 1);
                let ctx = $context;

                let client = failing_on_second_batch();
                let api: &dyn $contract = &client;
                let asked: Vec<GtsTypeId> = ids.iter().map(|id| type_id(id)).collect();
                assert_kind(
                    $ext::resolve_type_uuids(api, &ctx, &asked).await,
                    unavailable,
                );
                assert_eq!(client.inner.calls(Call::BatchGet), 2);

                let client = failing_on_second_batch();
                let api: &dyn $contract = &client;
                let asked: Vec<Uuid> = ids.iter().map(|id| uuid(id)).collect();
                assert_kind(
                    $ext::resolve_instance_ids(api, &ctx, &asked).await,
                    unavailable,
                );
                assert_eq!(client.inner.calls(Call::BatchGet), 2);
            }

            #[tokio::test]
            async fn a_mislabeled_kind_stays_a_protocol_fault() {
                let client = Recording::seeded();
                client.inner.protocol_fault(ProtocolFault::MislabelKind);
                let api: &dyn $contract = &client;
                let ctx = $context;

                assert_kind(
                    $ext::resolve_type_uuid(api, &ctx, &type_id(TYPE)).await,
                    internal,
                );
                assert_kind(
                    $ext::resolve_type_ids(api, &ctx, &[uuid(TYPE)]).await,
                    internal,
                );
                assert_kind(
                    $ext::resolve_instance_id(api, &ctx, uuid(INSTANCE)).await,
                    internal,
                );
            }

            #[tokio::test]
            async fn an_empty_resolution_needs_no_transport() {
                let client = Recording::default();
                let api: &dyn $contract = &client;
                let ctx = $context;

                assert!(
                    $ext::resolve_type_uuids(api, &ctx, &[])
                        .await
                        .expect("empty")
                        .is_empty()
                );
                assert!(
                    $ext::resolve_type_ids(api, &ctx, &[])
                        .await
                        .expect("empty")
                        .is_empty()
                );
                assert!(
                    $ext::resolve_instance_uuids(api, &ctx, &[])
                        .await
                        .expect("empty")
                        .is_empty()
                );
                assert!(
                    $ext::resolve_instance_ids(api, &ctx, &[])
                        .await
                        .expect("empty")
                        .is_empty()
                );
                assert!(client.reads().is_empty());
            }
        }
    };
}

plane_tests!(
    platform,
    PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt,
    PlatformSecurityContext::outbound_marker()
);
plane_tests!(
    tenant,
    TypesRegistryApi,
    TypesRegistryApiExt,
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x00C0_FFEE))
        .subject_tenant_id(Uuid::from_u128(0x7E4A))
        .bearer_token("bearer-resolve".to_owned())
        .build()
        .expect("a caller context")
);
