use std::collections::{HashMap, HashSet};

use toolkit_security::SecurityContext;

use super::*;
use crate::cache::CachedTypesRegistry;
use crate::contract::TypesRegistryApi;

/// A tenant registry with entity visibility independent of the snapshot contents.
struct TenantRegistry {
    registry: MockTypesRegistry,
    calls: Mutex<Vec<(Uuid, Option<Validator>)>>,
    /// Subjects and tenants the registry answers `not_found`.
    hidden: Mutex<HashSet<Uuid>>,
    /// Each batch resolution's tenant and keys, in order.
    resolutions: Mutex<Vec<(Uuid, Vec<EntityKey>)>>,
}

impl TenantRegistry {
    fn hides(&self, ctx: &SecurityContext) -> bool {
        let hidden = self.hidden.lock().expect("hidden");
        hidden.contains(&ctx.subject_tenant_id()) || hidden.contains(&ctx.subject_id())
    }

    fn new(hidden: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            registry: MockTypesRegistry::new(),
            calls: Mutex::new(Vec::new()),
            hidden: Mutex::new(hidden.into_iter().collect()),
            resolutions: Mutex::new(Vec::new()),
        }
    }

    fn resolutions(&self) -> Vec<(Uuid, Vec<EntityKey>)> {
        self.resolutions.lock().expect("resolutions").clone()
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
            return Ok(BatchGetEntitiesResponse(
                request
                    .items
                    .into_iter()
                    .map(|item| (item.key, EntityLookup::NotFound))
                    .collect(),
            ));
        }
        TypesRegistryApi::batch_get_entities(&self.registry, ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        TypesRegistryApi::list_entities(&self.registry, ctx, request).await
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
        TypesRegistryApi::batch_resolve_identities(&self.registry, ctx, keys).await
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

/// The registry may authorize each entity per subject, so only the read context
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

    for (other, why) in [
        (subject(11, Some("bob")), "another subject of the tenant"),
        (with(None, &["read"]), "other token scopes"),
        (with(None, &["read", "write"]), "more token scopes"),
        (
            with(None, &["write", "read"]),
            "the same scopes in another order",
        ),
        (with(Some("service"), &[]), "another subject type"),
        (
            SecurityContext::builder()
                .subject_id(Uuid::from_u128(10))
                .subject_tenant_id(Uuid::from_u128(2))
                .build()
                .expect("context"),
            "the same subject in another home tenant",
        ),
    ] {
        let before = registry.calls.lock().expect("calls").len();
        let calls = read_as(&cache, &registry, &other).await;
        assert_eq!(calls.len(), before + 1, "{why} revalidates");
        assert_eq!(
            calls[before],
            (other.subject_tenant_id(), Some(etag.clone())),
            "{why} revalidates the shared snapshot"
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

#[tokio::test(start_paused = true)]
async fn tenants_share_snapshot_bytes_but_confirm_visibility_independently() {
    let registry = Arc::new(TenantRegistry::new([Uuid::from_u128(2)]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
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

    let first = read(tenant(1), false).await;
    let EntityLookup::Found { ref etag, .. } = first else {
        panic!("tenant 1 can see the entity");
    };
    let stored = cache
        .cache
        .store()
        .peek(&key)
        .expect("snapshot")
        .snapshot
        .clone();
    assert_eq!(read(tenant(2), false).await, EntityLookup::NotFound);
    assert_eq!(read(tenant(1), false).await, first);
    assert_eq!(registry.calls.lock().expect("calls").len(), 2);

    registry.hidden.lock().expect("hidden").clear();
    assert_eq!(read(tenant(2), false).await, first);
    assert_eq!(read(tenant(2), false).await, first);
    assert_eq!(
        registry.calls.lock().expect("calls").as_slice(),
        &[
            (Uuid::from_u128(1), None),
            (Uuid::from_u128(2), Some(etag.clone())),
            (Uuid::from_u128(2), Some(etag.clone())),
        ]
    );
    assert!(Arc::ptr_eq(
        &stored,
        &cache.cache.store().peek(&key).expect("snapshot").snapshot
    ));

    registry
        .hidden
        .lock()
        .expect("hidden")
        .insert(Uuid::from_u128(1));
    assert_eq!(read(tenant(1), true).await, EntityLookup::NotFound);
    assert_eq!(read(tenant(1), false).await, EntityLookup::NotFound);
    assert_eq!(read(tenant(2), false).await, first);
    assert_eq!(registry.calls.lock().expect("calls").len(), 5);

    tokio::time::advance(WINDOW).await;
    assert_eq!(read(tenant(2), false).await, first);
    assert_eq!(registry.calls.lock().expect("calls").len(), 6);
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
        (
            subject(102, None),
            subject(102, None).subject_id(),
            false,
        ),
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
                registry
                    .hidden
                    .lock()
                    .expect("hidden")
                    .insert(hide);
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
async fn widening_the_projection_keeps_other_tenants_confirmed_under_the_same_etag() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    let key = gts_key(TYPE);
    let request = BatchGetEntitiesRequest::new([key.clone().into()]);
    let narrow = request.clone().with_projection(content_only());
    let first = cache
        .batch_get_entities(&tenant(1), narrow.clone())
        .await
        .expect("narrow read");
    let wide = cache
        .batch_get_entities(&tenant(2), request.with_projection(full()))
        .await
        .expect("wide read");
    let (EntityLookup::Found { etag: before, .. }, EntityLookup::Found { etag: after, .. }) =
        (&first.0[&key], &wide.0[&key])
    else {
        panic!("both tenants see the entity");
    };
    assert_eq!(before, after);
    assert_eq!(
        cache
            .batch_get_entities(&tenant(1), narrow)
            .await
            .expect("cached read"),
        first
    );
    assert_eq!(registry.calls(Call::BatchGet), 2);
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
    for id in [2, 1, 1] {
        assert_eq!(
            cache
                .batch_get_entities(&tenant(id), request.clone())
                .await
                .expect("read"),
            first
        );
    }
    assert_eq!(
        registry.calls.lock().expect("calls").as_slice(),
        &[
            (Uuid::from_u128(1), None),
            (Uuid::from_u128(2), Some(etag.clone())),
            (Uuid::from_u128(1), Some(etag.clone())),
        ]
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
        cache.cache.store().peek(&key).is_some(),
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
    let registry = Arc::new(MockTypesRegistry::new());
    for id in [TYPE, OTHER, THIRD] {
        registry.seed(id, schema(id, "v1"));
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
    assert_eq!(registry.calls(Call::BatchGet), 3);

    read(OTHER).await;
    assert_eq!(
        registry.calls(Call::BatchGet),
        3,
        "the more recent snapshot stayed"
    );
    read(TYPE).await;
    assert_eq!(
        registry.calls(Call::BatchGet),
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
    assert!(cache.cache.store().peek(&keys[0]).is_none(), "no snapshot");
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
        cache.cache.store().peek(&gts_key(TYPE)).is_none(),
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
        cache.cache.store().peek(&gts_key(TYPE)).is_none(),
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

/// Anonymous discovery bypasses the cache: it teaches no pair.
#[tokio::test(start_paused = true)]
async fn anonymous_discovery_teaches_nothing() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new(registry.clone(), CacheSettings::default());
    cache
        .list_entities(
            &SecurityContext::anonymous(),
            ListEntitiesRequest::default(),
        )
        .await
        .expect("page");
    assert_eq!(
        cache.cache.store().identities.peek_uuid(&gts_key(TYPE)),
        None
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

#[test]
fn debug_shows_the_window_only() {
    let cache =
        CachedTypesRegistry::new(Arc::new(MockTypesRegistry::new()), CacheSettings::default());
    assert_eq!(
        format!("{cache:?}"),
        format!(
            "CachedTypesRegistry {{ freshness_window: {:?}, .. }}",
            CacheSettings::DEFAULT_FRESHNESS_WINDOW
        )
    );
}
