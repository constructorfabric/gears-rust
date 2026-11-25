use std::collections::{HashMap, HashSet};

use toolkit_security::SecurityContext;

use super::*;
use crate::cache::CachedTypesRegistry;
use crate::cache::store::Scope;
use crate::contract::TypesRegistryApi;
use crate::models::{Entity, Ownership};

/// A tenant registry with entity visibility independent of the snapshot contents.
/// Its entities are tenant-owned unless marked global, so a test opts into the
/// shared global path explicitly.
struct TenantRegistry {
    registry: MockTypesRegistry,
    /// Entities answered as `Ownership::Global`.
    global: Mutex<HashSet<Uuid>>,
    /// How tenant-owned entities are labelled for a reader's tenant; `ContextTenant` otherwise.
    labels: Mutex<HashMap<Uuid, Ownership>>,
    calls: Mutex<Vec<(Uuid, Option<Validator>)>>,
    /// Subjects and tenants the registry answers `not_found`.
    hidden: Mutex<HashSet<Uuid>>,
    /// How long a hidden read takes to answer.
    hidden_delay: Mutex<Duration>,
    /// Registry References a read answers with instead of the stored ones, by identifier.
    misreported: Mutex<HashMap<String, Uuid>>,
    /// Each batch resolution's tenant and keys, in order.
    resolutions: Mutex<Vec<(Uuid, Vec<EntityKey>)>>,
    /// Rewrites the next batch resolution's answer.
    resolution_tamper: Mutex<Option<ResolutionTamper>>,
}

type ResolutionTamper = Box<dyn FnOnce(&mut HashMap<EntityKey, Option<EntityIdentity>>) + Send>;

impl TenantRegistry {
    fn hides(&self, ctx: &SecurityContext) -> bool {
        let hidden = self.hidden.lock().expect("hidden");
        hidden.contains(&ctx.subject_tenant_id()) || hidden.contains(&ctx.subject_id())
    }

    fn new(hidden: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            registry: MockTypesRegistry::new(),
            global: Mutex::new(HashSet::new()),
            labels: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            hidden: Mutex::new(hidden.into_iter().collect()),
            hidden_delay: Mutex::new(Duration::ZERO),
            misreported: Mutex::new(HashMap::new()),
            resolutions: Mutex::new(Vec::new()),
            resolution_tamper: Mutex::new(None),
        }
    }

    fn resolutions(&self) -> Vec<(Uuid, Vec<EntityKey>)> {
        self.resolutions.lock().expect("resolutions").clone()
    }

    /// Seeds `id` as a global entity.
    fn seed_global(&self, id: &str, content: serde_json::Value) {
        self.registry.seed(id, content);
        self.global
            .lock()
            .expect("global")
            .insert(gts::GtsId::try_new(id).expect("identifier").to_uuid());
    }

    fn set_ownership(&self, ctx: &SecurityContext, entity: &mut Entity) {
        entity.ownership = if self
            .global
            .lock()
            .expect("global")
            .contains(&entity.gts_uuid)
        {
            Ownership::Global
        } else {
            self.labels
                .lock()
                .expect("labels")
                .get(&ctx.subject_tenant_id())
                .copied()
                .unwrap_or(Ownership::ContextTenant)
        };
    }
}

#[async_trait]
impl TypesRegistryApi for TenantRegistry {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.calls
            .lock()
            .expect("calls")
            .extend(request.items.iter().map(|item| {
                (
                    ctx.subject_tenant_id(),
                    item.if_none_match_for(&request.projection).cloned(),
                )
            }));
        if self.hides(ctx) {
            let delay = *self.hidden_delay.lock().expect("delay");
            tokio::time::sleep(delay).await;
            return Ok(BatchGetEntitiesResponse(
                request
                    .items
                    .into_iter()
                    .map(|item| (item.key, EntityLookup::NotFound))
                    .collect(),
            ));
        }
        let mut response =
            TypesRegistryApi::batch_get_entities(&self.registry, ctx, request).await?;
        for lookup in response.0.values_mut() {
            if let EntityLookup::Found { entity, .. } = lookup {
                self.set_ownership(ctx, entity);
                if let Some(uuid) = self
                    .misreported
                    .lock()
                    .expect("misreported")
                    .get(entity.gts_id.as_ref())
                {
                    entity.gts_uuid = *uuid;
                }
            }
        }
        Ok(response)
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let mut page = TypesRegistryApi::list_entities(&self.registry, ctx, request).await?;
        for entity in &mut page.items {
            self.set_ownership(ctx, entity);
        }
        Ok(page)
    }

    async fn batch_resolve_identities(
        &self,
        ctx: &SecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        self.resolutions
            .lock()
            .expect("resolutions")
            .push((ctx.subject_tenant_id(), keys.clone()));
        if self.hides(ctx) {
            return Ok(keys.into_iter().map(|key| (key, None)).collect());
        }
        let mut answer =
            TypesRegistryApi::batch_resolve_identities(&self.registry, ctx, keys).await?;
        if let Some(tamper) = self.resolution_tamper.lock().expect("tamper").take() {
            tamper(&mut answer);
        }
        Ok(answer)
    }
}

fn tenant(id: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(id + 100))
        .subject_tenant_id(Uuid::from_u128(id))
        .build()
        .expect("tenant context")
}

/// Builds a subject in tenant 1.
fn subject(subject: u128, bearer: Option<&str>) -> SecurityContext {
    let builder = SecurityContext::builder()
        .subject_id(Uuid::from_u128(subject))
        .subject_tenant_id(Uuid::from_u128(1));
    match bearer {
        Some(bearer) => builder.bearer_token(bearer.to_owned()),
        None => builder,
    }
    .build()
    .expect("tenant context")
}

