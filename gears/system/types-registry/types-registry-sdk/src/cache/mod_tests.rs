use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use super::{CacheSettings, CachedPlatformTypesRegistry};
use crate::contract::PlatformTypesRegistryApi;
use crate::item_failure::AdmissionFailureReason;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, DeleteEntitiesRequest,
    DeletionOperation, EntityField, EntityIdentity, EntityKey, EntityLookup, FieldSelection,
    IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse, Operation, OperationStatus,
    Projection, PublisherContext, RegisterEntitiesRequest, RegisterItem, RegistrationOperation,
    Validator,
};
use crate::models::{DeleteItem, EntityKind, Provenance, RegistrationOutcome};
use crate::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

const TYPE: &str = "gts.cf.cache.client.entity.v1~";
const OTHER: &str = "gts.cf.cache.client.other.v1~";
const THIRD: &str = "gts.cf.cache.client.third.v1~";
const UNREGISTERED: &str = "gts.cf.cache.client.unregistered.v1~";
const WINDOW: Duration = Duration::from_secs(30);

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn gts_key(id: &str) -> EntityKey {
    EntityKey::GtsId(gts::GtsId::try_new(id).expect("valid GTS identifier"))
}

fn uuid_key(id: &str) -> EntityKey {
    EntityKey::GtsUuid(
        gts::GtsId::try_new(id)
            .expect("valid GTS identifier")
            .to_uuid(),
    )
}

fn schema(id: &str, title: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "title": title,
    })
}

/// A read of `key` conditional on `etag`, held for every field.
fn conditioned(key: EntityKey, etag: Validator) -> BatchGetItem {
    BatchGetItem::for_cache_read(key, Some(etag), &FieldSelection::full())
}

fn full() -> Projection {
    Projection::Select(FieldSelection::full())
}

fn content_only() -> Projection {
    Projection::Select(FieldSelection::with(&[EntityField::Content]))
}

/// A registry change the client does not observe: a new revision stored directly.
fn change(registry: &MockTypesRegistry, id: &str, title: &str, version: u64) {
    registry.seed_entity(Seed::new(id, schema(id, title)).version(version));
}

/// What the registry answered, per key: whether a snapshot crossed the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Found,
    Unchanged,
    NotFound,
}

/// The registry behind the cache, recording each batch read and its answers.
type Batch = Vec<(BatchGetItem, Answer)>;

/// Rewrites the next registry answer before the cache sees it.
type Tamper = Box<dyn FnOnce(&mut BatchGetEntitiesResponse) + Send>;

/// Rewrites the next registry resolution before the cache sees it.
type ResolutionTamper = Box<dyn FnOnce(&mut Resolutions) + Send>;

type Resolutions = std::collections::HashMap<EntityKey, Option<EntityIdentity>>;

/// Rewrites the next discovery page before the cache sees it.
type ListTamper = Box<dyn FnOnce(&mut ListEntitiesResponse) + Send>;

struct Recorder {
    registry: Arc<MockTypesRegistry>,
    batches: Mutex<Vec<Batch>>,
    tamper: Mutex<Option<Tamper>>,
    /// The keys of each batch resolution sent, in order.
    resolutions: Mutex<Vec<Vec<EntityKey>>>,
    resolution_tamper: Mutex<Option<ResolutionTamper>>,
    list_tamper: Mutex<Option<ListTamper>>,
    /// Holds the next batch read's or resolution's answer, already read from the
    /// registry, until released.
    gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl Recorder {
    fn batches(&self) -> Vec<Batch> {
        self.batches.lock().expect("lock").clone()
    }

    fn batch_count(&self) -> usize {
        self.batches.lock().expect("lock").len()
    }

    fn resolutions(&self) -> Vec<Vec<EntityKey>> {
        self.resolutions.lock().expect("lock").clone()
    }

    fn resolution_count(&self) -> usize {
        self.resolutions.lock().expect("lock").len()
    }

    fn tamper_next_resolution(&self, tamper: impl FnOnce(&mut Resolutions) + Send + 'static) {
        *self.resolution_tamper.lock().expect("lock") = Some(Box::new(tamper));
    }

    /// The next batch read reads the registry, then waits for the returned sender.
    fn hold_next(&self) -> tokio::sync::oneshot::Sender<()> {
        let (release, gate) = tokio::sync::oneshot::channel();
        *self.gate.lock().expect("lock") = Some(gate);
        release
    }

    fn tamper_next(&self, tamper: impl FnOnce(&mut BatchGetEntitiesResponse) + Send + 'static) {
        *self.tamper.lock().expect("lock") = Some(Box::new(tamper));
    }

    fn tamper_next_page(&self, tamper: impl FnOnce(&mut ListEntitiesResponse) + Send + 'static) {
        *self.list_tamper.lock().expect("lock") = Some(Box::new(tamper));
    }
}

#[async_trait]
impl PlatformTypesRegistryApi for Recorder {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let items = request.items.clone();
        let mut response = self.registry.batch_get_entities(ctx, request).await?;
        if let Some(tamper) = self.tamper.lock().expect("lock").take() {
            tamper(&mut response);
        }
        let gate = self.gate.lock().expect("lock").take();
        if let Some(gate) = gate {
            gate.await.expect("released");
        }
        let answered = items
            .into_iter()
            .map(|item| {
                let answer = match response.0.get(&item.key) {
                    Some(EntityLookup::Found { .. }) => Answer::Found,
                    Some(EntityLookup::Unchanged { .. }) => Answer::Unchanged,
                    _ => Answer::NotFound,
                };
                (item, answer)
            })
            .collect();
        self.batches.lock().expect("lock").push(answered);
        Ok(response)
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let mut page = self.registry.list_entities(ctx, request).await?;
        if let Some(tamper) = self.list_tamper.lock().expect("lock").take() {
            tamper(&mut page);
        }
        Ok(page)
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        self.registry.register_entities(ctx, key, request).await
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.registry.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        self.registry.get_operation(ctx, operation_id).await
    }

    async fn batch_resolve_identities(
        &self,
        ctx: &PlatformSecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<Resolutions, CanonicalError> {
        self.resolutions.lock().expect("lock").push(keys.clone());
        let mut response = self.registry.batch_resolve_identities(ctx, keys).await?;
        if let Some(tamper) = self.resolution_tamper.lock().expect("lock").take() {
            tamper(&mut response);
        }
        let gate = self.gate.lock().expect("lock").take();
        if let Some(gate) = gate {
            gate.await.expect("released");
        }
        Ok(response)
    }
}

struct Fixture {
    registry: Arc<MockTypesRegistry>,
    recorder: Arc<Recorder>,
    cache: CachedPlatformTypesRegistry,
}

/// Operations complete on submission: the submit response itself is terminal.
fn fixture(window: Duration) -> Fixture {
    fixture_over(MockTypesRegistry::new().completing_after(0), window)
}

fn fixture_over(registry: MockTypesRegistry, window: Duration) -> Fixture {
    fixture_with(
        registry,
        CacheSettings {
            freshness_window: window,
            ..CacheSettings::default()
        },
    )
}

fn fixture_with(registry: MockTypesRegistry, settings: CacheSettings) -> Fixture {
    let registry = Arc::new(registry);
    for id in [TYPE, OTHER, THIRD] {
        registry.seed(id, schema(id, "v1"));
    }
    let recorder = Arc::new(Recorder {
        registry: Arc::clone(&registry),
        batches: Mutex::new(Vec::new()),
        tamper: Mutex::new(None),
        resolutions: Mutex::new(Vec::new()),
        resolution_tamper: Mutex::new(None),
        list_tamper: Mutex::new(None),
        gate: Mutex::new(None),
    });
    let cache = CachedPlatformTypesRegistry::new(
        Arc::clone(&recorder) as Arc<dyn PlatformTypesRegistryApi>,
        settings,
    );
    Fixture {
        registry,
        recorder,
        cache,
    }
}

impl Fixture {
    async fn read_with(
        &self,
        keys: Vec<BatchGetItem>,
        projection: Projection,
        fresh: bool,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let request = BatchGetEntitiesRequest {
            items: keys,
            projection,
            fresh,
        };
        self.cache.batch_get_entities(&ctx(), request).await
    }

    async fn lookup(&self, key: EntityKey, projection: Projection, fresh: bool) -> EntityLookup {
        let mut response = self
            .read_with(vec![key.clone().into()], projection, fresh)
            .await
            .expect("read");
        response.0.remove(&key).expect("answered")
    }

