use std::collections::{HashMap, HashSet};

use toolkit_security::SecurityContext;

use super::*;
use crate::cache::CachedTypesRegistry;
use crate::contract::TypesRegistryApi;

/// A tenant registry with entity visibility independent of the snapshot contents.
struct TenantRegistry {
    registry: MockTypesRegistry,
    calls: Mutex<Vec<(Uuid, Option<Validator>)>>,
    hidden: Mutex<HashSet<Uuid>>,
    /// Each batch resolution's tenant and keys, in order.
    resolutions: Mutex<Vec<(Uuid, Vec<EntityKey>)>>,
}

impl TenantRegistry {
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
        if self
            .hidden
            .lock()
            .expect("hidden")
            .contains(&ctx.subject_tenant_id())
        {
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
        if self
            .hidden
            .lock()
            .expect("hidden")
            .contains(&ctx.subject_tenant_id())
        {
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

#[tokio::test(start_paused = true)]
async fn tenants_share_snapshot_bytes_but_confirm_visibility_independently() {
    let registry = Arc::new(TenantRegistry::new([Uuid::from_u128(2)]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new_local(
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
async fn an_auth_error_revokes_all_confirmations_for_the_viewer() {
    for refusal in [
        CanonicalError::unauthenticated()
            .with_reason("TOKEN_INVALID")
            .create(),
        crate::gts::TypeResource::permission_denied()
            .with_reason("DENIED")
            .create(),
    ] {
        let registry = Arc::new(MockTypesRegistry::new());
        for id in [TYPE, OTHER, THIRD] {
            registry.seed(id, schema(id, "v1"));
        }
        for attempt in [3, 4, 5] {
            registry.inject(Fault::on(Call::BatchGet).nth(attempt).fail(refusal.clone()));
        }
        let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
        let caller = tenant(1);
        let other_tenant = tenant(2);
        let first = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
        let second = BatchGetEntitiesRequest::new([gts_key(OTHER).into()]).with_projection(full());
        let warm = BatchGetEntitiesRequest::new([TYPE, OTHER, THIRD].map(|id| gts_key(id).into()))
            .with_projection(full());
        for ctx in [&caller, &other_tenant] {
            let response = cache
                .batch_get_entities(ctx, warm.clone())
                .await
                .expect("visible");
            assert!(
                response
                    .0
                    .values()
                    .all(|answer| matches!(answer, EntityLookup::Found { .. }))
            );
        }

        let fresh = BatchGetEntitiesRequest {
            fresh: true,
            ..first.clone()
        };
        for req in [fresh, second, first] {
            let error = cache
                .batch_get_entities(&caller, req)
                .await
                .expect_err("refused");
            assert_eq!(
                std::mem::discriminant(&error),
                std::mem::discriminant(&refusal)
            );
        }
        let other = cache
            .batch_get_entities(&other_tenant, warm)
            .await
            .expect("other tenant");
        assert!(
            other
                .0
                .values()
                .all(|answer| matches!(answer, EntityLookup::Found { .. }))
        );
        assert_eq!(registry.calls(Call::BatchGet), 5);

        // A successful read issues a new epoch for the viewer. The other old
        // confirmation still requires a registry read.
        for id in [TYPE, THIRD] {
            let request =
                BatchGetEntitiesRequest::new([gts_key(id).into()]).with_projection(full());
            cache
                .batch_get_entities(&caller, request)
                .await
                .expect("access restored");
        }
        assert_eq!(registry.calls(Call::BatchGet), 7);
    }
}

#[tokio::test(start_paused = true)]
async fn a_discovery_auth_error_revokes_batch_visibility() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(TYPE, schema(TYPE, "v1"));
    registry.inject(
        Fault::on(Call::List).fail(
            CanonicalError::unauthenticated()
                .with_reason("TOKEN_INVALID")
                .create(),
        ),
    );
    let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
    let caller = tenant(1);
    let request = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
    cache
        .batch_get_entities(&caller, request.clone())
        .await
        .expect("visible");
    assert!(matches!(
        cache
            .list_entities(&caller, ListEntitiesRequest::default())
            .await,
        Err(CanonicalError::Unauthenticated { .. })
    ));
    cache
        .batch_get_entities(&caller, request)
        .await
        .expect("recheck");
    assert_eq!(registry.calls(Call::BatchGet), 2);
}

/// A refusal while a read for the same viewer is in flight fences that read: its answer
/// must not confirm the viewer the registry has just refused.
#[tokio::test(start_paused = true)]
async fn a_refusal_fences_a_read_in_flight_for_its_viewer() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(TYPE, schema(TYPE, "v1"));
    registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(1)),
    );
    registry.inject(
        Fault::on(Call::List).fail(
            CanonicalError::unauthenticated()
                .with_reason("TOKEN_INVALID")
                .create(),
        ),
    );
    let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
    let caller = tenant(1);
    let request = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
    let (held, refused) = tokio::join!(cache.batch_get_entities(&caller, request.clone()), async {
        tokio::task::yield_now().await;
        cache
            .list_entities(&caller, ListEntitiesRequest::default())
            .await
    });
    held.expect("the held read is answered");
    assert!(
        matches!(refused, Err(CanonicalError::Unauthenticated { .. })),
        "{refused:?}"
    );
    cache
        .batch_get_entities(&caller, request)
        .await
        .expect("recheck");
    assert_eq!(
        registry.calls(Call::BatchGet),
        2,
        "the held answer did not confirm the refused viewer"
    );
}

/// A tenant read takes a slot for its viewer and releases it however it ends.
#[tokio::test(start_paused = true)]
async fn a_tenant_read_releases_its_viewer_slot_when_done_or_dropped() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(TYPE, schema(TYPE, "v1"));
    registry.inject(Fault::on(Call::BatchGet).delay(Duration::from_secs(1)));
    let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
    let caller = tenant(1);
    let viewer = super::super::store::TenantViewer::new(caller.subject_tenant_id(), None);
    let in_flight = || cache.cache.store().tenant_in_flight(viewer);
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

#[tokio::test(start_paused = true)]
async fn an_absent_read_fences_only_its_viewers_confirmation() {
    for denied_id in [1, 2] {
        let registry = Arc::new(TenantRegistry::new([]));
        registry.registry.seed(TYPE, schema(TYPE, "v1"));
        registry.registry.inject(
            Fault::on(Call::BatchGet)
                .nth(1)
                .delay(Duration::from_secs(1)),
        );
        let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
        let key = gts_key(TYPE);
        let request = BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(full());
        let allowed = tenant(1);
        let denied = tenant(denied_id);
        let (visible, hidden) =
            tokio::join!(cache.batch_get_entities(&allowed, request.clone()), async {
                tokio::task::yield_now().await;
                registry
                    .hidden
                    .lock()
                    .expect("hidden")
                    .insert(denied.subject_tenant_id());
                cache.batch_get_entities(&denied, request.clone()).await
            });
        let visible = visible.expect("found");
        assert!(matches!(visible.0[&key], EntityLookup::Found { .. }));
        assert_eq!(hidden.expect("hidden").0[&key], EntityLookup::NotFound);
        let next = cache
            .batch_get_entities(&allowed, request)
            .await
            .expect("next read");
        if denied_id == 1 {
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
    let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
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
    let cache = CachedTypesRegistry::new_local(
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

/// A viewer whose visibility of a resident snapshot is confirmed inside the window
/// resolves it under either spelling without the registry. Another viewer, or the same
/// one once its confirmation expires, is resolved by the registry for itself, and its
/// answer confirms nothing.
#[tokio::test(start_paused = true)]
async fn only_a_freshly_confirmed_viewer_resolves_from_the_resident_snapshot() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
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
        let answered = resolve(&cache, &tenant(2), &keys[..1])
            .await
            .expect("resolved");
        assert_eq!(answered[&keys[0]], Some(identity(TYPE)));
    }
    tokio::time::advance(WINDOW).await;
    resolve(&cache, &tenant(1), &keys[..1])
        .await
        .expect("resolved");
    assert_eq!(
        registry.resolutions(),
        [
            (Uuid::from_u128(2), vec![gts_key(TYPE)]),
            (Uuid::from_u128(2), vec![gts_key(TYPE)]),
            (Uuid::from_u128(1), vec![gts_key(TYPE)]),
        ]
    );
    assert_eq!(registry.calls.lock().expect("calls").len(), 1, "no read");
}

/// A credential refusal of the resolution revokes every confirmation of the viewer, as a
/// refused batch read or page does; another viewer's stand.
#[tokio::test(start_paused = true)]
async fn a_resolution_auth_error_revokes_all_confirmations_for_the_viewer() {
    for refusal in [
        CanonicalError::unauthenticated()
            .with_reason("TOKEN_INVALID")
            .create(),
        crate::gts::TypeResource::permission_denied()
            .with_reason("DENIED")
            .create(),
    ] {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed(TYPE, schema(TYPE, "v1"));
        registry.seed(OTHER, schema(OTHER, "v1"));
        registry.inject(Fault::on(Call::BatchResolve).fail(refusal.clone()));
        let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
        let request = BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full());
        for id in [1, 2] {
            cache
                .batch_get_entities(&tenant(id), request.clone())
                .await
                .expect("warm");
        }

        let error = resolve(&cache, &tenant(1), &[gts_key(TYPE), gts_key(OTHER)])
            .await
            .expect_err("refused");
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&refusal)
        );
        assert_eq!(registry.calls(Call::BatchResolve), 1, "only OTHER was sent");

        for id in [2, 1] {
            cache
                .batch_get_entities(&tenant(id), request.clone())
                .await
                .expect("read");
        }
        assert_eq!(
            registry.calls(Call::BatchGet),
            3,
            "only the refused viewer reads again"
        );
    }
}

/// The registry answers the viewer's resolution `not_found` while the viewer's own read
/// of the entity is in flight: the read's confirmation is fenced, so the viewer reads
/// again, while the shared snapshot stays resident for another viewer's confirmation.
#[tokio::test(start_paused = true)]
async fn a_resolution_absence_fences_only_its_viewers_confirmation() {
    let registry = Arc::new(TenantRegistry::new([]));
    registry.registry.seed(TYPE, schema(TYPE, "v1"));
    registry.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .delay(Duration::from_secs(1)),
    );
    let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
    let key = gts_key(TYPE);
    let request = BatchGetEntitiesRequest::new([key.clone().into()]).with_projection(full());
    let viewer = tenant(1);
    let (read, resolved) =
        tokio::join!(cache.batch_get_entities(&viewer, request.clone()), async {
            tokio::task::yield_now().await;
            registry
                .hidden
                .lock()
                .expect("hidden")
                .insert(viewer.subject_tenant_id());
            resolve(&cache, &viewer, std::slice::from_ref(&key)).await
        });
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
        .batch_get_entities(&viewer, request.clone())
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
        "the other viewer confirms the resident snapshot once"
    );
}