/// Reads `TYPE` and returns the call log.
async fn read_as(
    cache: &CachedTypesRegistry,
    registry: &TenantRegistry,
    ctx: &SecurityContext,
) -> Vec<(Uuid, Option<Validator>)> {
    let response = cache
        .batch_get_entities(
            ctx,
            BatchGetEntitiesRequest {
                items: vec![gts_key(TYPE).into()],
                projection: full(),
                fresh: false,
            },
        )
        .await
        .expect("read");
    assert!(matches!(
        response.0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    registry.calls.lock().expect("calls").clone()
}

/// From P1 the registry scopes reads per subject by ownership constraints, so only the read context
/// that was answered reuses its confirmation. The snapshot itself is shared.
#[tokio::test(start_paused = true)]
async fn a_confirmation_serves_only_its_read_context() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let with = |subject_type: Option<&str>, scopes: &[&str]| {
        let builder = SecurityContext::builder()
            .subject_id(Uuid::from_u128(10))
            .subject_tenant_id(Uuid::from_u128(1))
            .token_scopes(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        match subject_type {
            Some(kind) => builder.subject_type(kind),
            None => builder,
        }
        .build()
        .expect("context")
    };

    let alice = subject(10, Some("alice"));
    assert_eq!(read_as(&cache, &registry, &alice).await.len(), 1);
    let EntityLookup::Found { etag, .. } = cache
        .batch_get_entities(&alice, BatchGetEntitiesRequest::new([gts_key(TYPE).into()]))
        .await
        .expect("hit")
        .0[&gts_key(TYPE)]
        .clone()
    else {
        panic!("found");
    };
    assert_eq!(registry.calls.lock().expect("calls").len(), 1, "a hit");
    assert_eq!(
        read_as(&cache, &registry, &subject(10, Some("renewed")))
            .await
            .len(),
        1,
        "another bearer with the same claims is the same read context"
    );

    // A tenant-owned snapshot is the tenant's own: another tenant reads its own.
    for (other, sent, why) in [
        (
            subject(11, Some("bob")),
            Some(&etag),
            "another subject of the tenant",
        ),
        (with(None, &["read"]), Some(&etag), "other token scopes"),
        (
            with(None, &["read", "write"]),
            Some(&etag),
            "more token scopes",
        ),
        (
            with(None, &["write", "read"]),
            Some(&etag),
            "the same scopes in another order",
        ),
        (
            with(Some("service"), &[]),
            Some(&etag),
            "another subject type",
        ),
        (
            SecurityContext::builder()
                .subject_id(Uuid::from_u128(10))
                .subject_tenant_id(Uuid::from_u128(2))
                .build()
                .expect("context"),
            None,
            "the same subject in another home tenant",
        ),
    ] {
        let before = registry.calls.lock().expect("calls").len();
        let calls = read_as(&cache, &registry, &other).await;
        assert_eq!(calls.len(), before + 1, "{why} revalidates");
        assert_eq!(
            calls[before],
            (other.subject_tenant_id(), sent.cloned()),
            "{why} revalidates its tenant's snapshot, if any"
        );
        assert_eq!(
            read_as(&cache, &registry, &other).await.len(),
            before + 1,
            "{why} then hits its own confirmation"
        );
    }
    let forwarded = registry.registry.caller_contexts(Call::BatchGet);
    assert_eq!(forwarded[0].subject_id(), Uuid::from_u128(10));
    assert!(
        forwarded[0].bearer_token().is_some(),
        "the miss reached the registry with its caller's own context"
    );
}

/// The cache keeps no record of refusals: a refused credential keeps earlier
/// confirmations of its read context until they expire. The host authenticates
/// every request, and per-entity denials arrive as `not_found`.
#[tokio::test(start_paused = true)]
async fn a_refusal_changes_no_confirmation() {
    for refusal in [
        CanonicalError::unauthenticated()
            .with_reason("TOKEN_INVALID")
            .create(),
        crate::gts::TypeResource::permission_denied()
            .with_reason("DENIED")
            .create(),
    ] {
        let registry = Arc::new(TenantRegistry::new([]));
        registry.registry.seed(TYPE, schema(TYPE, "v1"));
        registry
            .registry
            .inject(Fault::on(Call::BatchGet).nth(2).fail(refusal.clone()));
        registry
            .registry
            .inject(Fault::on(Call::BatchResolve).fail(refusal.clone()));
        let cache = CachedTypesRegistry::new(
            registry.clone(),
            CacheSettings {
                freshness_window: WINDOW,
                ..CacheSettings::default()
            },
        );
        let alice = subject(10, Some("alice"));
        read_as(&cache, &registry, &alice).await;
        let forced = BatchGetEntitiesRequest {
            fresh: true,
            ..BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full())
        };
        let error = cache
            .batch_get_entities(&alice, forced)
            .await
            .expect_err("refused");
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&refusal)
        );
        resolve(&cache, &alice, &[gts_key(OTHER)])
            .await
            .expect_err("refused");

        assert_eq!(read_as(&cache, &registry, &alice).await.len(), 2, "a hit");
        tokio::time::advance(WINDOW).await;
        assert_eq!(
            read_as(&cache, &registry, &alice).await.len(),
            3,
            "until the window ends"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn anonymous_calls_never_reuse_the_tenant_cache() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let anonymous = SecurityContext::anonymous();

    cache
        .list_entities(&anonymous, ListEntitiesRequest::default())
        .await
        .expect("page");
    assert_eq!(
        cache.cache.store().identities.peek_uuid(&gts_key(TYPE)),
        None,
        "anonymous discovery teaches no pair"
    );
    read_as(&cache, &registry, &subject(10, Some("alice"))).await;
    read_as(&cache, &registry, &anonymous).await;
    let calls = read_as(&cache, &registry, &anonymous).await;
    assert_eq!(
        calls[1..],
        [
            (anonymous.subject_tenant_id(), None),
            (anonymous.subject_tenant_id(), None)
        ],
        "unconditional reads straight from the registry"
    );
    resolve(&cache, &anonymous, &[gts_key(TYPE)])
        .await
        .expect("resolved");
    assert_eq!(registry.resolutions().len(), 1, "forwarded");
    assert_eq!(
        read_as(&cache, &registry, &subject(10, Some("alice")))
            .await
            .len(),
        3,
        "the subject's confirmation is still a hit"
    );
}

/// Each tenant reads a tenant-owned entity into its own snapshot, labelled for it, and
/// confirms its visibility independently; equal documents are held once.
#[tokio::test(start_paused = true)]
async fn tenants_keep_their_own_snapshots_and_confirm_visibility_independently() {
    let registry = Arc::new(TenantRegistry::new([Uuid::from_u128(2)]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry
        .labels
        .lock()
        .expect("labels")
        .insert(Uuid::from_u128(2), Ownership::AncestorTenant);
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            freshness_window: WINDOW,
            max_entries: 2,
            ..CacheSettings::default()
        },
    );
    let key = gts_key(TYPE);
    let read = |tenant, fresh| {
        let ctx = tenant;
        let key = key.clone();
        let cache = &cache;
        async move {
            let response = cache
                .batch_get_entities(
                    &ctx,
                    BatchGetEntitiesRequest {
                        items: vec![key.clone().into()],
                        projection: full(),
                        fresh,
                    },
                )
                .await
                .expect("read");
            response.0[&key].clone()
        }
    };
    let calls = || registry.calls.lock().expect("calls").clone();

    let first = read(tenant(1), false).await;
    let EntityLookup::Found { ref etag, .. } = first else {
        panic!("tenant 1 can see the entity");
    };
    assert_eq!(read(tenant(2), false).await, EntityLookup::NotFound);
    assert_eq!(read(tenant(1), false).await, first);
    assert_eq!(calls().len(), 2);

    registry.hidden.lock().expect("hidden").clear();
    let second = read(tenant(2), false).await;
    let EntityLookup::Found { ref entity, .. } = second else {
        panic!("tenant 2 can see the entity now");
    };
    assert_eq!(entity.ownership, Ownership::AncestorTenant);
    assert_eq!(read(tenant(2), false).await, second);
    assert_eq!(read(tenant(1), false).await, first);
    assert_eq!(
        calls(),
        [
            (Uuid::from_u128(1), None),
            (Uuid::from_u128(2), None),
            (Uuid::from_u128(2), None),
        ],
        "tenant 2 never sent tenant 1's validator"
    );
    {
        let store = cache.cache.store();
        assert_eq!(
            store.scopes(&key),
            [
                Scope::Tenant(Uuid::from_u128(1)),
                Scope::Tenant(Uuid::from_u128(2))
            ]
        );
        let held = |tenant| {
            store
                .peek(&key, Some(Uuid::from_u128(tenant)))
                .expect("snapshot")
                .snapshot
                .documents()[0]
                .clone()
                .expect("content")
        };
        assert!(Arc::ptr_eq(&held(1), &held(2)), "one content document");
    }

    registry
        .hidden
        .lock()
        .expect("hidden")
        .insert(Uuid::from_u128(1));
    assert_eq!(read(tenant(1), true).await, EntityLookup::NotFound);
    assert_eq!(read(tenant(1), false).await, EntityLookup::NotFound);
    assert_eq!(read(tenant(2), false).await, second);
    assert_eq!(calls().len(), 5);
    let resent = (Uuid::from_u128(1), Some(etag.clone()));
    assert_eq!(calls()[3..], [resent.clone(), resent]);

    tokio::time::advance(WINDOW).await;
    assert_eq!(read(tenant(2), false).await, second);
    assert_eq!(calls()[5..], [(Uuid::from_u128(2), Some(etag.clone()))]);
}

#[tokio::test(start_paused = true)]
async fn a_tenant_read_releases_its_context_slot_when_done_or_dropped() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(TYPE, schema(TYPE, "v1"));
    registry.inject(Fault::on(Call::BatchGet).delay(Duration::from_secs(1)));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let caller = tenant(1);
    let context = CachedTypesRegistry::context(&caller).expect("authenticated");
    let in_flight = || cache.cache.store().context_in_flight(&context);
    let request = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
    {
        let read = std::pin::pin!(cache.batch_get_entities(&caller, request.clone()));
        tokio::select! {
            _ = read => panic!("held"),
            () = tokio::task::yield_now() => {}
        }
        assert!(in_flight(), "the read holds a slot");
    }
    assert!(!in_flight(), "dropping the read released it");
    cache
        .batch_get_entities(&caller, request)
        .await
        .expect("read");
    assert!(!in_flight(), "a completed read released it");
}