    /// The title of `key`'s content under `projection`.
    async fn title(&self, key: EntityKey, projection: Projection, fresh: bool) -> String {
        match self.lookup(key, projection, fresh).await {
            EntityLookup::Found { entity, .. } => entity
                .content
                .as_ref()
                .and_then(|c| c["title"].as_str())
                .expect("content selected")
                .to_owned(),
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn inside_the_window_a_read_is_cached_and_fresh_bypasses_it() {
    let f = fixture(WINDOW);
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v1");
    change(&f.registry, TYPE, "v2", 2);

    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v1",
        "inside the window the unobserved change is not seen"
    );
    assert_eq!(f.recorder.batch_count(), 1, "served without the registry");
    assert_eq!(f.title(gts_key(TYPE), full(), true).await, "v2");
    let fresh = &f.recorder.batches()[1];
    assert!(
        fresh[0].0.if_none_match_for(&full()).is_some(),
        "fresh revalidates against the entry's validator"
    );
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "the fresh answer replaced the entry"
    );
    assert_eq!(f.recorder.batch_count(), 2);
}

#[tokio::test(start_paused = true)]
async fn after_the_window_a_changed_entity_is_read_anew() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    change(&f.registry, TYPE, "v2", 2);
    tokio::time::advance(WINDOW).await;
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
}

#[tokio::test(start_paused = true)]
async fn a_zero_window_never_serves_a_cached_entry_and_moves_no_unchanged_document() {
    let f = fixture(Duration::ZERO);
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(TYPE), full(), false).await;
    let batches = f.recorder.batches();
    assert_eq!(batches.len(), 2, "every read asks the registry");
    assert_eq!(
        batches[1][0].1,
        Answer::Unchanged,
        "no document transferred"
    );

    change(&f.registry, TYPE, "v2", 2);
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
}

#[tokio::test(start_paused = true)]
async fn an_expired_unchanged_entry_is_kept_and_its_window_restarts() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    tokio::time::advance(WINDOW + Duration::from_secs(1)).await;

    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v1");
    let revalidation = &f.recorder.batches()[1];
    assert!(revalidation[0].0.if_none_match_for(&full()).is_some());
    assert_eq!(
        revalidation[0].1,
        Answer::Unchanged,
        "kept, not refetched: the registry sent no snapshot"
    );

    tokio::time::advance(WINDOW.saturating_sub(Duration::from_secs(1))).await;
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the confirmation restarted the window"
    );
}

#[tokio::test(start_paused = true)]
async fn one_conditional_batch_carries_every_expired_key_and_only_those_validators() {
    let f = fixture(WINDOW);
    f.read_with(
        vec![gts_key(TYPE).into(), gts_key(OTHER).into()],
        full(),
        false,
    )
    .await
    .expect("warm");
    tokio::time::advance(WINDOW).await;
    // THIRD is warmed after expiry, so it is still fresh below.
    f.lookup(gts_key(THIRD), full(), false).await;

    let missing = "gts.cf.cache.client.missing.v1~";
    let response = f
        .read_with(
            vec![
                gts_key(TYPE).into(),
                gts_key(OTHER).into(),
                gts_key(THIRD).into(),
                gts_key(missing).into(),
            ],
            full(),
            false,
        )
        .await
        .expect("read");

    let batches = f.recorder.batches();
    assert_eq!(batches.len(), 3, "one registry call for the whole batch");
    let sent = &batches[2];
    let conditional: Vec<_> = sent
        .iter()
        .filter(|(item, _)| item.if_none_match_for(&full()).is_some())
        .map(|(item, _)| item.key.clone())
        .collect();
    assert_eq!(conditional, vec![gts_key(TYPE), gts_key(OTHER)]);
    assert!(
        sent.iter().all(|(item, _)| item.key != gts_key(THIRD)),
        "a fresh entry is answered locally"
    );
    assert!(
        sent.iter()
            .any(|(item, _)| item.key == gts_key(missing)
                && item.if_none_match_for(&full()).is_none()),
        "an uncached key is read unconditionally"
    );
    assert_eq!(response.0.len(), 4);
    assert_eq!(response.0[&gts_key(missing)], EntityLookup::NotFound);
}

#[tokio::test(start_paused = true)]
async fn a_failed_revalidation_propagates_and_never_extends_the_window() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    tokio::time::advance(WINDOW).await;
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .fail(CanonicalError::service_unavailable().create()),
    );

    let error = f
        .read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("the registry could not answer");
    assert!(
        matches!(error, CanonicalError::ServiceUnavailable { .. }),
        "{error:?}"
    );

    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.registry.calls(Call::BatchGet),
        3,
        "the expired entry was not served after the failure"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_read_leaves_no_entry() {
    let f = fixture(WINDOW);
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .fail(CanonicalError::service_unavailable().create()),
    );
    f.read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("injected");
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.registry.calls(Call::BatchGet),
        2,
        "the second read filled it"
    );
}

#[tokio::test(start_paused = true)]
async fn not_found_is_never_cached() {
    let f = fixture(WINDOW);
    let late = "gts.cf.cache.client.late.v1~";
    assert_eq!(
        f.lookup(gts_key(late), full(), false).await,
        EntityLookup::NotFound
    );
    f.registry.seed(late, schema(late, "v1"));
    assert_eq!(f.title(gts_key(late), full(), false).await, "v1");
}

#[tokio::test(start_paused = true)]
async fn discovery_warms_identities_but_pages_and_snapshots_still_read_the_registry() {
    use crate::PlatformTypesRegistryApiExt;

    let f = fixture(WINDOW);
    let page = f
        .cache
        .list_entities(&ctx(), ListEntitiesRequest::default())
        .await
        .expect("page");
    assert_eq!(page.items.len(), 3);
    for entity in &page.items {
        let id = gts::GtsTypeId::try_new(entity.gts_id.as_ref()).expect("type identifier");
        assert_eq!(
            f.cache
                .resolve_type_uuid(&ctx(), &id)
                .await
                .expect("forward"),
            entity.gts_uuid
        );
        assert_eq!(
            f.cache
                .resolve_type_id(&ctx(), entity.gts_uuid)
                .await
                .expect("reverse"),
            id
        );
    }
    assert_eq!(f.recorder.batch_count(), 0);
    f.cache
        .list_entities(&ctx(), ListEntitiesRequest::default())
        .await
        .expect("page");
    assert_eq!(f.registry.calls(Call::List), 2, "the page is not cached");

    assert_eq!(f.title(gts_key(TYPE), content_only(), false).await, "v1");
    assert_eq!(
        f.recorder.batch_count(),
        1,
        "a page item does not prime exact reads"
    );
}

#[tokio::test]
async fn a_conflicting_pair_rejects_the_whole_discovery_page() {
    let f = fixture(WINDOW);
    let tail = "7a1d2f34-5678-49ab-9012-abcdef123456";
    // These two different identifiers carry the same registry reference in the fake.
    for base in [TYPE, OTHER] {
        f.registry.seed(&format!("{base}{tail}"), json!({}));
    }
    let result = f
        .cache
        .list_entities(&ctx(), ListEntitiesRequest::default())
        .await;
    let error = result.expect_err("two identifiers for one Registry Reference");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
    assert!(
        format!("{error:?}").contains("this batch pairs"),
        "{error:?}"
    );
    assert!(f.cache.identity(&gts_key(TYPE)).is_none());
}

/// An inapplicable field rejects the discovery page before any pair is learned.
#[tokio::test]
async fn a_discovery_item_with_a_field_its_kind_lacks_rejects_the_page() {
    let f = fixture(WINDOW);
    let instance = format!("{TYPE}7a1d2f34-5678-49ab-9012-abcdef123456");
    f.registry.seed(&instance, json!({}));
    f.recorder.tamper_next_page(move |page| {
        for item in &mut page.items {
            if item.kind == EntityKind::Instance {
                item.provenance = Some(Provenance {
                    compatibility_check_skipped: false,
                });
            }
        }
    });
    let result = f
        .cache
        .list_entities(&ctx(), ListEntitiesRequest::default())
        .await;
    assert!(
        matches!(result, Err(CanonicalError::Internal { .. })),
        "{result:?}"
    );
    assert!(f.cache.identity(&gts_key(TYPE)).is_none());
}

#[tokio::test]
async fn discovery_checks_both_stores_before_learning_any_pairs() {
    for (max_size_bytes, max_identity_entries, source) in [
        (0, 1, "the identity store pairs"),
        (
            CacheSettings::DEFAULT_MAX_SIZE_BYTES,
            0,
            "a resident snapshot pairs",
        ),
    ] {
        let f = fixture_with(
            MockTypesRegistry::new(),
            CacheSettings {
                max_size_bytes,
                max_identity_entries,
                ..CacheSettings::default()
            },
        );
        let snapshot = snapshot_under(&f.registry, ISSUED).await;
        answer_next(&f, gts_key(TYPE), snapshot);
        f.lookup(gts_key(TYPE), full(), false).await;

        // The ordinary list returns TYPE with the fake's original UUID instead.
        let result = f
            .cache
            .list_entities(&ctx(), ListEntitiesRequest::default())
            .await;
        let error = result.expect_err("TYPE under another Registry Reference");
        assert!(
            matches!(error, CanonicalError::Internal { .. }),
            "{error:?}"
        );
        // Names where the known pair is held, and its counterpart there.
        let error = format!("{error:?}");
        assert!(error.contains(source), "{error}");
        assert!(error.contains(&ISSUED.to_string()), "{error}");
        assert!(f.cache.identity(&gts_key(OTHER)).is_none());
        let known = f
            .cache
            .batch_resolve_identities(&ctx(), vec![gts_key(TYPE)])
            .await
            .expect("known");
        assert_eq!(
            known[&gts_key(TYPE)].as_ref().expect("found").gts_uuid,
            ISSUED
        );
        assert_eq!(f.recorder.batch_count(), 1);
    }
}

