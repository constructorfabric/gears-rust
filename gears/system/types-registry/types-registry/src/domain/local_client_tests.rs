//! Unit tests for [`TypesRegistryLocalClient`](super::TypesRegistryLocalClient).
//!
//! Kept in a sibling `_tests.rs` file per the `de1101_tests_in_separate_files`
//! repo lint. Linked into `local_client.rs` via
//! `#[path = "local_client_tests.rs"] mod tests;`, so the module sees
//! `local_client.rs` as `super`.

use super::*;
use crate::infra::InMemoryGtsRepository;
use gts::GtsConfig;
use serde_json::json;
use std::time::Duration;
use toolkit_canonical_errors::InvalidArgument;

const JSON_SCHEMA_DRAFT_07: &str = "https://json-schema.org/draft-07/schema#";

// The client trait now returns `CanonicalError`; the legacy `is_*` predicates
// on the SDK error enum are gone. These helpers assert the canonical category
// the adapter routes each former SDK-error variant to (ADR 0005).
fn is_invalid_gts_id(err: &CanonicalError) -> bool {
    matches!(
        err,
        CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } if field_violations
            .iter()
            .any(|v| v.reason == types_registry_sdk::field::INVALID_GTS_ID)
    )
}

fn is_not_found(err: &CanonicalError) -> bool {
    matches!(err, CanonicalError::NotFound { .. })
}

fn is_parent_not_registered(err: &CanonicalError) -> bool {
    matches!(
        err,
        CanonicalError::FailedPrecondition { ctx, .. }
            if ctx.violations.iter().any(|v| {
                v.type_ == types_registry_sdk::precondition::PARENT_NOT_REGISTERED
            })
    )
}

fn default_config() -> GtsConfig {
    crate::config::TypesRegistryConfig::default().to_gts_config()
}

fn create_client() -> TypesRegistryLocalClient {
    let repo = Arc::new(InMemoryGtsRepository::new(default_config()));
    let service = Arc::new(TypesRegistryService::new(
        repo,
        crate::config::TypesRegistryConfig::default(),
    ));
    TypesRegistryLocalClient::new(service)
}

#[tokio::test]
async fn test_register_and_get_type_schema() {
    let client = create_client();
    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "properties": { "userId": { "type": "string" } }
    });
    let results = client.register(vec![entity]).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());

    client.service.switch_to_ready().unwrap();

    let retrieved = client
        .get_type_schema("gts.acme.core.events.user_created.v1~")
        .await
        .unwrap();
    assert_eq!(
        retrieved.type_id.as_ref(),
        "gts.acme.core.events.user_created.v1~"
    );
    assert!(retrieved.parent.is_none());
}

#[tokio::test]
async fn test_get_type_schema_resolves_parent_chain() {
    let client = create_client();
    let base_id = "gts.acme.core.events.base.v1~";
    let derived_id = "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~";
    let base = json!({
        "$id": format!("gts://{base_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "properties": { "id": { "type": "string" } }
    });
    let derived = json!({
        "$id": format!("gts://{derived_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "allOf": [
            { "$ref": format!("gts://{base_id}") },
            { "properties": { "name": { "type": "string" } } }
        ]
    });
    client.register(vec![base, derived]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let schema = client.get_type_schema(derived_id).await.unwrap();
    let parent = schema.parent.as_ref().expect("parent must be resolved");
    assert_eq!(parent.type_id.as_ref(), base_id);

    let merged = schema.effective_properties();
    assert!(merged.contains_key("id"));
    assert!(merged.contains_key("name"));
}

#[tokio::test]
async fn test_type_schema_cache_dedups_parents() {
    let client = create_client();
    let base_id = "gts.acme.core.events.base.v1~";
    let d1_id = "gts.acme.core.events.base.v1~acme.core.events.d1.v1.0~";
    let d2_id = "gts.acme.core.events.base.v1~acme.core.events.d2.v1.0~";
    let base = json!({
        "$id": format!("gts://{base_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "properties": { "id": { "type": "string" } }
    });
    let d1 = json!({
        "$id": format!("gts://{d1_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{base_id}") }]
    });
    let d2 = json!({
        "$id": format!("gts://{d2_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{base_id}") }]
    });
    client.register(vec![base, d1, d2]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let s1 = client.get_type_schema(d1_id).await.unwrap();
    let s2 = client.get_type_schema(d2_id).await.unwrap();
    let p1 = s1.parent.as_ref().expect("d1 has a parent");
    let p2 = s2.parent.as_ref().expect("d2 has a parent");
    assert!(Arc::ptr_eq(p1, p2));
}