/// A `not_found` revokes and fences only the read context it answered, whether
/// the other context is the same subject in another tenant or another subject
/// of the same tenant.
#[tokio::test(start_paused = true)]
async fn an_absent_read_fences_only_its_read_context() {
    let same_subject_in_tenant_2 = SecurityContext::builder()
        .subject_id(tenant(1).subject_id())
        .subject_tenant_id(Uuid::from_u128(2))
        .build()
        .expect("tenant context");
    for (denied, hide, same) in [
        (tenant(1), tenant(1).subject_id(), true),
        (
            same_subject_in_tenant_2.clone(),
            same_subject_in_tenant_2.subject_tenant_id(),
            false,
        ),
        (subject(102, None), subject(102, None).subject_id(), false),
    ] {
        let registry = Arc::new(TenantRegistry::new([]));
        registry.registry.seed(TYPE, schema(TYPE, "v1"));
        registry.registry.inject(
            Fault::on(Call::BatchGet)
                .nth(1)
                .delay(Duration::from_secs(1)),
        );
        let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
        let key = gts_key(TYPE);
        let request = BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(full());
        let allowed = tenant(1);
        let (visible, hidden) =
            tokio::join!(cache.batch_get_entities(&allowed, request.clone()), async {
                tokio::task::yield_now().await;
                registry.hidden.lock().expect("hidden").insert(hide);
                cache.batch_get_entities(&denied, request.clone()).await
            });
        let visible = visible.expect("found");
        assert!(matches!(visible.0[&key], EntityLookup::Found { .. }));
        assert_eq!(hidden.expect("hidden").0[&key], EntityLookup::NotFound);
        let next = cache
            .batch_get_entities(&allowed, request)
            .await
            .expect("next read");
        if same {
            assert_eq!(next.0[&key], EntityLookup::NotFound);
            assert_eq!(registry.calls.lock().expect("calls").len(), 3);
        } else {
            assert_eq!(next, visible);
            assert_eq!(registry.calls.lock().expect("calls").len(), 2);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn widening_the_projection_keeps_other_subjects_confirmed_under_the_same_etag() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let key = gts_key(TYPE);
    let request = BatchGetEntitiesRequest::new([key.clone().into()]);
    let narrow = request.clone().with_projection(content_only());
    let first = cache
        .batch_get_entities(&tenant(1), narrow.clone())
        .await
        .expect("narrow read");
    let wide_request = request.with_projection(full());
    let colleague = subject(11, None);
    let wide = cache
        .batch_get_entities(&colleague, wide_request.clone())
        .await
        .expect("wide read");
    let (
        EntityLookup::Found {
            entity: owned,
            etag: before,
        },
        EntityLookup::Found { etag: after, .. },
    ) = (&first.0[&key], &wide.0[&key])
    else {
        panic!("both subjects see the entity");
    };
    assert_eq!(
        owned.ownership,
        Ownership::ContextTenant,
        "a global snapshot would bypass the confirmation under test"
    );
    assert_eq!(before, after);
    assert_eq!(
        cache
            .batch_get_entities(&tenant(1), narrow)
            .await
            .expect("cached read"),
        first
    );
    assert_eq!(
        cache
            .batch_get_entities(&colleague, wide_request)
            .await
            .expect("cached wide read"),
        wide
    );
    assert_eq!(
        registry.calls.lock().expect("calls").as_slice(),
        [(Uuid::from_u128(1), None), (Uuid::from_u128(1), None)],
        "a narrow snapshot pins no wider read; the widened one was kept and the first \
         subject's confirmation survived"
    );
}

#[tokio::test(start_paused = true)]
async fn evicting_a_confirmation_leaves_the_shared_snapshot_for_revalidation() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 1,
            ..CacheSettings::default()
        },
    );
    let key = gts_key(TYPE);
    let request = BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(full());
    let first = cache
        .batch_get_entities(&tenant(1), request.clone())
        .await
        .expect("fill");
    let EntityLookup::Found { etag, .. } = &first.0[&key] else {
        panic!("snapshot");
    };
    // Two subjects of one tenant share its snapshot; one confirmation slot holds either.
    for reader in [subject(11, None), tenant(1), tenant(1)] {
        assert_eq!(
            cache
                .batch_get_entities(&reader, request.clone())
                .await
                .expect("read"),
            first
        );
    }
    let revalidated = (Uuid::from_u128(1), Some(etag.clone()));
    assert_eq!(
        registry.calls.lock().expect("calls").as_slice(),
        &[(Uuid::from_u128(1), None), revalidated.clone(), revalidated]
    );
}

/// Reads `TYPE` under `projection` as `ctx`, expecting it found.
async fn found_as(
    cache: &CachedTypesRegistry,
    ctx: &SecurityContext,
    projection: Projection,
) -> (Entity, Validator) {
    let key = gts_key(TYPE);
    let response = cache
        .batch_get_entities(
            ctx,
            BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(projection),
        )
        .await
        .expect("read");
    match response.0[&key].clone() {
        EntityLookup::Found { entity, etag } => (*entity, etag),
        other => panic!("found, not {other:?}"),
    }
}

/// An owner and its descendant tenant, labelled for each, under one validator: the
/// registry's P0 validator has no tenant input.
fn owner_and_descendant() -> (Arc<TenantRegistry>, CachedTypesRegistry) {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry
        .labels
        .lock()
        .expect("labels")
        .insert(Uuid::from_u128(2), Ownership::AncestorTenant);
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    (registry, cache)
}

/// `ownership` is relative to the reader's tenant, so a tenant-owned snapshot read for
/// one tenant is never served to, pinned by, or confirmed for another — even when the
/// registry answers both under one validator.
#[tokio::test(start_paused = true)]
async fn a_tenant_owned_snapshot_answers_only_the_tenant_it_was_read_for() {
    let (registry, cache) = owner_and_descendant();
    let (owned, etag) = found_as(&cache, &tenant(1), full()).await;
    assert_eq!(owned.ownership, Ownership::ContextTenant);
    let (inherited, same) = found_as(&cache, &tenant(2), full()).await;
    assert_eq!(
        same, etag,
        "the case under test: one validator for both tenants"
    );
    assert_eq!(inherited.ownership, Ownership::AncestorTenant);
    for _ in 0..2 {
        assert_eq!(found_as(&cache, &tenant(1), full()).await.0, owned);
        assert_eq!(found_as(&cache, &tenant(2), full()).await.0, inherited);
    }
    assert_eq!(
        registry.calls.lock().expect("calls").as_slice(),
        &[(Uuid::from_u128(1), None), (Uuid::from_u128(2), None)],
        "the descendant never revalidated the owner's snapshot, and both then hit their own"
    );
}

/// A wider read for one tenant does not merge into another tenant's snapshot, nor
/// replace it.
#[tokio::test(start_paused = true)]
async fn a_wider_read_never_merges_into_another_tenants_snapshot() {
    let (registry, cache) = owner_and_descendant();
    let (narrow, _) = found_as(&cache, &tenant(1), content_only()).await;
    let (wide, _) = found_as(&cache, &tenant(2), full()).await;
    assert_eq!(wide.ownership, Ownership::AncestorTenant);
    assert_eq!(found_as(&cache, &tenant(1), content_only()).await.0, narrow);
    assert_eq!(found_as(&cache, &tenant(2), full()).await.0, wide);
    assert_eq!(
        registry.calls.lock().expect("calls").as_slice(),
        &[(Uuid::from_u128(1), None), (Uuid::from_u128(2), None)],
    );
}