#[tokio::test]
async fn discovery_respects_the_identity_entry_limit_and_page_order() {
    for bound in [0, 1] {
        let f = fixture_with(
            MockTypesRegistry::new(),
            CacheSettings {
                max_identity_entries: bound,
                ..CacheSettings::default()
            },
        );
        let page = f
            .cache
            .list_entities(&ctx(), ListEntitiesRequest::new().with_limit(2))
            .await
            .expect("page");
        assert_eq!(page.items.len(), 2);
        assert!(page.next.is_some());
        assert!(
            f.cache
                .identity(&EntityKey::from(page.items[0].gts_uuid))
                .is_none()
        );
        assert_eq!(
            f.cache
                .identity(&EntityKey::from(page.items[1].gts_uuid))
                .is_some(),
            bound == 1
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_operation_resource_is_never_cached() {
    let f = fixture_over(MockTypesRegistry::new().completing_after(2), WINDOW);
    let new = "gts.cf.cache.client.new.v1~";
    let submitted = f
        .cache
        .register_entities(
            &ctx(),
            IdempotencyKey::new("cache-operation").expect("key"),
            RegisterEntitiesRequest {
                items: vec![RegisterItem {
                    gts_id: gts::GtsId::try_new(new).expect("identifier"),
                    content: schema(new, "v1"),
                    expected_resource_version: None,
                    skip_compatibility_check: false,
                }],
                dry_run: false,
                publisher: PublisherContext {
                    name: "cache-test".to_owned(),
                    version: "1.0.0".parse().expect("version"),
                },
            },
        )
        .await
        .expect("accepted");
    let first = f
        .cache
        .get_operation(&ctx(), submitted.operation_id)
        .await
        .expect("poll");
    let second = f
        .cache
        .get_operation(&ctx(), submitted.operation_id)
        .await
        .expect("poll");
    assert_ne!(first.status(), OperationStatus::Completed);
    assert_eq!(
        second.status(),
        OperationStatus::Completed,
        "not a cached poll"
    );
    assert_eq!(f.registry.calls(Call::GetOperation), 2);
}

#[tokio::test(start_paused = true)]
async fn either_key_form_hits_the_same_snapshot() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(f.title(uuid_key(TYPE), full(), false).await, "v1");
    assert_eq!(f.recorder.batch_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn one_entry_answers_every_projection_it_covers_and_no_other() {
    let f = fixture(WINDOW);
    f.lookup(gts_key(TYPE), Projection::Default, false).await;
    let explicit_default = Projection::Select(FieldSelection::with(&EntityField::DEFAULT));
    f.lookup(gts_key(TYPE), explicit_default, false).await;
    assert_eq!(
        f.recorder.batch_count(),
        1,
        "an explicit default reuses Default"
    );

    f.title(gts_key(TYPE), content_only(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "an entry never answers a field it does not hold"
    );
    assert!(
        f.recorder.batches()[1][0]
            .0
            .if_none_match_for(&content_only())
            .is_none(),
        "nor conditions the read on it"
    );

    // Default's origin and the content are one state now held together.
    let both = Projection::Select(FieldSelection::with(&[
        EntityField::Content,
        EntityField::Origin,
    ]));
    let EntityLookup::Found { entity, .. } = f.lookup(gts_key(TYPE), both, false).await else {
        panic!("found");
    };
    assert!(entity.content.is_some() && entity.origin.is_some());
    assert_eq!(f.recorder.batch_count(), 2, "merged under one validator");
}

/// A wider read replaces a stale narrow snapshot so the next narrow hit serves
/// the new state within the window.
#[tokio::test(start_paused = true)]
async fn a_wider_read_of_a_new_state_replaces_the_narrow_entry_it_does_not_cover() {
    let f = fixture(WINDOW);
    assert_eq!(f.title(gts_key(TYPE), content_only(), false).await, "v1");
    change(&f.registry, TYPE, "v2", 2);

    let wider = Projection::Select(FieldSelection::with(&[
        EntityField::Content,
        EntityField::ResolvedSchema,
    ]));
    assert_eq!(f.title(gts_key(TYPE), wider, false).await, "v2");
    assert_eq!(f.title(gts_key(TYPE), content_only(), false).await, "v2");
    assert_eq!(f.recorder.batch_count(), 2, "the last read is a hit");
}

/// A hit is cut down to exactly the fields asked, as the registry would answer it.
#[tokio::test(start_paused = true)]
async fn a_wider_entry_answers_a_narrower_read_with_exactly_its_fields() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { etag: wide, .. } = f.lookup(gts_key(TYPE), full(), false).await
    else {
        panic!("found");
    };
    let narrow = f.lookup(gts_key(TYPE), content_only(), false).await;
    let direct = f
        .registry
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(content_only()),
        )
        .await
        .expect("read")
        .0
        .remove(&gts_key(TYPE))
        .expect("answered");
    assert_eq!(narrow, direct);
    assert!(matches!(&narrow, EntityLookup::Found { etag, .. } if *etag == wide));
    assert_eq!(f.recorder.batch_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_new_state_seen_by_a_narrow_read_replaces_what_the_entry_held() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    change(&f.registry, TYPE, "v2", 2);

    assert_eq!(f.title(gts_key(TYPE), content_only(), true).await, "v2");
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "the v1 fields are not served beside the v2 content for the rest of the window"
    );
    assert_eq!(f.recorder.batch_count(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_caller_validator_is_answered_from_the_current_representation() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { etag, .. } = f.lookup(gts_key(TYPE), full(), false).await else {
        panic!("found");
    };
    let conditional = |etag: Validator| conditioned(gts_key(TYPE), etag);

    let response = f
        .read_with(vec![conditional(etag.clone())], full(), false)
        .await
        .expect("read");
    assert_eq!(
        response.0[&gts_key(TYPE)],
        EntityLookup::Unchanged { etag: etag.clone() }
    );
    let response = f
        .read_with(
            vec![conditional(Validator::from_bytes(b"stale".to_vec()))],
            full(),
            false,
        )
        .await
        .expect("read");
    assert!(matches!(
        response.0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    assert_eq!(f.recorder.batch_count(), 1, "both answered from the cache");
}

#[tokio::test(start_paused = true)]
async fn an_unconditional_fresh_read_of_an_unchanged_entry_still_gets_the_snapshot() {
    // Reconciliation reads `fresh` and treats `Unchanged` as a protocol fault.
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(f.title(gts_key(TYPE), full(), true).await, "v1");
    assert_eq!(f.recorder.batches()[1][0].1, Answer::Unchanged);
}

#[tokio::test(start_paused = true)]
async fn an_out_of_range_batch_is_refused_even_when_every_key_is_cached() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    let error = f
        .read_with(vec![], full(), false)
        .await
        .expect_err("empty batch");
    assert!(
        matches!(error, CanonicalError::InvalidArgument { .. }),
        "{error:?}"
    );
    let oversized = vec![BatchGetItem::new(gts_key(TYPE)); crate::models::MAX_BATCH_GET_KEYS + 1];
    f.read_with(oversized, full(), false)
        .await
        .expect_err("oversized batch");
}

fn long_validator() -> Validator {
    Validator::from_bytes(vec![b'a'; 1025])
}

#[tokio::test(start_paused = true)]
async fn a_long_caller_validator_is_forwarded_on_a_miss_and_compared_on_a_hit() {
    let f = fixture(WINDOW);
    let item = conditioned(gts_key(TYPE), long_validator());
    for _ in 0..2 {
        let response = f
            .read_with(vec![item.clone()], full(), false)
            .await
            .expect("read");
        assert!(matches!(
            response.0[&gts_key(TYPE)],
            EntityLookup::Found { .. }
        ));
    }
    let batches = f.recorder.batches();
    assert_eq!(batches.len(), 1);
    assert_eq!(
        batches[0][0].0.if_none_match_for(&full()),
        Some(&long_validator())
    );
}

#[tokio::test(start_paused = true)]
async fn a_partial_answer_fails_the_read_and_caches_none_of_it() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        response.0.remove(&gts_key(OTHER));
    });
    let error = f
        .read_with(
            vec![gts_key(TYPE).into(), gts_key(OTHER).into()],
            full(),
            false,
        )
        .await
        .expect_err("an answer is missing");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );

    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the answered key was not cached either"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unchanged_answer_to_a_validator_nobody_sent_fails_the_read() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        response.0.insert(
            gts_key(TYPE),
            EntityLookup::Unchanged {
                etag: Validator::from_bytes(b"forged".to_vec()),
            },
        );
    });
    let error = f
        .read_with(vec![gts_key(TYPE).into()], full(), true)
        .await
        .expect_err("unsolicited unchanged");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