/// Room for two snapshots. A viewer with no confirmation probing a resident key leaves
/// its recency, so the next fill evicts it rather than the more recent snapshot.
#[tokio::test(start_paused = true)]
async fn an_unconfirmed_viewer_s_resolution_leaves_the_snapshot_s_recency() {
    let registry = Arc::new(MockTypesRegistry::new());
    for id in [TYPE, OTHER, THIRD] {
        registry.seed(id, schema(id, "v1"));
    }
    let cache = CachedTypesRegistry::new_local(
        registry.clone(),
        CacheSettings {
            max_entries: 2,
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

/// The registry's answers are checked against every resident snapshot: a resolution
/// pairing another identifier with a resident Registry Reference fails.
#[tokio::test(start_paused = true)]
async fn a_tenant_resolution_contradicting_a_resident_snapshot_fails() {
    let registry = Arc::new(MockTypesRegistry::new());
    let tail = "7a1d2f34-5678-49ab-9012-abcdef123456";
    let registered = format!("{TYPE}{tail}");
    let alias = format!("{OTHER}{tail}");
    registry.seed(&registered, json!({}));
    let cache = CachedTypesRegistry::new_local(registry.clone(), CacheSettings::default());
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
fn debug_shows_the_window_and_binding_only() {
    let cache = CachedTypesRegistry::new_local(
        Arc::new(MockTypesRegistry::new()),
        CacheSettings::default(),
    );
    assert_eq!(
        format!("{cache:?}"),
        format!(
            r#"CachedTypesRegistry {{ freshness_window: {:?}, binding: "trusted_local", .. }}"#,
            CacheSettings::DEFAULT_FRESHNESS_WINDOW
        )
    );
}