/// Tenants' snapshots of one entity share equal documents: the second costs only its
/// own metadata and validator.
#[tokio::test(start_paused = true)]
async fn tenant_snapshots_of_one_entity_share_their_documents() {
    let (_, cache) = owner_and_descendant();
    found_as(&cache, &tenant(1), full()).await;
    let one = cache.cache.store().used();
    let (_, etag) = found_as(&cache, &tenant(2), full()).await;
    let two = cache.cache.store().used();
    assert_eq!(cache.cache.store().usage().snapshots, 2);
    assert_eq!(
        two - one,
        TYPE.len() + 256 + etag.as_bytes().len(),
        "the documents are charged once"
    );
}

/// Two tenants' first reads race: the later reply finds no snapshot of its own and
/// leaves the other tenant's fresh one in place.
#[tokio::test(start_paused = true)]
async fn a_late_cold_fill_leaves_another_tenants_fresh_snapshot() {
    let (registry, cache) = owner_and_descendant();
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(1)),
    );
    let owner = tenant(1);
    let (first, ()) = tokio::join!(found_as(&cache, &owner, full()), async {
        tokio::task::yield_now().await;
        found_as(&cache, &tenant(2), full()).await;
    });
    assert_eq!(first.0.ownership, Ownership::ContextTenant);
    assert_eq!(
        found_as(&cache, &tenant(2), full()).await.0.ownership,
        Ownership::AncestorTenant
    );
    assert_eq!(
        registry.calls.lock().expect("calls").len(),
        2,
        "tenant 2's snapshot survived tenant 1's late reply"
    );
}

// batch resolution.

fn resolve<'a>(
    cache: &'a CachedTypesRegistry,
    ctx: &'a SecurityContext,
    keys: &[EntityKey],
) -> impl std::future::Future<
    Output = Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError>,
> + 'a {
    cache.batch_resolve_identities(ctx, keys.to_vec())
}

fn identity(id: &str) -> EntityIdentity {
    let gts_id = gts::GtsId::try_new(id).expect("identifier");
    let gts_uuid = gts_id.to_uuid();
    EntityIdentity { gts_id, gts_uuid }
}

/// An empty or over-long resolution goes to the registry whole, known keys and
/// repeats included, so the registry refuses it rather than the cache answering
/// part of it.
#[tokio::test(start_paused = true)]
async fn an_over_long_or_empty_tenant_resolution_is_sent_whole_for_the_registry_to_refuse() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let reader = tenant(1);
    resolve(&cache, &reader, &[gts_key(TYPE)])
        .await
        .expect("known");
    let mut keys = vec![gts_key(TYPE); crate::models::MAX_BATCH_GET_KEYS];
    keys.push(gts_key(OTHER));
    for keys in [keys, Vec::new()] {
        let error = resolve(&cache, &reader, &keys).await.expect_err("refused");
        assert!(
            matches!(error, CanonicalError::InvalidArgument { .. }),
            "{error:?}"
        );
        let sent = registry.resolutions().last().map(|(_, sent)| sent.clone());
        assert_eq!(sent, Some(keys));
    }
}

/// A fresh read resolves either key. Other read contexts and expired confirmations
/// go to the registry, and a registry resolution shows the pair to its context.
#[tokio::test(start_paused = true)]
async fn only_a_freshly_confirmed_context_resolves_from_the_resident_snapshot() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let request = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
    cache
        .batch_get_entities(&tenant(1), request)
        .await
        .expect("warm");

    let keys = [gts_key(TYPE), uuid_key(TYPE)];
    let answered = resolve(&cache, &tenant(1), &keys).await.expect("resident");
    assert_eq!(answered[&keys[0]], Some(identity(TYPE)));
    assert_eq!(answered[&keys[1]], Some(identity(TYPE)));
    assert!(registry.resolutions().is_empty());

    for _ in 0..2 {
        let answered = resolve(&cache, &tenant(2), &keys).await.expect("resolved");
        assert_eq!(answered[&keys[0]], Some(identity(TYPE)));
        assert_eq!(answered[&keys[1]], Some(identity(TYPE)));
    }
    tokio::time::advance(WINDOW).await;
    resolve(&cache, &tenant(1), &keys[..1])
        .await
        .expect("resolved");
    assert_eq!(
        registry.resolutions(),
        [
            (Uuid::from_u128(2), keys.to_vec()),
            (Uuid::from_u128(1), vec![gts_key(TYPE)]),
        ]
    );
    assert_eq!(registry.calls.lock().expect("calls").len(), 1, "no read");
}

/// A resolution miss fences that read context's in-flight confirmation while
/// another one still uses the shared snapshot.
#[tokio::test(start_paused = true)]
async fn a_resolution_absence_fences_only_its_contexts_confirmation() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(1)),
    );
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let key = gts_key(TYPE);
    let request = BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(full());
    let confirmed = tenant(1);
    let (read, resolved) = tokio::join!(
        cache.batch_get_entities(&confirmed, request.clone()),
        async {
            tokio::task::yield_now().await;
            registry
                .hidden
                .lock()
                .expect("hidden")
                .insert(confirmed.subject_tenant_id());
            resolve(&cache, &confirmed, std::slice::from_ref(&key)).await
        }
    );
    assert!(matches!(
        read.expect("found").0[&key],
        EntityLookup::Found { .. }
    ));
    assert_eq!(resolved.expect("resolved")[&key], None);
    assert!(
        cache.cache.store().peek_any(&key).is_some(),
        "the snapshot stays"
    );

    let again = cache
        .batch_get_entities(&confirmed, request.clone())
        .await
        .expect("read");
    assert_eq!(again.0[&key], EntityLookup::NotFound);
    assert_eq!(registry.calls.lock().expect("calls").len(), 2);

    let other = tenant(2);
    for _ in 0..2 {
        cache
            .batch_get_entities(&other, request.clone())
            .await
            .expect("visible");
    }
    assert_eq!(
        registry.calls.lock().expect("calls").len(),
        3,
        "the other tenant confirms the resident snapshot once"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_context_s_resolution_leaves_the_snapshot_s_recency() {
    // Tenant-owned: a global pair would resolve for any reader and promote it.
    let registry = Arc::new(TenantRegistry::new([]));
    for id in [TYPE, OTHER, THIRD] {
        registry.registry.seed(id, schema(id, "v1"));
    }
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 2,
            // A remembered pair would let the probe confirm and take a slot.
            max_identity_entries: 0,
            ..CacheSettings::default()
        },
    );
    let read = |id: &str| {
        let request = BatchGetEntitiesRequest::new([gts_key(id).into()]).with_projection(full());
        let cache = &cache;
        async move {
            cache
                .batch_get_entities(&tenant(1), request)
                .await
                .expect("read")
        }
    };
    read(TYPE).await;
    read(OTHER).await;
    resolve(&cache, &tenant(2), &[gts_key(TYPE)])
        .await
        .expect("resolved");
    read(THIRD).await;
    assert_eq!(registry.registry.calls(Call::BatchGet), 3);

    read(OTHER).await;
    assert_eq!(
        registry.registry.calls(Call::BatchGet),
        3,
        "the more recent snapshot stayed"
    );
    read(TYPE).await;
    assert_eq!(
        registry.registry.calls(Call::BatchGet),
        4,
        "the probed one was evicted"
    );
}

/// A registry resolution shows the pair to its read context only, for the window,
/// without a resident snapshot.
#[tokio::test(start_paused = true)]
async fn a_resolution_shows_the_pair_to_its_read_context_only() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let keys = [gts_key(TYPE), uuid_key(TYPE)];
    let alice = subject(10, Some("alice"));
    for _ in 0..2 {
        let answered = resolve(&cache, &alice, &keys).await.expect("resolved");
        assert_eq!(answered[&keys[0]], Some(identity(TYPE)));
        assert_eq!(answered[&keys[1]], Some(identity(TYPE)));
    }
    assert!(
        cache.cache.store().peek_any(&keys[0]).is_none(),
        "no snapshot"
    );
    resolve(&cache, &subject(11, None), &keys[..1])
        .await
        .expect("resolved");
    tokio::time::advance(WINDOW).await;
    resolve(&cache, &alice, &keys[1..]).await.expect("resolved");
    assert_eq!(
        registry.resolutions(),
        [
            (Uuid::from_u128(1), keys.to_vec()),
            (Uuid::from_u128(1), vec![gts_key(TYPE)]),
            (Uuid::from_u128(1), vec![uuid_key(TYPE)]),
        ]
    );
    assert_eq!(
        read_as(&cache, &registry, &alice).await.len(),
        1,
        "a resolution confirms no read"
    );
}