/// `Unchanged` must echo the validator sent, not just follow some validator.
#[tokio::test(start_paused = true)]
async fn an_unchanged_answer_to_another_validator_fails_and_extends_nothing() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { etag: pinned, .. } = f.lookup(gts_key(TYPE), full(), false).await
    else {
        panic!("warmed");
    };
    tokio::time::advance(WINDOW).await;
    f.recorder.tamper_next(|response| {
        response.0.insert(
            gts_key(TYPE),
            EntityLookup::Unchanged {
                etag: Validator::from_bytes(b"another".to_vec()),
            },
        );
    });
    let error = f
        .read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("unchanged to another validator");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
    assert_eq!(
        f.recorder.batches()[1][0].0.if_none_match_for(&full()),
        Some(&pinned),
        "the expired entry was revalidated with its own validator"
    );
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        3,
        "the refused answer did not restart the window"
    );
}

#[tokio::test(start_paused = true)]
async fn a_mislabelled_snapshot_fails_the_read_and_is_not_cached() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            entity.kind = crate::models::EntityKind::Instance;
        }
    });
    f.read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("inconsistent kind");
    let healthy = f.lookup(gts_key(TYPE), full(), false).await;
    assert!(
        matches!(&healthy, EntityLookup::Found { entity, .. } if entity.kind == crate::models::EntityKind::TypeSchema),
        "{healthy:?}"
    );
    assert_eq!(f.recorder.batch_count(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_long_etag_over_budget_is_returned_and_drops_the_stale_entry() {
    let f = fixture_with(
        MockTypesRegistry::new(),
        CacheSettings {
            max_size_bytes: 4096,
            ..CacheSettings::default()
        },
    );
    f.title(gts_key(TYPE), full(), false).await;
    change(&f.registry, TYPE, "v2", 2);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { etag, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            *etag = Validator::from_bytes(vec![b'v'; 4096]);
        }
    });
    let EntityLookup::Found { etag, .. } = f.lookup(gts_key(TYPE), full(), true).await else {
        panic!("expected changed state");
    };
    assert_eq!(etag.as_bytes().len(), 4096);
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    assert!(
        f.recorder.batches()[2][0]
            .0
            .if_none_match_for(&full())
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_missing_a_selected_field_fails_the_read_and_is_not_cached() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            entity.origin = None;
        }
    });
    f.read_with(vec![gts_key(TYPE).into()], full(), true)
        .await
        .expect_err("origin was selected");
    let retry = f
        .read_with(vec![gts_key(TYPE).into()], full(), true)
        .await
        .expect("healthy");
    assert!(matches!(
        &retry.0[&gts_key(TYPE)],
        EntityLookup::Found { entity, .. } if entity.origin.is_some()
    ));
    assert!(
        f.recorder.batches()[1][0]
            .0
            .if_none_match_for(&full())
            .is_none(),
        "the broken body left no validator to revalidate against"
    );
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_with_an_unselected_field_fails_the_read() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            entity.content = Some(json!({}));
        }
    });
    let error = f
        .read_with(vec![gts_key(TYPE).into()], Projection::Default, false)
        .await
        .expect_err("content was not selected");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );

    f.lookup(gts_key(TYPE), Projection::Default, false).await;
    assert_eq!(f.recorder.batch_count(), 2, "nothing was cached");
    assert!(
        f.recorder.batches()[1]
            .iter()
            .all(|(item, _)| item.if_none_match_for(&Projection::Default).is_none()),
        "no validator was left to revalidate against"
    );
}

#[tokio::test(start_paused = true)]
async fn a_long_registry_etag_is_cached_and_sent_back_verbatim() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { etag, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            *etag = long_validator();
        }
    });
    for _ in 0..2 {
        let EntityLookup::Found { etag, .. } = f.lookup(gts_key(TYPE), full(), false).await else {
            panic!("expected a snapshot");
        };
        assert_eq!(etag, long_validator());
    }
    assert_eq!(f.recorder.batch_count(), 1);
    f.lookup(gts_key(TYPE), full(), true).await;
    assert_eq!(
        f.recorder.batches()[1][0].0.if_none_match_for(&full()),
        Some(&long_validator())
    );
}

#[tokio::test(start_paused = true)]
async fn a_changed_validator_the_caller_sent_replaces_the_narrower_entry() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), content_only(), false).await;
    // The caller holds every field from elsewhere; this cache holds only the content.
    let held = registry_etag(&f).await;
    change(&f.registry, TYPE, "v2", 2);

    let response = f
        .read_with(
            vec![conditioned(gts_key(TYPE), held.clone())],
            full(),
            false,
        )
        .await
        .expect("read");
    assert!(matches!(
        response.0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    assert_eq!(
        f.recorder.batches()[1][0].0.if_none_match_for(&full()),
        Some(&held),
        "the entry does not cover the read, so the caller's own validator is sent"
    );
    assert_eq!(
        f.title(gts_key(TYPE), content_only(), false).await,
        "v2",
        "the observed change replaced the content-only entry"
    );
    assert_eq!(f.recorder.batch_count(), 2);
}

/// The cache cannot match a caller condition with insufficient field coverage.
#[tokio::test(start_paused = true)]
async fn a_caller_condition_that_does_not_cover_the_read_is_answered_in_full() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { etag, .. } = f.lookup(gts_key(TYPE), full(), false).await else {
        panic!("found");
    };
    let previous = f.lookup(gts_key(TYPE), Projection::Default, false).await;
    let narrow = BatchGetItem::new(gts_key(TYPE)).revalidate(&previous);
    let response = f
        .read_with(vec![narrow.clone()], content_only(), false)
        .await
        .expect("read");
    assert!(matches!(
        &response.0[&gts_key(TYPE)],
        EntityLookup::Found { entity, .. } if entity.content.is_some()
    ));
    let response = f
        .read_with(vec![narrow], Projection::Default, false)
        .await
        .expect("read");
    assert_eq!(response.0[&gts_key(TYPE)], EntityLookup::Unchanged { etag });
    assert_eq!(f.recorder.batch_count(), 1, "both answered from the cache");
}

/// Even a matching validator cannot condition a different entity.
#[tokio::test(start_paused = true)]
async fn a_caller_condition_for_another_entity_is_never_matched() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { etag, .. } = f.lookup(gts_key(OTHER), full(), false).await else {
        panic!("found");
    };
    let previous = EntityLookup::Found {
        entity: Box::new(registry_snapshot(&f.registry, TYPE).await),
        etag,
    };
    let misbound = BatchGetItem::new(gts_key(OTHER)).revalidate(&previous);
    // Warm: answered from the entry.
    let response = f
        .read_with(vec![misbound.clone()], full(), false)
        .await
        .expect("read");
    assert!(matches!(
        response.0[&gts_key(OTHER)],
        EntityLookup::Found { .. }
    ));
    // Cold: forwarded without it.
    let cold = fixture(WINDOW);
    let response = cold
        .read_with(vec![misbound], full(), false)
        .await
        .expect("read");
    assert!(matches!(
        response.0[&gts_key(OTHER)],
        EntityLookup::Found { .. }
    ));
    assert!(
        cold.recorder.batches()[0][0]
            .0
            .if_none_match_for(&full())
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn a_cold_caller_match_does_not_evict_what_the_other_spelling_just_filled() {
    for uuid_first in [false, true] {
        let f = fixture(WINDOW);
        let held = match f
            .registry
            .batch_get_entities(
                &ctx(),
                BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full()),
            )
            .await
            .expect("read")
            .0
            .remove(&gts_key(TYPE))
        {
            Some(EntityLookup::Found { etag, .. }) => etag,
            other => panic!("found, got {other:?}"),
        };
        let by_id = BatchGetItem::new(gts_key(TYPE));
        let by_uuid = conditioned(uuid_key(TYPE), held.clone());
        let items = if uuid_first {
            vec![by_uuid, by_id]
        } else {
            vec![by_id, by_uuid]
        };
        let response = f.read_with(items, full(), false).await.expect("read");
        assert!(matches!(
            response.0[&gts_key(TYPE)],
            EntityLookup::Found { .. }
        ));
        assert_eq!(
            response.0[&uuid_key(TYPE)],
            EntityLookup::Unchanged { etag: held }
        );

        f.title(gts_key(TYPE), full(), false).await;
        assert_eq!(
            f.recorder.batch_count(),
            1,
            "uuid_first={uuid_first}: still resident"
        );
    }
}

fn registration(id: &str, title: &str, expected: Option<u64>) -> RegisterEntitiesRequest {
    RegisterEntitiesRequest {
        items: vec![RegisterItem {
            gts_id: gts::GtsId::try_new(id).expect("identifier"),
            content: schema(id, title),
            expected_resource_version: expected,
            skip_compatibility_check: false,
        }],
        dry_run: false,
        publisher: PublisherContext {
            name: "cache-test".to_owned(),
            version: "1.0.0".parse().expect("version"),
        },
    }
}