#[tokio::test]
async fn test_get_instance_carries_type_schema_arc() {
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let instance = json!({
        "id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1",
        "type": "gts.acme.core.events.user.v1~"
    });
    client.register(vec![schema, instance]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let inst = client
        .get_instance("gts.acme.core.events.user.v1~acme.core.instances.u1.v1")
        .await
        .unwrap();
    assert_eq!(inst.type_id().as_ref(), "gts.acme.core.events.user.v1~");
    assert_eq!(
        inst.type_schema.type_id.as_ref(),
        "gts.acme.core.events.user.v1~"
    );
}

#[tokio::test]
async fn test_instance_cache_returns_same_value() {
    // After get_instance, a second get_instance hits the cache and
    // returns an equal value.
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let instance = json!({
        "id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1",
        "type": "gts.acme.core.events.user.v1~"
    });
    client.register(vec![schema, instance]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let id = "gts.acme.core.events.user.v1~acme.core.instances.u1.v1";
    let i1 = client.get_instance(id).await.unwrap();
    let i2 = client.get_instance(id).await.unwrap();
    assert_eq!(i1.id, i2.id);
    // Type-schema Arc is the same instance — proves both went through the
    // shared cache.
    assert!(Arc::ptr_eq(&i1.type_schema, &i2.type_schema));
}

#[tokio::test]
async fn test_clear_caches_drops_type_schema_arcs() {
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    client.register(vec![schema]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let s1 = client
        .get_type_schema("gts.acme.core.events.user.v1~")
        .await
        .unwrap();
    // After the get, the cache must hold the entry — both LRU and the
    // reverse uuid index.
    assert_eq!(client.type_schemas.len(), 1);
    assert!(client.type_schemas.get_by_uuid(s1.type_uuid).is_some());

    client.clear_caches();
    // Cache state must be observably empty before we trigger any rebuild.
    assert_eq!(client.type_schemas.len(), 0);
    assert!(client.type_schemas.get_by_uuid(s1.type_uuid).is_none());

    // Subsequent get rebuilds and repopulates the cache.
    let s2 = client
        .get_type_schema("gts.acme.core.events.user.v1~")
        .await
        .unwrap();
    assert_eq!(s2.type_id, s1.type_id);
    assert_eq!(client.type_schemas.len(), 1);
}

#[tokio::test]
async fn test_invalidate_type_schema_drops_only_one_entry() {
    let client = create_client();
    let s1 = json!({
        "$id": "gts://gts.acme.core.events.a.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let s2 = json!({
        "$id": "gts://gts.acme.core.events.b.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    client.register(vec![s1, s2]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    client
        .get_type_schema("gts.acme.core.events.a.v1~")
        .await
        .unwrap();
    client
        .get_type_schema("gts.acme.core.events.b.v1~")
        .await
        .unwrap();
    assert_eq!(client.type_schemas.len(), 2);

    client.invalidate_type_schema("gts.acme.core.events.a.v1~");
    assert_eq!(client.type_schemas.len(), 1);
}

#[tokio::test]
async fn test_custom_cache_configs_apply() {
    let repo = Arc::new(InMemoryGtsRepository::new(default_config()));
    let service = Arc::new(TypesRegistryService::new(
        repo,
        crate::config::TypesRegistryConfig::default(),
    ));
    let client = TypesRegistryLocalClient::with_cache_configs(
        service,
        CacheConfig::type_schemas()
            .with_capacity(2)
            .with_ttl(Duration::from_millis(50)),
        CacheConfig::instances().with_capacity(8),
    );
    let s = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    client.register(vec![s]).await.unwrap();
    client.service.switch_to_ready().unwrap();
    let _ = client
        .get_type_schema("gts.acme.core.events.user.v1~")
        .await
        .unwrap();
    assert_eq!(client.type_schemas.len(), 1);

    // After TTL elapses, the cached entry must observably go away. We
    // check via a `get` (which triggers TTL eviction as a side effect)
    // that returns None, then check len. Asserting only `len() == 1`
    // post-rebuild would be a false positive — len stays 1 whether the
    // entry expired and was rebuilt or never expired at all.
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        client
            .type_schemas
            .get("gts.acme.core.events.user.v1~")
            .is_none(),
        "TTL did not evict expired entry"
    );
    assert_eq!(client.type_schemas.len(), 0);

    // Subsequent get rebuilds and repopulates the cache.
    let _ = client
        .get_type_schema("gts.acme.core.events.user.v1~")
        .await
        .unwrap();
    assert_eq!(client.type_schemas.len(), 1);
}

#[tokio::test]
async fn test_get_type_schema_rejects_instance() {
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let instance = json!({
        "id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1",
        "type": "gts.acme.core.events.user.v1~"
    });
    client.register(vec![schema, instance]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let err = client
        .get_type_schema("gts.acme.core.events.user.v1~acme.core.instances.u1.v1")
        .await
        .unwrap_err();
    assert!(is_invalid_gts_id(&err));
}

#[tokio::test]
async fn test_register_type_schemas_rejects_instance_input() {
    let client = create_client();
    let instance = json!({
        "id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1",
        "type": "gts.acme.core.events.user.v1~"
    });
    let results = client.register_type_schemas(vec![instance]).await.unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        RegisterResult::Err { error, .. } => assert!(is_invalid_gts_id(error)),
        RegisterResult::Ok { .. } => panic!("expected Err for instance input"),
    }
}

#[tokio::test]
async fn test_register_type_schemas_unsorted_batch_succeeds() {
    // Batch order [derived, base] is reordered by sort to [base, derived]
    // so the parent registers before its child within a single call.
    let client = create_client();
    let base_id = "gts.acme.core.events.base.v1~";
    let derived_id = "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~";
    let base = json!({
        "$id": format!("gts://{base_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
    });
    let derived = json!({
        "$id": format!("gts://{derived_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{base_id}") }]
    });
    // Pass derived first; sort should put base first.
    let results = client
        .register_type_schemas(vec![derived, base])
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(RegisterResult::is_ok));
}

#[tokio::test]
async fn test_register_type_schemas_orphan_derived_in_ready_fails() {
    // After switch_to_ready, registering a derived schema whose parent
    // is not in persistent storage must fail with
    // ParentTypeSchemaNotRegistered (no persist) — config-phase
    // permissiveness is gone.
    let client = create_client();
    client.service.switch_to_ready().unwrap();

    let derived_id = "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~";
    let derived = json!({
        "$id": format!("gts://{derived_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
    });
    let results = client.register_type_schemas(vec![derived]).await.unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        RegisterResult::Err { error, .. } => {
            assert!(is_parent_not_registered(error));
        }
        RegisterResult::Ok { .. } => panic!("expected Err for orphan derived in ready"),
    }
}

#[tokio::test]
async fn test_register_instances_orphan_in_ready_fails() {
    // Symmetric to the above: instance whose declaring type-schema is
    // not registered must fail with ParentTypeSchemaNotRegistered.
    let client = create_client();
    client.service.switch_to_ready().unwrap();

    let instance = json!({
        "id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1",
        "type": "gts.acme.core.events.user.v1~"
    });
    let results = client.register_instances(vec![instance]).await.unwrap();
    assert_eq!(results.len(), 1);
    match &results[0] {
        RegisterResult::Err { error, .. } => {
            assert!(is_parent_not_registered(error));
        }
        RegisterResult::Ok { .. } => panic!("expected Err for orphan instance in ready"),
    }
}

#[tokio::test]
async fn test_list_type_schemas_and_instances_filtered() {
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let instance = json!({
        "id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1",
        "type": "gts.acme.core.events.user.v1~"
    });
    client.register(vec![schema, instance]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let schemas = client
        .list_type_schemas(TypeSchemaQuery::default())
        .await
        .unwrap();
    assert_eq!(schemas.len(), 1);
    assert!(schemas[0].type_id.as_ref().ends_with('~'));

    let instances = client
        .list_instances(InstanceQuery::default())
        .await
        .unwrap();
    assert_eq!(instances.len(), 1);
    assert!(!instances[0].id.as_ref().ends_with('~'));
}

#[tokio::test]
async fn test_get_type_schema_by_uuid() {
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    client.register(vec![schema]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    let listed = client
        .list_type_schemas(TypeSchemaQuery::default())
        .await
        .unwrap();
    let uuid = listed[0].type_uuid;
    let by_uuid = client.get_type_schema_by_uuid(uuid).await.unwrap();
    assert_eq!(by_uuid.type_id.as_ref(), "gts.acme.core.events.user.v1~");

    let unknown = client
        .get_type_schema_by_uuid(Uuid::nil())
        .await
        .unwrap_err();
    assert!(is_not_found(&unknown));
}

#[tokio::test]
async fn test_uuid_index_populated_by_get() {
    // After a successful get_type_schema(gts_id), the UUID→gts_id mapping
    // must be in the index so that a subsequent get_type_schema_by_uuid
    // can take the fast path.
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    client.register(vec![schema]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    // First call by gts_id — populates type_schema cache, which writes the
    // uuid → gts_id mapping into the cache's internal reverse index.
    let by_id = client
        .get_type_schema("gts.acme.core.events.user.v1~")
        .await
        .unwrap();
    assert!(client.type_schemas.get_by_uuid(by_id.type_uuid).is_some());

    // Second call by UUID hits the fast path and returns the same value.
    let by_uuid = client
        .get_type_schema_by_uuid(by_id.type_uuid)
        .await
        .unwrap();
    assert_eq!(by_uuid.type_id, by_id.type_id);
}

#[tokio::test]
async fn test_invalidate_type_schema_cascades_to_dependents() {
    // Re-registering a base type-schema must drop cached derived
    // schemas whose chain references the base. Cache walks the
    // resolved Arc chain via `ancestors()`.
    let client = create_client();
    let base_id = "gts.acme.core.events.base.v1~";
    let derived_id = "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~";
    let base = json!({
        "$id": format!("gts://{base_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
    });
    let derived = json!({
        "$id": format!("gts://{derived_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "allOf": [
            { "$ref": format!("gts://{base_id}") },
            { "properties": { "extra": { "type": "string" } } }
        ],
    });
    client.register(vec![base, derived]).await.unwrap();
    client.service.switch_to_ready().unwrap();

    // Warm caches: derived resolution pulls base into the cache too.
    let _ = client.get_type_schema(derived_id).await.unwrap();
    assert_eq!(client.type_schemas.len(), 2); // base + derived

    // Cascade-invalidate base — derived references base in its chain,
    // so both cache entries must drop.
    client.invalidate_type_schema(base_id);
    assert_eq!(client.type_schemas.len(), 0);
}

#[tokio::test]
async fn test_clear_caches_resets_uuid_index() {
    let client = create_client();
    let schema = json!({
        "$id": "gts://gts.acme.core.events.user.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    client.register(vec![schema]).await.unwrap();
    client.service.switch_to_ready().unwrap();
    let warmed = client
        .get_type_schema("gts.acme.core.events.user.v1~")
        .await
        .unwrap();
    assert!(client.type_schemas.get_by_uuid(warmed.type_uuid).is_some());

    client.clear_caches();
    assert!(client.type_schemas.get_by_uuid(warmed.type_uuid).is_none());
}

// ── Batch get_*  tests ───────────────────────────────────────────────

#[tokio::test]
async fn test_get_type_schemas_returns_keyed_map() {
    let client = create_client();
    client
        .register(vec![
            json!({"$id": "gts://gts.acme.core.events.alpha.v1~", "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
            json!({"$id": "gts://gts.acme.core.events.beta.v1~",  "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
            json!({"$id": "gts://gts.acme.core.events.gamma.v1~", "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
        ])
        .await
        .unwrap();
    client.service.switch_to_ready().unwrap();

    let ids = vec![
        "gts.acme.core.events.gamma.v1~".to_owned(),
        "gts.acme.core.events.alpha.v1~".to_owned(),
        "gts.acme.core.events.beta.v1~".to_owned(),
    ];
    let results = client.get_type_schemas(ids.clone()).await;
    assert_eq!(results.len(), 3);
    for id in &ids {
        let got = results
            .get(id)
            .expect("present")
            .as_ref()
            .expect("ok")
            .type_id
            .as_ref();
        assert_eq!(got, id);
    }
}

#[tokio::test]
async fn test_get_type_schemas_partial_failures() {
    let client = create_client();
    client
        .register(vec![
            json!({"$id": "gts://gts.acme.core.events.alpha.v1~", "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
            json!({"$id": "gts://gts.acme.core.events.gamma.v1~", "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
        ])
        .await
        .unwrap();
    client.service.switch_to_ready().unwrap();

    let alpha = "gts.acme.core.events.alpha.v1~";
    let missing = "gts.acme.core.events.missing.v1~";
    let gamma = "gts.acme.core.events.gamma.v1~";
    let results = client
        .get_type_schemas(vec![alpha.to_owned(), missing.to_owned(), gamma.to_owned()])
        .await;
    assert!(results.get(alpha).expect("present").is_ok());
    assert!(is_not_found(
        results
            .get(missing)
            .expect("present")
            .as_ref()
            .err()
            .unwrap()
    ));
    assert!(results.get(gamma).expect("present").is_ok());
}

#[tokio::test]
async fn test_get_type_schemas_kind_mismatch() {
    // An instance-shaped id (no trailing `~`) passed to get_type_schemas
    // must surface as an invalid-GTS-id validation error for that single
    // item, not fail the whole batch.
    let client = create_client();
    let bad = "gts.acme.core.events.user.v1~acme.core.instances.u1.v1";
    let results = client.get_type_schemas(vec![bad.to_owned()]).await;
    assert_eq!(results.len(), 1);
    assert!(is_invalid_gts_id(
        results.get(bad).expect("present").as_ref().err().unwrap()
    ));
}

#[tokio::test]
async fn test_get_type_schemas_by_uuid_warm_cache_uses_index() {
    // Warm the uuid_index via get_type_schema(id), then batch-by-uuid
    // must succeed without storage scan (index already has the mapping).
    let client = create_client();
    client
        .register(vec![
            json!({"$id": "gts://gts.acme.core.events.alpha.v1~", "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
        ])
        .await
        .unwrap();
    client.service.switch_to_ready().unwrap();
    let warmed = client
        .get_type_schema("gts.acme.core.events.alpha.v1~")
        .await
        .unwrap();
    assert!(client.type_schemas.get_by_uuid(warmed.type_uuid).is_some());

    let results = client
        .get_type_schemas_by_uuid(vec![warmed.type_uuid])
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(
        results
            .get(&warmed.type_uuid)
            .expect("present")
            .as_ref()
            .expect("ok")
            .type_id,
        warmed.type_id,
    );
}

#[tokio::test]
async fn test_get_type_schemas_by_uuid_cold_falls_back_to_storage() {
    let client = create_client();
    client
        .register(vec![
            json!({"$id": "gts://gts.acme.core.events.alpha.v1~", "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
        ])
        .await
        .unwrap();
    client.service.switch_to_ready().unwrap();

    // Find the uuid via list_*; then clear_caches to drop the type-schema
    // cache (and its uuid index).
    let listed = client
        .list_type_schemas(TypeSchemaQuery::default())
        .await
        .unwrap();
    let uuid = listed[0].type_uuid;
    client.clear_caches();
    assert!(client.type_schemas.get_by_uuid(uuid).is_none());

    let results = client.get_type_schemas_by_uuid(vec![uuid]).await;
    assert_eq!(results.len(), 1);
    assert!(results.get(&uuid).expect("present").is_ok());
    // After cold lookup, the cache's uuid index is repopulated.
    assert!(client.type_schemas.get_by_uuid(uuid).is_some());
}

#[tokio::test]
async fn test_get_instances_keyed_with_partial_failures() {
    let client = create_client();
    client
        .register(vec![
            json!({"$id": "gts://gts.acme.core.events.user.v1~",            "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
            json!({"id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1"}),
            json!({"id": "gts.acme.core.events.user.v1~acme.core.instances.u2.v1"}),
        ])
        .await
        .unwrap();
    client.service.switch_to_ready().unwrap();

    let u2 = "gts.acme.core.events.user.v1~acme.core.instances.u2.v1";
    let missing = "gts.acme.core.events.user.v1~acme.core.instances.missing.v1";
    let u1 = "gts.acme.core.events.user.v1~acme.core.instances.u1.v1";
    let results = client
        .get_instances(vec![u2.to_owned(), missing.to_owned(), u1.to_owned()])
        .await;
    assert_eq!(results.len(), 3);
    assert_eq!(
        results.get(u2).expect("present").as_ref().expect("ok").id,
        u2
    );
    assert!(is_not_found(
        results
            .get(missing)
            .expect("present")
            .as_ref()
            .err()
            .unwrap()
    ));
    assert_eq!(
        results.get(u1).expect("present").as_ref().expect("ok").id,
        u1
    );
}

#[tokio::test]
async fn test_get_instances_by_uuid_warm_cache_uses_index() {
    let client = create_client();
    client
        .register(vec![
            json!({"$id": "gts://gts.acme.core.events.user.v1~", "$schema": JSON_SCHEMA_DRAFT_07, "type": "object"}),
            json!({"id": "gts.acme.core.events.user.v1~acme.core.instances.u1.v1"}),
        ])
        .await
        .unwrap();
    client.service.switch_to_ready().unwrap();
    let warmed = client
        .get_instance("gts.acme.core.events.user.v1~acme.core.instances.u1.v1")
        .await
        .unwrap();
    assert!(client.instances.get_by_uuid(warmed.uuid).is_some());

    let results = client.get_instances_by_uuid(vec![warmed.uuid]).await;
    assert_eq!(results.len(), 1);
    assert_eq!(
        results
            .get(&warmed.uuid)
            .expect("present")
            .as_ref()
            .expect("ok")
            .id,
        warmed.id
    );
}

#[tokio::test]
async fn test_get_batch_empty_input() {
    let client = create_client();
    client.service.switch_to_ready().unwrap();

    assert!(client.get_type_schemas(vec![]).await.is_empty());
    assert!(client.get_type_schemas_by_uuid(vec![]).await.is_empty());
    assert!(client.get_instances(vec![]).await.is_empty());
    assert!(client.get_instances_by_uuid(vec![]).await.is_empty());
}

/// Caller order must survive the parent-before-child sort that
/// `register*` does internally. We pass `[child, parent]` (child sorts
/// later by id) and require `results[0]` to correspond to the child and
/// `results[1]` to the parent, matching the input order.
#[tokio::test]
async fn test_register_preserves_caller_order() {
    let client = create_client();
    let base_id = "gts.acme.core.events.base.v1~";
    let derived_id = "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~";
    let base = json!({
        "$id": format!("gts://{base_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let derived = json!({
        "$id": format!("gts://{derived_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{base_id}") }]
    });

    // Caller passes child first, parent second. Internal sort flips this so
    // the parent is registered first, but the returned vec must still align
    // with caller order.
    let results = client
        .register(vec![derived.clone(), base.clone()])
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_result().ok(), Some(derived_id));
    assert_eq!(results[1].as_result().ok(), Some(base_id));
}

/// `register_type_schemas` / `register_instances` share the same sort
/// trick — same regression risk. Verify both preserve caller order.
#[tokio::test]
async fn test_register_typed_preserves_caller_order() {
    let client = create_client();
    let base_id = "gts.acme.core.events.base.v1~";
    let derived_id = "gts.acme.core.events.base.v1~acme.core.events.derived.v1.0~";
    let base = json!({
        "$id": format!("gts://{base_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let derived = json!({
        "$id": format!("gts://{derived_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{base_id}") }]
    });

    let schema_results = client
        .register_type_schemas(vec![derived.clone(), base.clone()])
        .await
        .unwrap();
    assert_eq!(schema_results.len(), 2);
    assert_eq!(schema_results[0].as_result().ok(), Some(derived_id));
    assert_eq!(schema_results[1].as_result().ok(), Some(base_id));

    // For instances, register a parent schema first, then verify caller order
    // for two instances of it.
    let parent_id = "gts.acme.core.events.parent.v1~";
    let earlier = "gts.acme.core.events.parent.v1~acme.core.evt.a.v1";
    let later = "gts.acme.core.events.parent.v1~acme.core.evt.b.v1";
    client
        .register(vec![json!({
            "$id": format!("gts://{parent_id}"),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        })])
        .await
        .unwrap();
    // Pass `later` before `earlier`; `later` sorts after `earlier` by id, so
    // the internal sort would otherwise flip them.
    let instance_results = client
        .register_instances(vec![json!({ "$id": later }), json!({ "$id": earlier })])
        .await
        .unwrap();
    assert_eq!(instance_results.len(), 2);
    assert_eq!(instance_results[0].as_result().ok(), Some(later));
    assert_eq!(instance_results[1].as_result().ok(), Some(earlier));
}