/// A resolution answering other keys than asked fails whole: it teaches the read
/// context neither the valid pair beside the malformed key nor its visibility.
#[tokio::test(start_paused = true)]
async fn a_malformed_resolution_fails_and_teaches_the_context_nothing() {
    type Tamper = fn(&mut HashMap<EntityKey, Option<EntityIdentity>>);
    let rows: [(&str, Tamper); 3] = [
        ("missing", |answer| {
            answer.remove(&gts_key(OTHER));
        }),
        ("extra", |answer| {
            answer.insert(gts_key(THIRD), Some(identity(THIRD)));
        }),
        ("substituted", |answer| {
            answer.remove(&gts_key(OTHER));
            answer.insert(gts_key(THIRD), Some(identity(THIRD)));
        }),
    ];
    for (row, tamper) in rows {
        let registry = Arc::new(TenantRegistry::new([]));
        for id in [TYPE, OTHER, THIRD] {
            registry.registry.seed(id, schema(id, "v1"));
        }
        *registry.resolution_tamper.lock().expect("tamper") = Some(Box::new(tamper));
        let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
        let alice = subject(10, Some("alice"));
        let error = resolve(&cache, &alice, &[gts_key(TYPE), gts_key(OTHER)])
            .await
            .expect_err(row);
        assert!(matches!(error, CanonicalError::Internal { .. }), "{row}");
        assert_eq!(
            cache.cache.store().usage().identities,
            0,
            "{row}: no pair was learned"
        );
        let answered = resolve(&cache, &alice, &[gts_key(TYPE), uuid_key(TYPE)])
            .await
            .expect(row);
        assert_eq!(answered[&uuid_key(TYPE)], Some(identity(TYPE)), "{row}");
        assert_eq!(
            registry.resolutions().len(),
            2,
            "{row}: the valid pair beside the malformed key was not learned"
        );
    }
}

/// A read shows its context the pair too, which outlives the read's snapshot.
#[tokio::test(start_paused = true)]
async fn a_read_lets_its_context_resolve_after_the_snapshot_changes() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let alice = subject(10, Some("alice"));
    read_as(&cache, &registry, &alice).await;

    registry.registry.seed(TYPE, schema(TYPE, "v2"));
    let forced = BatchGetEntitiesRequest {
        fresh: true,
        ..BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full())
    };
    cache
        .batch_get_entities(&subject(11, None), forced)
        .await
        .expect("replaced");
    let answered = resolve(&cache, &alice, &[uuid_key(TYPE)])
        .await
        .expect("resolved");
    assert_eq!(answered[&uuid_key(TYPE)], Some(identity(TYPE)));
    assert!(registry.resolutions().is_empty(), "the pair was shown");
    assert_eq!(
        read_as(&cache, &registry, &alice).await.len(),
        3,
        "the replaced snapshot was not"
    );
}

/// An evicted pair takes its resolution proof with it, so relearning the pair
/// for another context cannot revive a proof whose revocation found no UUID.
#[tokio::test(start_paused = true)]
async fn an_evicted_pair_retires_its_resolution_proof() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry.registry.seed(OTHER, schema(OTHER, "v1"));
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_identity_entries: 1,
            ..CacheSettings::default()
        },
    );
    let alice = subject(10, Some("alice"));
    let bob = subject(11, None);
    resolve(&cache, &alice, &[gts_key(TYPE)])
        .await
        .expect("shown");
    resolve(&cache, &bob, &[gts_key(OTHER)])
        .await
        .expect("evicts TYPE");

    registry
        .hidden
        .lock()
        .expect("hidden")
        .insert(alice.subject_id());
    let denied = resolve(&cache, &alice, &[gts_key(TYPE)])
        .await
        .expect("denied");
    assert_eq!(denied[&gts_key(TYPE)], None);
    resolve(&cache, &bob, &[gts_key(TYPE)])
        .await
        .expect("relearned");
    let again = resolve(&cache, &alice, &[uuid_key(TYPE)])
        .await
        .expect("denied");
    assert_eq!(again[&uuid_key(TYPE)], None);
    assert_eq!(
        registry.resolutions().len(),
        5,
        "every resolution reached the registry"
    );
}

/// A `not_found` for one context fences its resolutions in flight, whichever
/// method and key spelling reported it.
#[tokio::test(start_paused = true)]
async fn an_absence_fences_a_resolution_in_flight() {
    for by_read in [false, true] {
        let registry = Arc::new(TenantRegistry::new([]));
        registry.registry.seed(TYPE, schema(TYPE, "v1"));
        registry.registry.inject(
            Fault::on(Call::BatchResolve)
                .nth(1)
                .delay(Duration::from_secs(1)),
        );
        let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
        let alice = subject(10, Some("alice"));
        let (held, absent) = tokio::join!(resolve(&cache, &alice, &[gts_key(TYPE)]), async {
            tokio::task::yield_now().await;
            registry
                .hidden
                .lock()
                .expect("hidden")
                .insert(alice.subject_id());
            if by_read {
                let request = BatchGetEntitiesRequest::new([uuid_key(TYPE).into()]);
                let answer = cache
                    .batch_get_entities(&alice, request)
                    .await
                    .expect("read");
                answer.0[&uuid_key(TYPE)] == EntityLookup::NotFound
            } else {
                let answer = resolve(&cache, &alice, &[uuid_key(TYPE)])
                    .await
                    .expect("resolved");
                answer[&uuid_key(TYPE)].is_none()
            }
        });
        assert_eq!(held.expect("held")[&gts_key(TYPE)], Some(identity(TYPE)));
        assert!(absent, "by_read={by_read}");
        let next = resolve(&cache, &alice, &[gts_key(TYPE)])
            .await
            .expect("resolved");
        assert_eq!(next[&gts_key(TYPE)], None, "by_read={by_read}");
    }
}

/// A read shows its pair even when its snapshot is too large to keep.
#[tokio::test(start_paused = true)]
async fn a_read_too_large_to_keep_still_shows_its_pair() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_size_bytes: 0,
            ..CacheSettings::default()
        },
    );
    let alice = subject(10, Some("alice"));
    read_as(&cache, &registry, &alice).await;
    assert!(
        cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
        "not kept"
    );
    let answered = resolve(&cache, &alice, &[uuid_key(TYPE)])
        .await
        .expect("resolved");
    assert_eq!(answered[&uuid_key(TYPE)], Some(identity(TYPE)));
    assert!(registry.resolutions().is_empty(), "shown by the read");
}