impl Fixture {
    async fn register(&self, request: RegisterEntitiesRequest) -> RegistrationOperation {
        self.cache
            .register_entities(&ctx(), IdempotencyKey::generate(), request)
            .await
            .expect("accepted")
    }
}

#[tokio::test(start_paused = true)]
async fn a_terminal_registration_invalidates_every_projection_under_both_key_forms() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    f.title(uuid_key(TYPE), content_only(), false).await;

    let operation = f.register(registration(TYPE, "v2", Some(1))).await;
    assert!(matches!(
        operation.items[0].outcome,
        RegistrationOutcome::Succeeded { .. }
    ));

    assert_eq!(f.title(uuid_key(TYPE), full(), false).await, "v2");
    assert_eq!(f.title(gts_key(TYPE), content_only(), false).await, "v2");
}

#[tokio::test(start_paused = true)]
async fn an_accepted_registration_invalidates_nothing_until_a_poll_sees_it_commit() {
    let f = fixture_over(MockTypesRegistry::new().completing_after(2), WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    let accepted = f.register(registration(TYPE, "v2", Some(1))).await;
    assert!(
        !accepted.items[0].outcome.is_terminal(),
        "a 202, not a commit"
    );

    let first = f
        .cache
        .get_operation(&ctx(), accepted.operation_id)
        .await
        .expect("poll");
    assert_ne!(first.status(), OperationStatus::Completed);
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        1,
        "neither the acceptance nor a pending poll invalidated the entry"
    );
    let done = f
        .cache
        .get_operation(&ctx(), accepted.operation_id)
        .await
        .expect("poll");
    assert_eq!(done.status(), OperationStatus::Completed);
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    assert_eq!(f.recorder.batch_count(), 2, "the terminal poll did");
}

#[tokio::test(start_paused = true)]
async fn a_mixed_operation_invalidates_only_its_committed_candidates() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(OTHER), full(), false).await;
    f.registry
        .reject(OTHER, AdmissionFailureReason::InvalidSchema);
    let mut request = registration(TYPE, "v2", Some(1));
    request
        .items
        .extend(registration(OTHER, "v2", Some(1)).items);
    let operation = f.register(request).await;
    assert!(matches!(
        operation.items[0].outcome,
        RegistrationOutcome::Succeeded { .. }
    ));
    assert!(matches!(
        operation.items[1].outcome,
        RegistrationOutcome::Failed { .. }
    ));

    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    assert_eq!(
        f.recorder.batch_count(),
        3,
        "the committed key was read anew"
    );
    f.title(gts_key(OTHER), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        3,
        "the refused one is still a hit"
    );
}

#[tokio::test(start_paused = true)]
async fn a_deletion_by_uuid_invalidates_the_entry_read_by_identifier() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { entity, .. } = f.lookup(gts_key(TYPE), full(), false).await else {
        panic!("found");
    };
    assert_eq!(
        entity.lifecycle_status,
        crate::models::LifecycleStatus::Active
    );
    f.cache
        .delete_entities(
            &ctx(),
            IdempotencyKey::generate(),
            DeleteEntitiesRequest {
                items: vec![DeleteItem {
                    key: uuid_key(TYPE),
                    expected_resource_version: 1,
                }],
                dry_run: false,
                publisher: PublisherContext {
                    name: "cache-test".to_owned(),
                    version: "1.0.0".parse().expect("version"),
                },
            },
        )
        .await
        .expect("deleted");
    let EntityLookup::Found { entity, .. } = f.lookup(gts_key(TYPE), full(), false).await else {
        panic!("a tombstone still reads");
    };
    assert_eq!(
        entity.lifecycle_status,
        crate::models::LifecycleStatus::Deleted
    );
}

#[tokio::test(start_paused = true)]
async fn a_dry_run_an_unchanged_and_a_refused_registration_invalidate_nothing() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(OTHER), full(), false).await;
    // Change the registry directly so a wrongly dropped entry would show it.
    change(&f.registry, TYPE, "direct", 1);
    change(&f.registry, OTHER, "direct", 1);

    let mut dry = registration(TYPE, "v2", Some(1));
    dry.dry_run = true;
    let operation = f.register(dry).await;
    assert!(matches!(
        operation.items[0].outcome,
        RegistrationOutcome::WouldSucceed
    ));

    // Identical to what is stored now: the metadata-only confirmation path.
    let operation = f.register(registration(TYPE, "direct", Some(1))).await;
    assert!(
        matches!(
            operation.items[0].outcome,
            RegistrationOutcome::Unchanged { .. }
        ),
        "{:?}",
        operation.items[0].outcome
    );

    f.registry
        .reject(OTHER, AdmissionFailureReason::InvalidSchema);
    let operation = f.register(registration(OTHER, "v2", Some(1))).await;
    assert!(matches!(
        operation.items[0].outcome,
        RegistrationOutcome::Failed { .. }
    ));

    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v1");
    assert_eq!(f.title(gts_key(OTHER), full(), false).await, "v1");
    assert_eq!(f.recorder.batch_count(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_read_started_before_a_terminal_outcome_never_fills_the_entry() {
    let f = fixture(WINDOW);
    let release = f.recorder.hold_next();
    let read = f.title(uuid_key(TYPE), full(), false);
    let mutate = async {
        tokio::task::yield_now().await;
        // By identifier, with nothing resident: only the read's ticket keeps the slot.
        f.register(registration(TYPE, "v2", Some(1))).await;
        release.send(()).expect("held read");
    };
    let (old, ()) = tokio::join!(read, mutate);
    assert_eq!(old, "v1", "the read in flight returns what it read");

    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "and did not fill the entry"
    );
    assert_eq!(f.recorder.batch_count(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_fresh_read_seeing_a_new_validator_fences_a_wider_read_in_flight() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), content_only(), false).await;
    // Not covered by the content-only entry, so it goes to the registry and is held.
    let release = f.recorder.hold_next();
    let stale = f.title(gts_key(TYPE), full(), false);
    let fresh = async {
        tokio::task::yield_now().await;
        change(&f.registry, TYPE, "v2", 2);
        let title = f.title(gts_key(TYPE), content_only(), true).await;
        release.send(()).expect("held read");
        title
    };
    let (old, new) = tokio::join!(stale, fresh);
    assert_eq!((old.as_str(), new.as_str()), ("v1", "v2"));
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    assert_eq!(f.title(gts_key(TYPE), content_only(), false).await, "v2");
}

#[tokio::test(start_paused = true)]
async fn an_older_cold_read_does_not_overwrite_a_newer_one() {
    let f = fixture(WINDOW);
    let release = f.recorder.hold_next();
    let older = f.title(gts_key(TYPE), full(), false);
    let newer = async {
        tokio::task::yield_now().await;
        change(&f.registry, TYPE, "v2", 2);
        let title = f.title(gts_key(TYPE), full(), false).await;
        release.send(()).expect("held read");
        title
    };
    let (old, new) = tokio::join!(older, newer);
    assert_eq!((old.as_str(), new.as_str()), ("v1", "v2"));
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    // Conflicting replies dropped the resident entry, forcing a refetch.
    assert_eq!(f.recorder.batch_count(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_read_releases_its_ticket() {
    let f = fixture(WINDOW);
    let _release = f.recorder.hold_next();
    let key = gts_key(TYPE);
    {
        let read = std::pin::pin!(f.title(key.clone(), full(), false));
        tokio::select! {
            _ = read => panic!("held"),
            () = tokio::task::yield_now() => {}
        }
        assert!(f.cache.store().indexes(&key), "the read holds a ticket");
    }
    assert!(
        !f.cache.store().indexes(&key),
        "dropping the read released it"
    );
}

#[tokio::test(start_paused = true)]
async fn of_two_overlapping_fresh_reads_the_later_answer_is_not_contradicted() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    change(&f.registry, TYPE, "v2", 2);
    // The first fresh read plans under v1, then waits before reading the registry.
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(5)),
    );
    let slow = f.title(gts_key(TYPE), full(), true);
    let quick = async {
        tokio::task::yield_now().await;
        let title = f.title(gts_key(TYPE), full(), true).await;
        change(&f.registry, TYPE, "v3", 3);
        title
    };
    let (slow, quick) = tokio::join!(slow, quick);
    assert_eq!((quick.as_str(), slow.as_str()), ("v2", "v3"));
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v3",
        "the v2 entry the later answer contradicts is not served"
    );
}

#[tokio::test(start_paused = true)]
async fn an_older_conditional_read_does_not_replace_a_newer_answer() {
    let f = fixture(WINDOW);
    let held = match f
        .registry
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full()),
        )
        .await
        .expect("read")
        .0
        .remove(&gts_key(TYPE))
    {
        Some(EntityLookup::Found { etag, .. }) => etag,
        other => panic!("found, got {other:?}"),
    };
    change(&f.registry, TYPE, "v2", 2);
    let release = f.recorder.hold_next();
    let older = f.read_with(vec![conditioned(gts_key(TYPE), held)], full(), false);
    let newer = async {
        tokio::task::yield_now().await;
        change(&f.registry, TYPE, "v3", 3);
        let title = f.title(gts_key(TYPE), full(), false).await;
        release.send(()).expect("held read");
        title
    };
    let (older, newer) = tokio::join!(older, newer);
    assert!(matches!(
        older.expect("read").0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    assert_eq!(newer, "v3");
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v3");
}

#[tokio::test(start_paused = true)]
async fn a_fresh_read_revalidates_the_cached_entry_even_with_a_long_caller_condition() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { etag: pinned, .. } = f.lookup(gts_key(TYPE), full(), false).await
    else {
        panic!("warmed");
    };
    change(&f.registry, TYPE, "v2", 2);
    f.read_with(
        vec![conditioned(gts_key(TYPE), long_validator())],
        full(),
        true,
    )
    .await
    .expect("read changed state");
    assert_eq!(
        f.recorder.batches()[1][0].0.if_none_match_for(&full()),
        Some(&pinned),
        "the entry's validator is sent, not the caller's"
    );
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    assert_eq!(f.recorder.batch_count(), 2);
}

/// The validator the registry would issue for `TYPE` under `full()` now, read past the cache.
async fn registry_etag(f: &Fixture) -> Validator {
    match f
        .registry
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full()),
        )
        .await
        .expect("read")
        .0
        .remove(&gts_key(TYPE))
    {
        Some(EntityLookup::Found { etag, .. }) => etag,
        other => panic!("found, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_later_caller_match_supersedes_an_older_cold_answer() {
    let f = fixture(WINDOW);
    let release = f.recorder.hold_next();
    let older = f.title(gts_key(TYPE), full(), false);
    let later = async {
        tokio::task::yield_now().await;
        change(&f.registry, TYPE, "v2", 2);
        let current = registry_etag(&f).await;
        let response = f
            .read_with(
                vec![conditioned(gts_key(TYPE), current.clone())],
                full(),
                false,
            )
            .await
            .expect("read");
        assert_eq!(
            response.0[&gts_key(TYPE)],
            EntityLookup::Unchanged { etag: current }
        );
        release.send(()).expect("held read");
    };
    let (older, ()) = tokio::join!(older, later);
    assert_eq!(older, "v1");
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "the older v1 answer did not fill after the later one"
    );
}

#[tokio::test(start_paused = true)]
async fn an_older_cold_read_answering_a_newer_revision_drops_the_resident_one() {
    let f = fixture(WINDOW);
    // The older read takes its ticket first, then waits before reading the registry.
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(5)),
    );
    let older = f.title(gts_key(TYPE), full(), true);
    let later = async {
        tokio::task::yield_now().await;
        change(&f.registry, TYPE, "v2", 2);
        let title = f.title(gts_key(TYPE), full(), false).await;
        change(&f.registry, TYPE, "v3", 3);
        title
    };
    let (older, later) = tokio::join!(older, later);
    assert_eq!((later.as_str(), older.as_str()), ("v2", "v3"));
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v3",
        "the v2 entry the older read contradicted is not served"
    );
}

#[tokio::test(start_paused = true)]
async fn a_later_ticket_answering_an_older_revision_does_not_replace_a_newer_fill() {
    let f = fixture(WINDOW);
    // A takes the first ticket and waits before reading the registry.
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(5)),
    );
    // B takes the second ticket, reads v1 at once and is held before committing.
    let release = f.recorder.hold_next();
    let a = f.title(gts_key(TYPE), full(), false);
    let b = f.title(gts_key(TYPE), full(), false);
    let control = async {
        change(&f.registry, TYPE, "v2", 2);
        // A reads v2 and records it at the fifth second; B commits after it.
        tokio::time::sleep(Duration::from_secs(6)).await;
        release.send(()).expect("held read");
    };
    let (a, b, ()) = tokio::join!(a, b, control);
    assert_eq!((a.as_str(), b.as_str()), ("v2", "v1"));
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "B's older revision did not replace A's"
    );
    assert_eq!(
        f.recorder.batch_count(),
        3,
        "B's conflicting answer dropped the entry, so v2 was read anew"
    );
}

#[tokio::test(start_paused = true)]
async fn both_spellings_revalidated_in_one_batch_keep_the_new_revision() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    tokio::time::advance(WINDOW).await;
    change(&f.registry, TYPE, "v2", 2);
    f.read_with(
        vec![gts_key(TYPE).into(), uuid_key(TYPE).into()],
        full(),
        false,
    )
    .await
    .expect("read");
    assert_eq!(f.title(uuid_key(TYPE), full(), false).await, "v2");
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the second spelling kept the first's fill"
    );
}

/// The snapshot the registry holds for `id` under `full()`, read past the cache.
async fn registry_snapshot(registry: &MockTypesRegistry, id: &str) -> crate::models::Entity {
    match registry
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest::new([gts_key(id).into()]).with_projection(full()),
        )
        .await
        .expect("read")
        .0
        .remove(&gts_key(id))
    {
        Some(EntityLookup::Found { entity, .. }) => *entity,
        other => panic!("found, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn an_older_answer_does_not_fill_after_a_newer_one_was_evicted() {
    let registry = MockTypesRegistry::new().completing_after(0);
    for id in [TYPE, OTHER, THIRD] {
        registry.seed(id, schema(id, "v1"));
    }
    let f = fixture_with(
        registry,
        CacheSettings {
            freshness_window: WINDOW,
            max_entries: 1,
            ..CacheSettings::default()
        },
    );
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(5)),
    );
    let release = f.recorder.hold_next();
    let a = f.title(gts_key(TYPE), full(), false);
    let b = f.title(gts_key(TYPE), full(), false);
    let control = async {
        change(&f.registry, TYPE, "v2", 2);
        tokio::time::sleep(Duration::from_secs(6)).await;
        // A recorded v2; evict it before B, holding v1, commits.
        assert!(f.cache.store().peek(&gts_key(TYPE)).is_some(), "A filled v2");
        f.title(gts_key(OTHER), full(), false).await;
        assert!(
            f.cache.store().peek(&gts_key(TYPE)).is_none(),
            "OTHER evicted v2 before B settles"
        );
        release.send(()).expect("held read");
    };
    let (a, b, ()) = tokio::join!(a, b, control);
    assert_eq!((a.as_str(), b.as_str()), ("v2", "v1"));
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    assert_eq!(
        f.recorder.batch_count(),
        4,
        "B's older answer filled nothing, so v2 was read anew"
    );
}

#[tokio::test(start_paused = true)]
async fn a_newer_revision_seen_under_one_projection_reaches_a_projection_filled_meanwhile() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(TYPE), content_only(), false).await;
    change(&f.registry, TYPE, "v2", 2);
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(5)),
    );
    // A: a fresh read of the full projection, waiting before it reads the registry.
    let a = f.title(gts_key(TYPE), full(), true);
    let others = async {
        // B's fresh read sees v2 under content-only: it drops the full projection and
        // fills its own while A waits.
        let b = f.title(gts_key(TYPE), content_only(), true).await;
        change(&f.registry, TYPE, "v3", 3);
        b
    };
    let (a, b) = tokio::join!(a, others);
    assert_eq!((b.as_str(), a.as_str()), ("v2", "v3"));
    assert_eq!(
        f.title(gts_key(TYPE), content_only(), false).await,
        "v3",
        "the v2 projection filled meanwhile is not served after v3 was seen"
    );
}

#[tokio::test(start_paused = true)]
async fn a_confirmation_overtaken_by_another_revision_does_not_keep_it() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    tokio::time::advance(WINDOW).await;
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(5)),
    );
    // A revalidates its expired v1 entry, waiting before it reads the registry.
    let a = f.title(gts_key(TYPE), full(), false);
    let others = async {
        change(&f.registry, TYPE, "v2", 2);
        let b = f.title(gts_key(TYPE), full(), true).await;
        // Back to the first revision: A's validator matches again.
        change(&f.registry, TYPE, "v1", 1);
        b
    };
    let (a, b) = tokio::join!(a, others);
    assert_eq!((a.as_str(), b.as_str()), ("v1", "v2"));
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v1",
        "A's confirmation of v1 does not leave B's v2 resident"
    );
}

fn deletion(items: &[(&str, u64)], dry_run: bool) -> DeleteEntitiesRequest {
    DeleteEntitiesRequest {
        items: items
            .iter()
            .map(|(id, version)| DeleteItem {
                key: uuid_key(id),
                expected_resource_version: *version,
            })
            .collect(),
        dry_run,
        publisher: PublisherContext {
            name: "cache-test".to_owned(),
            version: "1.0.0".parse().expect("version"),
        },
    }
}

impl Fixture {
    async fn delete(&self, request: DeleteEntitiesRequest) -> DeletionOperation {
        self.cache
            .delete_entities(&ctx(), IdempotencyKey::generate(), request)
            .await
            .expect("accepted")
    }
}