/// An `Unchanged` to a pinned snapshot shows its pair even if the snapshot is
/// evicted while the read is in flight.
#[tokio::test(start_paused = true)]
async fn a_confirmed_pin_evicted_in_flight_still_shows_its_pair() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry.registry.seed(OTHER, schema(OTHER, "v1"));
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(2)
            .delay(Duration::from_secs(1)),
    );
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 1,
            ..CacheSettings::default()
        },
    );
    let bob = subject(11, None);
    let alice = subject(10, Some("alice"));
    read_as(&cache, &registry, &bob).await;
    let request =
        |id: &str| BatchGetEntitiesRequest::new([gts_key(id).into()]).with_projection(full());
    let (pinned, _) = tokio::join!(cache.batch_get_entities(&alice, request(TYPE)), async {
        tokio::task::yield_now().await;
        cache
            .batch_get_entities(&bob, request(OTHER))
            .await
            .expect("evicts TYPE")
    });
    assert!(matches!(
        pinned.expect("confirmed").0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    assert!(
        cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
        "evicted"
    );
    let answered = resolve(&cache, &alice, &[uuid_key(TYPE)])
        .await
        .expect("resolved");
    assert_eq!(answered[&uuid_key(TYPE)], Some(identity(TYPE)));
    assert!(
        registry.resolutions().is_empty(),
        "shown by the confirmed read"
    );
}

/// Discovery is a `list`: it teaches pairs but shows them to no read context.
#[tokio::test(start_paused = true)]
async fn discovery_teaches_pairs_without_showing_them() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let alice = subject(10, Some("alice"));
    let page = cache
        .list_entities(&alice, ListEntitiesRequest::default())
        .await
        .expect("page");
    assert!(
        page.items
            .iter()
            .any(|entity| entity.gts_id.as_ref() == TYPE)
    );
    assert_eq!(
        cache.cache.store().identities.peek_uuid(&gts_key(TYPE)),
        Some(identity(TYPE).gts_uuid),
        "learned"
    );
    resolve(&cache, &alice, &[gts_key(TYPE)])
        .await
        .expect("resolved");
    assert_eq!(registry.resolutions().len(), 1, "not shown by discovery");
}

/// A resolution cannot pair a resident reference with another ID.
#[tokio::test(start_paused = true)]
async fn a_tenant_resolution_contradicting_a_resident_snapshot_fails() {
    let registry = Arc::new(MockTypesRegistry::new());
    let tail = "7a1d2f34-5678-49ab-9012-abcdef123456";
    let registered = format!("{TYPE}{tail}");
    let alias = format!("{OTHER}{tail}");
    registry.seed(&registered, json!({}));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let request =
        BatchGetEntitiesRequest::new([gts_key(&registered).into()]).with_projection(full());
    cache
        .batch_get_entities(&tenant(1), request)
        .await
        .expect("warm");
    // The fake answers the alias with the Registry Reference its identifier derives,
    // which the resident snapshot already carries for another identifier.
    registry.seed(&alias, json!({}));
    let error = resolve(&cache, &tenant(1), &[gts_key(&alias)])
        .await
        .expect_err("two identifiers for one Registry Reference");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

// Global entities, readable by every authenticated subject.

const ABSENT: &str = "gts.cf.cache.client.absent.v1~";

fn global_registry() -> Arc<TenantRegistry> {
    let registry = Arc::new(TenantRegistry::new([]));
    for id in [TYPE, OTHER, THIRD] {
        registry.seed_global(id, schema(id, "v1"));
    }
    registry
}

async fn get(
    cache: &CachedTypesRegistry,
    ctx: &SecurityContext,
    ids: &[&str],
) -> BatchGetEntitiesResponse {
    let request = BatchGetEntitiesRequest::new(ids.iter().map(|id| gts_key(id).into()))
        .with_projection(full());
    cache.batch_get_entities(ctx, request).await.expect("read")
}

fn read_keys(registry: &TenantRegistry) -> usize {
    registry.calls.lock().expect("calls").len()
}

/// A tenant-owned snapshot still needs the reader's own confirmation.
#[tokio::test(start_paused = true)]
async fn a_tenant_owned_snapshot_still_needs_its_own_confirmation() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.seed_global(TYPE, schema(TYPE, "v1"));
    registry.registry.seed(OTHER, schema(OTHER, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let (alice, bob) = (subject(10, Some("a")), subject(11, None));
    get(&cache, &alice, &[TYPE]).await;
    get(&cache, &bob, &[OTHER]).await;
    resolve(&cache, &alice, &[gts_key(OTHER)])
        .await
        .expect("resolved");
    assert_eq!(registry.resolutions().len(), 1);
    get(&cache, &alice, &[OTHER]).await;
    assert_eq!(read_keys(&registry), 3);
}

/// A pair resolves by its class only once an entity-carrying reply showed it global;
/// a bare resolution leaves it unknown.
#[tokio::test(start_paused = true)]
async fn only_an_entity_carrying_reply_classifies_a_pair() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let (alice, bob, dave) = (subject(10, Some("a")), subject(11, None), subject(13, None));
    get(&cache, &alice, &[TYPE]).await;
    get(&cache, &dave, &[TYPE]).await;
    resolve(&cache, &bob, &[gts_key(OTHER)])
        .await
        .expect("bare");
    resolve(&cache, &alice, &[gts_key(OTHER)])
        .await
        .expect("unknown class");
    assert_eq!(
        registry.resolutions().len(),
        2,
        "a bare resolution classified nothing"
    );

    cache
        .list_entities(&bob, ListEntitiesRequest::default())
        .await
        .expect("page");
    resolve(&cache, &dave, &[gts_key(OTHER)])
        .await
        .expect("by class");
    assert_eq!(registry.resolutions().len(), 2, "discovery classified it");
}

/// The ownership of an admitted entity never changes, so a reply contradicting it is
/// refused rather than learned.
#[tokio::test(start_paused = true)]
async fn a_contradictory_ownership_is_refused() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let bob = subject(11, None);
    get(&cache, &bob, &[OTHER]).await;
    registry
        .global
        .lock()
        .expect("global")
        .remove(&gts::GtsId::try_new(OTHER).expect("identifier").to_uuid());
    let error = cache
        .list_entities(&bob, ListEntitiesRequest::default())
        .await
        .expect_err("contradiction");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
    let diagnostic = error.diagnostic().expect("internal diagnostic");
    let other = gts::GtsId::try_new(OTHER).expect("identifier").to_uuid();
    assert!(
        diagnostic.contains(&format!(
            "{other} as tenant-owned, but a resident snapshot has it as global"
        )),
        "{diagnostic}"
    );
}

/// With no snapshot retained, the remembered pair's scope still refuses a
/// contradicting reply, and the diagnostic says that is where it is known.
#[tokio::test(start_paused = true)]
async fn a_contradictory_ownership_against_a_remembered_pair_is_refused() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 0,
            ..CacheSettings::default()
        },
    );
    let bob = subject(11, None);
    get(&cache, &bob, &[OTHER]).await;
    registry
        .global
        .lock()
        .expect("global")
        .remove(&gts::GtsId::try_new(OTHER).expect("identifier").to_uuid());
    let error = cache
        .list_entities(&bob, ListEntitiesRequest::default())
        .await
        .expect_err("contradiction");
    let diagnostic = error.diagnostic().expect("internal diagnostic");
    let other = gts::GtsId::try_new(OTHER).expect("identifier").to_uuid();
    assert!(
        diagnostic.contains(&format!(
            "{other} as tenant-owned, but the identity store has it as global"
        )),
        "{diagnostic}"
    );
}

/// Global answers keep no per-entity proofs, so they never evict a usable one.
#[tokio::test(start_paused = true)]
async fn global_answers_take_no_per_entity_slots() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry.seed_global(OTHER, schema(OTHER, "v1"));
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 2,
            ..CacheSettings::default()
        },
    );
    let alice = subject(10, Some("a"));
    get(&cache, &alice, &[TYPE]).await;
    for ctx in [subject(11, None), subject(12, None)] {
        get(&cache, &ctx, &[OTHER]).await;
    }
    let before = read_keys(&registry);
    get(&cache, &alice, &[TYPE]).await;
    assert_eq!(
        read_keys(&registry),
        before,
        "alice's own confirmation survived"
    );
}

/// Any authenticated reader may see a global entity, so a fresh global snapshot
/// serves every read context, whoever read it first, even after that reader's
/// absence for another key; anonymous calls still bypass.
#[tokio::test(start_paused = true)]
async fn any_authenticated_context_reads_a_fresh_global_snapshot() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let (alice, bob) = (subject(10, Some("a")), subject(11, None));
    get(&cache, &alice, &[TYPE]).await;
    get(&cache, &alice, &[ABSENT]).await;
    let served = get(&cache, &bob, &[TYPE]).await;
    assert!(matches!(
        served.0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    get(&cache, &tenant(5), &[TYPE]).await;
    get(&cache, &alice, &[TYPE]).await;
    assert_eq!(
        read_keys(&registry),
        2,
        "bob, another tenant and alice read alice's snapshot"
    );
    let resolved = resolve(&cache, &bob, &[uuid_key(TYPE)])
        .await
        .expect("resolved");
    assert_eq!(resolved[&uuid_key(TYPE)], Some(identity(TYPE)));
    assert!(
        registry.resolutions().is_empty(),
        "a known-global pair resolves for anyone"
    );
    get(&cache, &SecurityContext::anonymous(), &[TYPE]).await;
    assert_eq!(read_keys(&registry), 3, "anonymous calls bypass the cache");
}

/// Sharing never extends a snapshot's own freshness: a stale global snapshot is
/// revalidated, once, for whoever reads it next.
#[tokio::test(start_paused = true)]
async fn a_stale_global_snapshot_is_revalidated_for_its_next_reader() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            freshness_window: WINDOW,
            ..CacheSettings::default()
        },
    );
    get(&cache, &subject(10, Some("a")), &[TYPE]).await;
    tokio::time::advance(WINDOW).await;
    get(&cache, &subject(11, None), &[TYPE]).await;
    get(&cache, &subject(12, None), &[TYPE]).await;
    assert_eq!(
        read_keys(&registry),
        2,
        "one revalidation serves the next reader too"
    );
}

/// Every reader sees a global entity, so a tenant read's `not_found` for one is
/// everyone's absence: it drops the shared snapshot.
#[tokio::test(start_paused = true)]
async fn a_global_absence_drops_the_shared_snapshot() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let (alice, bob) = (subject(10, Some("a")), subject(11, None));
    get(&cache, &bob, &[TYPE]).await;
    registry
        .hidden
        .lock()
        .expect("hidden")
        .insert(alice.subject_id());
    let forced = BatchGetEntitiesRequest {
        fresh: true,
        ..BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full())
    };
    let absent = cache
        .batch_get_entities(&alice, forced)
        .await
        .expect("read");
    assert_eq!(absent.0[&gts_key(TYPE)], EntityLookup::NotFound);
    assert!(
        cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
        "dropped"
    );
    let before = read_keys(&registry);
    get(&cache, &bob, &[TYPE]).await;
    assert_eq!(read_keys(&registry), before + 1);
}

/// A global absence invalidates the entity even if its snapshot was evicted while
/// the read was in flight, so an older positive answer cannot refill it.
#[tokio::test(start_paused = true)]
async fn a_global_absence_fences_a_refill_after_an_eviction_in_flight() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 1,
            max_identity_entries: 0,
            ..CacheSettings::default()
        },
    );
    let (alice, bob, carol, dave) = (
        subject(10, Some("a")),
        subject(11, None),
        subject(12, None),
        subject(13, None),
    );
    // A light snapshot, so dave's full read is a fill, not a pinned confirmation.
    cache
        .batch_get_entities(&bob, BatchGetEntitiesRequest::new([gts_key(TYPE).into()]))
        .await
        .expect("light read");
    registry
        .hidden
        .lock()
        .expect("hidden")
        .insert(alice.subject_id());
    *registry.hidden_delay.lock().expect("delay") = Duration::from_secs(1);
    // The first read after injection is dave's, answered last.
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(2)),
    );
    let forced = || BatchGetEntitiesRequest {
        fresh: true,
        ..BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full())
    };
    let (_, absent, _) = tokio::join!(
        cache.batch_get_entities(&dave, forced()),
        async {
            tokio::task::yield_now().await;
            cache.batch_get_entities(&alice, forced()).await
        },
        async {
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            // Evicts TYPE while alice's and dave's reads are in flight.
            get(&cache, &carol, &[OTHER]).await
        }
    );
    assert_eq!(
        absent.expect("read").0[&gts_key(TYPE)],
        EntityLookup::NotFound
    );
    assert!(
        cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
        "dave's older answer did not refill TYPE"
    );
}

/// A reply contradicting the class of the snapshot its read started from is refused,
/// even after that snapshot was evicted.
#[tokio::test(start_paused = true)]
async fn a_reply_contradicting_its_evicted_snapshot_is_refused() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 1,
            max_identity_entries: 0,
            ..CacheSettings::default()
        },
    );
    let (bob, carol, dave) = (subject(11, None), subject(12, None), subject(13, None));
    cache
        .batch_get_entities(&bob, BatchGetEntitiesRequest::new([gts_key(TYPE).into()]))
        .await
        .expect("light read");
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(1)),
    );
    let full = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
    let (refused, ()) = tokio::join!(cache.batch_get_entities(&dave, full), async {
        tokio::task::yield_now().await;
        get(&cache, &carol, &[OTHER]).await;
        registry
            .global
            .lock()
            .expect("global")
            .remove(&gts::GtsId::try_new(TYPE).expect("identifier").to_uuid());
    });
    let error = refused.expect_err("contradiction");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

/// The class known when a read starts binds its reply even when the read had no
/// snapshot of its own: another tenant's, evicted in flight, still told the class.
#[tokio::test(start_paused = true)]
async fn a_reply_contradicting_another_tenants_evicted_snapshot_is_refused() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry.registry.seed(OTHER, schema(OTHER, "v1"));
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 1,
            max_identity_entries: 0,
            ..CacheSettings::default()
        },
    );
    get(&cache, &tenant(1), &[TYPE]).await;
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(1)),
    );
    let read = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
    let reader = tenant(2);
    let (refused, ()) = tokio::join!(cache.batch_get_entities(&reader, read), async {
        tokio::task::yield_now().await;
        get(&cache, &tenant(1), &[OTHER]).await;
        assert!(
            cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
            "evicted"
        );
        registry
            .global
            .lock()
            .expect("global")
            .insert(gts::GtsId::try_new(TYPE).expect("identifier").to_uuid());
    });
    let error = refused.expect_err("contradiction");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
    let diagnostic = error.diagnostic().expect("internal diagnostic");
    assert!(
        diagnostic.contains(&format!("answered {TYPE} with ")),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("as global, but the read started with it known as tenant-owned"),
        "{diagnostic}"
    );
}

/// The plan keeps what it knew at the start: evicting the remembered global pair while
/// the reads are in flight neither unclassifies the absence nor unfences either key.
#[tokio::test(start_paused = true)]
async fn an_absence_keeps_the_global_class_its_plan_knew() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 1,
            max_identity_entries: 2,
            ..CacheSettings::default()
        },
    );
    let (alice, bob, carol, dave) = (
        subject(10, Some("a")),
        subject(11, None),
        subject(12, None),
        subject(13, None),
    );
    get(&cache, &bob, &[TYPE]).await;
    get(&cache, &bob, &[OTHER]).await;
    registry
        .hidden
        .lock()
        .expect("hidden")
        .insert(alice.subject_id());
    *registry.hidden_delay.lock().expect("delay") = Duration::from_secs(1);
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(2)),
    );
    let (_, absent, ()) = tokio::join!(
        get(&cache, &dave, &[TYPE]),
        async {
            tokio::task::yield_now().await;
            cache
                .batch_get_entities(
                    &alice,
                    BatchGetEntitiesRequest::new([uuid_key(TYPE).into()]),
                )
                .await
        },
        async {
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            // Evicts TYPE's remembered pair while both reads are in flight.
            get(&cache, &carol, &[THIRD]).await;
        }
    );
    assert_eq!(
        absent.expect("read").0[&uuid_key(TYPE)],
        EntityLookup::NotFound
    );
    assert!(
        cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
        "dave's older answer did not refill TYPE"
    );
}