#[tokio::test(start_paused = true)]
async fn a_dry_run_and_a_refused_deletion_invalidate_nothing_beside_a_committed_one() {
    let f = fixture(WINDOW);
    for id in [TYPE, OTHER, THIRD] {
        f.title(gts_key(id), full(), false).await;
    }
    assert_eq!(f.recorder.batch_count(), 3);

    let dry = f.delete(deletion(&[(TYPE, 1)], true)).await;
    assert!(matches!(
        dry.items[0].outcome,
        crate::models::DeletionOutcome::WouldSucceed
    ));
    // A stale expected version is refused; the third key commits.
    let mixed = f.delete(deletion(&[(OTHER, 99), (THIRD, 1)], false)).await;
    assert!(matches!(
        mixed.items[0].outcome,
        crate::models::DeletionOutcome::Failed { .. }
    ));
    assert!(matches!(
        mixed.items[1].outcome,
        crate::models::DeletionOutcome::Succeeded { .. }
    ));

    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(OTHER), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        3,
        "the dry-run and refused keys are still hits"
    );
    f.lookup(gts_key(THIRD), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        4,
        "the committed key was read anew"
    );
}

#[tokio::test(start_paused = true)]
async fn a_deletion_invalidates_when_a_poll_sees_it_commit() {
    let f = fixture_over(MockTypesRegistry::new().completing_after(2), WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    let accepted = f.delete(deletion(&[(TYPE, 1)], false)).await;
    assert!(
        !matches!(
            accepted.items[0].outcome,
            crate::models::DeletionOutcome::Succeeded { .. }
        ),
        "a 202, not a commit"
    );

    let first = f
        .cache
        .get_operation(&ctx(), accepted.operation_id)
        .await
        .expect("poll");
    assert_ne!(first.status(), OperationStatus::Completed);
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        1,
        "neither the acceptance nor a pending poll invalidated the entry"
    );

    let done = f
        .cache
        .get_operation(&ctx(), accepted.operation_id)
        .await
        .expect("poll");
    assert_eq!(done.status(), OperationStatus::Completed);
    f.lookup(gts_key(TYPE), full(), false).await;
    assert_eq!(f.recorder.batch_count(), 2, "the terminal poll did");
}

#[tokio::test(start_paused = true)]
async fn an_answer_that_is_another_entity_fails_the_read_and_is_not_cached() {
    let f = fixture(WINDOW);
    let other = registry_snapshot(&f.registry, OTHER).await;
    f.recorder.tamper_next(move |response| {
        if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            **entity = other;
        }
    });
    let error = f
        .read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("a consistent snapshot of the wrong entity");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );

    let healthy = f.lookup(gts_key(TYPE), full(), false).await;
    assert!(
        matches!(&healthy, EntityLookup::Found { entity, .. } if entity.gts_id.to_string() == TYPE),
        "{healthy:?}"
    );
    assert_eq!(f.recorder.batch_count(), 2, "nothing was cached");
}

#[tokio::test(start_paused = true)]
async fn an_answer_for_a_key_nobody_asked_fails_the_read_and_caches_none_of_it() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        response.0.insert(gts_key(OTHER), EntityLookup::NotFound);
    });
    let error = f
        .read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("an unrequested answer");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );

    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the asked key was not cached either"
    );
}

/// ID and UUID replies for one entity may settle in either order. The second
/// must judge against the resident state left by the first.
#[tokio::test(start_paused = true)]
async fn a_caller_match_beside_a_fill_of_the_new_state_keeps_that_fill() {
    for uuid_first in [false, true] {
        let f = fixture(WINDOW);
        f.title(gts_key(TYPE), content_only(), false).await;
        change(&f.registry, TYPE, "v2", 2);
        let current = registry_etag(&f).await;
        let by_id = BatchGetItem::new(gts_key(TYPE));
        let by_uuid = conditioned(uuid_key(TYPE), current.clone());
        let items = if uuid_first {
            vec![by_uuid, by_id]
        } else {
            vec![by_id, by_uuid]
        };
        let response = f.read_with(items, full(), false).await.expect("read");
        assert!(matches!(
            &response.0[&gts_key(TYPE)],
            EntityLookup::Found { etag, .. } if *etag == current
        ));
        assert_eq!(
            response.0[&uuid_key(TYPE)],
            EntityLookup::Unchanged {
                etag: current.clone()
            }
        );

        assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
        assert_eq!(
            f.recorder.batch_count(),
            2,
            "uuid_first={uuid_first}: the v2 fill is resident and answers the full read"
        );
    }
}

/// The byte weight of `TYPE`'s full snapshot as the registry holds it now.
async fn full_weight(registry: &MockTypesRegistry) -> usize {
    super::store::weight(&registry_snapshot(registry, TYPE).await).total()
}

/// An oversized merge leaves the valid smaller snapshot resident.
#[tokio::test(start_paused = true)]
async fn an_oversized_answer_of_the_same_state_keeps_the_smaller_entry() {
    let big = json!({
        "$id": format!("gts://{TYPE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "title": "v1",
        "description": "x".repeat(4096),
    });
    let measured = MockTypesRegistry::new();
    measured.seed(TYPE, big.clone());
    // Room for the light snapshot, never the documents: they carry the description twice.
    let bound = full_weight(&measured).await - 4096;
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        CacheSettings {
            freshness_window: WINDOW,
            max_size_bytes: bound,
            ..CacheSettings::default()
        },
    );
    f.registry.seed(TYPE, big);
    f.lookup(gts_key(TYPE), Projection::Default, false).await;
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(f.recorder.batch_count(), 2);

    f.lookup(gts_key(TYPE), Projection::Default, false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the light entry is still served"
    );
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(f.recorder.batch_count(), 3, "the documents were never kept");
}

/// An oversized reply with a new validator still drops the stale snapshot.
#[tokio::test(start_paused = true)]
async fn an_oversized_answer_of_a_new_state_drops_the_old_entry() {
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        CacheSettings {
            freshness_window: WINDOW,
            max_size_bytes: 4096,
            ..CacheSettings::default()
        },
    );
    f.lookup(gts_key(TYPE), Projection::Default, false).await;
    let huge = json!({
        "$id": format!("gts://{TYPE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "title": "v2",
        "description": "x".repeat(8192),
    });
    f.registry.seed_entity(Seed::new(TYPE, huge).version(2));

    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
    let EntityLookup::Found { entity, .. } =
        f.lookup(gts_key(TYPE), Projection::Default, false).await
    else {
        panic!("found");
    };
    assert!(
        matches!(
            entity.origin,
            Some(crate::models::Origin::Managed {
                resource_version: 2,
                ..
            })
        ),
        "the v1 entry was dropped, not served: {:?}",
        entity.origin
    );
    assert_eq!(f.recorder.batch_count(), 3);
}

// Validate registry-issued pairs against resident, remembered and pinned pairs,
// including other replies in the batch. Conflicts leave both stores unchanged.

/// A Registry Reference no derivation produces for any fixture identifier.
const ISSUED: Uuid = Uuid::from_u128(0x0123_4567_89ab_4cde_8f01_2345_6789_abcd);
const FORGED: Uuid = Uuid::from_u128(0x0fed_cba9_8765_4321_8fed_cba9_8765_4321);

/// TYPE as the registry reads it, but under `gts_uuid`.
async fn snapshot_under(registry: &MockTypesRegistry, gts_uuid: Uuid) -> crate::models::Entity {
    let mut entity = registry_snapshot(registry, TYPE).await;
    entity.gts_uuid = gts_uuid;
    entity
}

/// Rewrites the next answer for `key` to `entity`.
fn answer_next(f: &Fixture, key: EntityKey, entity: crate::models::Entity) {
    f.recorder.tamper_next(move |response| {
        response.0.insert(
            key,
            EntityLookup::Found {
                entity: Box::new(entity),
                etag: Validator::from_bytes(b"forged".to_vec()),
            },
        );
    });
}

/// Room for one full snapshot of TYPE or OTHER, never both.
fn one_snapshot_settings(max_identity_entries: usize) -> CacheSettings {
    CacheSettings {
        freshness_window: WINDOW,
        max_entries: 1,
        max_identity_entries,
        ..CacheSettings::default()
    }
}