/// What becomes known meanwhile counts too: an absence planned before anything was
/// known about the entity drops the global snapshot another reader filled in flight.
#[tokio::test(start_paused = true)]
async fn an_absence_drops_a_snapshot_classified_global_in_flight() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let (alice, bob) = (subject(10, Some("a")), subject(11, None));
    registry
        .hidden
        .lock()
        .expect("hidden")
        .insert(alice.subject_id());
    *registry.hidden_delay.lock().expect("delay") = Duration::from_secs(1);
    let (absent, ()) = tokio::join!(
        cache.batch_get_entities(
            &alice,
            BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full()),
        ),
        async {
            tokio::task::yield_now().await;
            get(&cache, &bob, &[TYPE]).await;
            assert!(
                cache
                    .cache
                    .store()
                    .peek_any(&gts_key(TYPE))
                    .is_some_and(|entry| entry.snapshot.meta().ownership == Ownership::Global),
                "bob's global snapshot is resident before alice's absence settles"
            );
        }
    );
    assert_eq!(
        absent.expect("read").0[&gts_key(TYPE)],
        EntityLookup::NotFound
    );
    assert!(
        cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
        "a global entity's absence is everyone's"
    );
}

/// A reply naming the read's remembered identifier with another Registry Reference is
/// refused, even after the remembered pair was evicted in flight.
#[tokio::test(start_paused = true)]
async fn a_reply_contradicting_the_tickets_remembered_pair_is_refused() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_identity_entries: 1,
            ..CacheSettings::default()
        },
    );
    let (bob, carol, dave) = (subject(11, None), subject(12, None), subject(13, None));
    resolve(&cache, &bob, &[gts_key(TYPE)])
        .await
        .expect("remembers TYPE's pair");
    registry
        .misreported
        .lock()
        .expect("misreported")
        .insert(TYPE.to_owned(), Uuid::from_u128(99));
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(1)),
    );
    let full = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
    let (refused, _) = tokio::join!(cache.batch_get_entities(&dave, full), async {
        tokio::task::yield_now().await;
        // Evicts TYPE's pair while dave's read is in flight.
        resolve(&cache, &carol, &[gts_key(OTHER)]).await
    });
    let error = refused.expect_err("contradiction");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

/// A changed validator answering this reader's own condition is an observed change
/// even when its snapshot was evicted in flight: no tenant's older snapshot survives.
#[tokio::test(start_paused = true)]
async fn a_changed_validator_after_the_readers_snapshot_left_drops_every_tenants() {
    let registry = Arc::new(TenantRegistry::new([]));
    change(&registry.registry, TYPE, "old", 1);
    registry.registry.seed(OTHER, schema(OTHER, "other"));
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 2,
            ..CacheSettings::default()
        },
    );
    let (owner, descendant) = (tenant(1), tenant(2));
    let (_, old) = found_as(&cache, &owner, full()).await;
    let forced = BatchGetEntitiesRequest {
        fresh: true,
        ..BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full())
    };
    let context = CachedTypesRegistry::context(&owner).expect("authenticated");
    let reply = cache
        .cache
        .read(forced, Some(&context), |request| async {
            assert_eq!(
                request.items[0].if_none_match_for(&request.projection),
                Some(&old)
            );
            found_as(&cache, &descendant, full()).await;
            get(&cache, &descendant, &[OTHER]).await;
            assert!(
                cache
                    .cache
                    .store()
                    .peek(&gts_key(TYPE), Some(owner.subject_tenant_id()))
                    .is_none(),
                "the owner's snapshot left"
            );
            change(&registry.registry, TYPE, "new", 2);
            registry.batch_get_entities(&owner, request).await
        })
        .await
        .expect("read");
    let EntityLookup::Found { etag: new, .. } = &reply.0[&gts_key(TYPE)] else {
        panic!("found");
    };
    assert_ne!(&old, new);
    assert_eq!(
        found_as(&cache, &descendant, full()).await.1,
        *new,
        "the descendant's older snapshot went with the observed change"
    );
}

/// An authoritative global absence fences reads taken after an intervening event even
/// with nothing resident to drop, so an older positive reply cannot refill the entity.
#[tokio::test(start_paused = true)]
async fn a_global_absence_with_nothing_resident_still_fences_older_fills() {
    let registry = global_registry();
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_entries: 1,
            ..CacheSettings::default()
        },
    );
    let (first, second) = (tenant(1), tenant(2));
    get(&cache, &first, &[TYPE]).await;
    let forced = BatchGetEntitiesRequest {
        fresh: true,
        ..BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full())
    };
    let first_context = CachedTypesRegistry::context(&first).expect("authenticated");
    let second_context = CachedTypesRegistry::context(&second).expect("authenticated");
    let (captured, wait_captured) = tokio::sync::oneshot::channel();
    let (release, wait_release) = tokio::sync::oneshot::channel();
    let (start, wait_start) = tokio::sync::oneshot::channel();
    let absence = async {
        let result = cache
            .cache
            .read(forced.clone(), Some(&first_context), |_| async {
                cache
                    .batch_get_entities(&second, forced.clone())
                    .await
                    .expect("revalidated");
                get(&cache, &second, &[OTHER]).await;
                start.send(()).expect("start");
                wait_captured.await.expect("captured");
                Ok(BatchGetEntitiesResponse(
                    [(gts_key(TYPE), EntityLookup::NotFound)].into(),
                ))
            })
            .await
            .expect("read");
        assert_eq!(result.0[&gts_key(TYPE)], EntityLookup::NotFound);
        release.send(()).expect("release");
    };
    let older = async {
        wait_start.await.expect("started");
        cache
            .cache
            .read(forced.clone(), Some(&second_context), |request| async {
                let positive = registry.batch_get_entities(&second, request).await;
                captured.send(()).expect("captured");
                wait_release.await.expect("released");
                positive
            })
            .await
            .expect("read");
    };
    tokio::join!(absence, older);
    assert!(
        cache.cache.store().peek_any(&gts_key(TYPE)).is_none(),
        "the older positive reply did not refill the entity after its absence"
    );
}

/// A changed validator answering the read's own condition retires other tenants'
/// snapshots even when the reply is too large to keep and the reader's own smaller
/// snapshot already carries it.
#[tokio::test(start_paused = true)]
async fn an_oversized_reply_to_a_changed_condition_retires_other_tenants_snapshots() {
    let registry = Arc::new(TenantRegistry::new([]));
    change(&registry.registry, TYPE, "old", 1);
    let (owner, other) = (tenant(1), tenant(2));
    let key = gts_key(TYPE);
    let light =
        BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(FieldSelection::light());
    let wide = BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(full());
    let held = registry
        .batch_get_entities(&owner, wide.clone())
        .await
        .expect("held by the caller");
    let EntityLookup::Found {
        etag: held_etag, ..
    } = &held.0[&key]
    else {
        panic!("found");
    };
    let condition = BatchGetItem::new(key.clone()).revalidate(&held.0[&key]);
    let cache = CachedTypesRegistry::new(
        registry.clone(),
        CacheSettings {
            max_size_bytes: 2048,
            ..CacheSettings::default()
        },
    );
    let stale = cache
        .batch_get_entities(&other, light.clone())
        .await
        .expect("other tenant's light read");
    change(&registry.registry, TYPE, &"new".repeat(2000), 2);
    let latest = cache
        .batch_get_entities(&owner, light.clone())
        .await
        .expect("owner's light read");
    assert_ne!(stale, latest);
    let result = cache
        .batch_get_entities(
            &owner,
            BatchGetEntitiesRequest {
                items: vec![condition],
                ..wide
            },
        )
        .await
        .expect("conditional wide read");
    let EntityLookup::Found { etag, .. } = &result.0[&key] else {
        panic!("found");
    };
    assert_ne!(etag, held_etag);
    assert_eq!(
        registry
            .calls
            .lock()
            .expect("calls")
            .last()
            .expect("call")
            .1
            .as_ref(),
        Some(held_etag),
        "the read sent the caller's condition"
    );
    assert_eq!(
        cache
            .batch_get_entities(&other, light)
            .await
            .expect("other tenant's next read")
            .0[&key],
        latest.0[&key],
        "the other tenant's older snapshot was retired"
    );
}