fn assert_protocol_fault(result: Result<BatchGetEntitiesResponse, CanonicalError>) {
    let error = result.expect_err("a contradicting identity");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

/// Reject a conflicting Registry Reference against the pinned pair even after
/// both stores evict it.
#[tokio::test(start_paused = true)]
async fn a_revalidation_contradicting_its_pinned_pair_fails_after_both_stores_evicted_it() {
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        one_snapshot_settings(1),
    );
    f.title(gts_key(TYPE), full(), false).await;
    let pinned = uuid_key(TYPE);
    tokio::time::advance(WINDOW).await;

    let forged = snapshot_under(&f.registry, FORGED).await;
    let release = f.recorder.hold_next();
    answer_next(&f, gts_key(TYPE), forged);
    let (revalidation, ()) = tokio::join!(
        f.read_with(vec![gts_key(TYPE).into()], full(), false),
        async {
            f.title(gts_key(OTHER), full(), false).await;
            assert!(f.cache.store().peek(&gts_key(TYPE)).is_none(), "evicted");
            assert!(f.cache.identity(&gts_key(TYPE)).is_none(), "evicted");
            release.send(()).expect("held read");
        }
    );
    // Batches are recorded as answered: the held revalidation last.
    let held = &f.recorder.batches()[2][0].0;
    assert_eq!(held.key, gts_key(TYPE));
    assert!(
        held.if_none_match_for(&full()).is_some(),
        "the held read revalidated its pinned entry"
    );
    assert_protocol_fault(revalidation);

    assert!(f.cache.identity(&EntityKey::GtsUuid(FORGED)).is_none());
    assert!(f.cache.identity(&gts_key(TYPE)).is_none());
    let store = f.cache.store();
    assert!(store.peek(&EntityKey::GtsUuid(FORGED)).is_none());
    assert!(store.peek(&gts_key(TYPE)).is_none());
    assert!(store.peek(&pinned).is_none());
}

/// Conflicting references within one batch fail before any fill, even without an identity LRU.
#[tokio::test(start_paused = true)]
async fn a_batch_contradicting_itself_fails_and_fills_nothing() {
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        CacheSettings {
            max_identity_entries: 0,
            ..CacheSettings::default()
        },
    );
    let forged = snapshot_under(&f.registry, FORGED).await;
    answer_next(&f, EntityKey::GtsUuid(FORGED), forged);
    assert_protocol_fault(
        f.read_with(
            vec![gts_key(TYPE).into(), EntityKey::GtsUuid(FORGED).into()],
            full(),
            false,
        )
        .await,
    );

    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(f.recorder.batch_count(), 2, "neither answer was kept");
    assert_eq!(
        f.lookup(EntityKey::GtsUuid(FORGED), full(), false).await,
        EntityLookup::NotFound
    );
}

/// A resident snapshot rejects a conflicting reference even without an identity LRU.
#[tokio::test(start_paused = true)]
async fn an_answer_contradicting_a_resident_pair_fails_and_leaves_the_entry() {
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        CacheSettings {
            max_identity_entries: 0,
            ..CacheSettings::default()
        },
    );
    f.title(gts_key(TYPE), full(), false).await;
    let forged = snapshot_under(&f.registry, FORGED).await;
    answer_next(&f, EntityKey::GtsUuid(FORGED), forged);
    assert_protocol_fault(
        f.read_with(vec![EntityKey::GtsUuid(FORGED).into()], full(), false)
            .await,
    );

    f.title(gts_key(TYPE), full(), false).await;
    f.title(uuid_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the entry still serves both spellings"
    );
}

/// A remembered pair holds after its snapshot was evicted.
#[tokio::test(start_paused = true)]
async fn an_answer_contradicting_a_remembered_pair_fails_after_its_snapshot_was_evicted() {
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        one_snapshot_settings(CacheSettings::DEFAULT_MAX_IDENTITY_ENTRIES),
    );
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(OTHER), full(), false).await;
    assert!(f.cache.store().peek(&gts_key(TYPE)).is_none(), "evicted");

    let forged = snapshot_under(&f.registry, FORGED).await;
    answer_next(&f, EntityKey::GtsUuid(FORGED), forged);
    assert_protocol_fault(
        f.read_with(vec![EntityKey::GtsUuid(FORGED).into()], full(), false)
            .await,
    );
    assert!(f.cache.store().peek(&EntityKey::GtsUuid(FORGED)).is_none());
    let known = f.cache.identity(&gts_key(TYPE)).expect("still remembered");
    assert_eq!(EntityKey::GtsUuid(known.gts_uuid), uuid_key(TYPE));
}

/// The first settled pair wins. A conflicting concurrent reply cannot replace it.
#[tokio::test(start_paused = true)]
async fn of_two_reads_answered_with_contradicting_pairs_the_later_settled_fails() {
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        CacheSettings {
            max_identity_entries: 0,
            ..CacheSettings::default()
        },
    );
    let forged = snapshot_under(&f.registry, FORGED).await;
    let release = f.recorder.hold_next();
    let (held, ()) = tokio::join!(
        f.read_with(vec![gts_key(TYPE).into()], full(), false),
        async {
            answer_next(&f, EntityKey::GtsUuid(FORGED), forged);
            f.title(EntityKey::GtsUuid(FORGED), full(), false).await;
            release.send(()).expect("held read");
        }
    );
    assert_protocol_fault(held);
    assert_eq!(
        f.cache
            .store()
            .peek(&gts_key(TYPE))
            .map(|entry| entry.snapshot.gts_uuid),
        Some(FORGED),
        "the pair settled first stays"
    );
}

/// A one-sided ID miss fences a cold read by UUID, which might be its counterpart.
/// Another ID cannot alias it and still fills.
#[tokio::test(start_paused = true)]
async fn a_miss_on_another_identifier_does_not_fence_a_cold_read_by_identifier() {
    for (cold, fills) in [(gts_key(TYPE), true), (uuid_key(TYPE), false)] {
        let f = fixture(WINDOW);
        let release = f.recorder.hold_next();
        let (held, ()) = tokio::join!(f.lookup(cold.clone(), full(), false), async {
            assert_eq!(
                f.lookup(gts_key(UNREGISTERED), full(), false).await,
                EntityLookup::NotFound
            );
            release.send(()).expect("held read");
        });
        assert!(matches!(held, EntityLookup::Found { .. }), "{held:?}");
        f.title(cold.clone(), full(), false).await;
        let expected = if fills { 2 } else { 3 };
        assert_eq!(f.recorder.batch_count(), expected, "{cold}: filled {fills}");
    }
}

/// An issued Registry Reference works for snapshot hits, resolution and
/// revalidation. Registration by ID invalidates it even without an identity LRU.
#[tokio::test(start_paused = true)]
async fn an_issued_registry_reference_is_served_cached_under_either_spelling() {
    use crate::PlatformTypesRegistryApiExt;

    let issued = EntityKey::GtsUuid(ISSUED);
    let type_id = gts::GtsTypeId::try_new(TYPE).expect("a Type Schema identifier");
    for max_identity_entries in [0, CacheSettings::DEFAULT_MAX_IDENTITY_ENTRIES] {
        let f = fixture_with(
            MockTypesRegistry::new().completing_after(0),
            CacheSettings {
                max_identity_entries,
                ..CacheSettings::default()
            },
        );
        f.recorder.tamper_next(|response| {
            if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
                entity.gts_uuid = ISSUED;
            }
        });
        assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v1");
        assert_eq!(f.title(issued.clone(), full(), false).await, "v1");
        assert_eq!(
            f.cache
                .resolve_type_uuid(&ctx(), &type_id)
                .await
                .expect("known"),
            ISSUED
        );
        assert_eq!(
            f.cache
                .resolve_type_id(&ctx(), ISSUED)
                .await
                .expect("known"),
            type_id
        );
        assert_eq!(
            f.recorder.batch_count(),
            1,
            "{max_identity_entries}: one entry answered"
        );

        tokio::time::advance(WINDOW).await;
        let held = f
            .cache
            .store()
            .peek(&issued)
            .expect("resident")
            .etag
            .clone();
        let current = held.clone();
        f.recorder.tamper_next(move |response| {
            response.0.insert(
                EntityKey::GtsUuid(ISSUED),
                EntityLookup::Unchanged { etag: current },
            );
        });
        assert_eq!(f.title(issued.clone(), full(), false).await, "v1");
        let revalidation = &f.recorder.batches()[1][0];
        assert_eq!(revalidation.0.key, issued);
        assert_eq!(
            revalidation.0.if_none_match_for(&full()),
            Some(&held),
            "{max_identity_entries}: revalidated under the issued Registry Reference"
        );
        assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v1");
        assert_eq!(
            f.recorder.batch_count(),
            2,
            "the confirmation serves the identifier"
        );

        let operation = f.register(registration(TYPE, "v2", Some(1))).await;
        assert!(matches!(
            operation.items[0].outcome,
            RegistrationOutcome::Succeeded { .. }
        ));
        let changed = snapshot_under(&f.registry, ISSUED).await;
        answer_next(&f, issued.clone(), changed);
        assert_eq!(f.title(issued.clone(), full(), false).await, "v2");
        assert!(
            f.recorder.batches()[2][0]
                .0
                .if_none_match_for(&full())
                .is_none(),
            "{max_identity_entries}: the registration by identifier dropped the issued spelling"
        );
    }
}

#[path = "resolve_tests.rs"]
mod resolve_tests;

#[path = "tenant_tests.rs"]
mod tenant_tests;

#[path = "metrics_tests.rs"]
mod metrics_tests;
