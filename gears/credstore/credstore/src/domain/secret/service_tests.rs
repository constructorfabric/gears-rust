//! Unit tests for [`Service`] using domain fakes.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{
    CredStorePluginClientV1, OwnerId, SecretRef, SecretType, SecretValue, SharingMode, TenantId,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{SecretRow, SecretStatus, WriteSpec};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::service::{ReaperSettings, Service};
use toolkit_security::AccessScope;

use crate::domain::secret::test_support::{
    FakeDir, FakeMetrics, FakePlugin, FakePluginSelector, FakeSecretRepo, NoopMetrics,
    catalog_type_resolver, deny_enforcer, failing_enforcer, make_ctx, mock_enforcer,
};

fn key(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid key")
}

fn make_service(
    repo: Arc<FakeSecretRepo>,
    dir: Arc<FakeDir>,
    enforcer: PolicyEnforcer,
    plugin: Arc<FakePlugin>,
    metrics: Arc<FakeMetrics>,
) -> Service {
    let selector = Arc::new(FakePluginSelector::new(plugin));
    Service::new(
        repo,
        dir,
        enforcer,
        selector,
        catalog_type_resolver(),
        metrics,
        test_reaper_settings(),
    )
}

fn test_reaper_settings() -> ReaperSettings {
    ReaperSettings {
        tick_secs: 60,
        provisioning_timeout_secs: 300,
        deprovisioning_timeout_secs: 300,
    }
}

fn make_service_noop(
    repo: Arc<FakeSecretRepo>,
    dir: Arc<FakeDir>,
    enforcer: PolicyEnforcer,
    plugin: Arc<FakePlugin>,
) -> Service {
    let selector = Arc::new(FakePluginSelector::new(plugin));
    Service::new(
        repo,
        dir,
        enforcer,
        selector,
        catalog_type_resolver(),
        Arc::new(NoopMetrics),
        test_reaper_settings(),
    )
}

// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_own_tenant_secret_returns_hit_own() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("mykey");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());
    let metrics = FakeMetrics::new();

    // Pre-seed the plugin with a value.
    let plugin_key_owner: Option<&OwnerId> = None;
    plugin
        .put(
            &ctx,
            &TenantId(tenant),
            &k,
            SecretValue::from("v1"),
            plugin_key_owner,
        )
        .await
        .expect("put");

    // Seed repo with active row.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "mykey".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    let result = svc.get(&ctx, &k).await.expect("get ok");
    assert!(result.is_some());
    let resp = result.unwrap();
    assert!(!resp.is_inherited);
    assert_eq!(resp.owner_tenant_id.0, tenant);
    assert_eq!(
        metrics.last_read_outcome(),
        Some(crate::domain::ports::metrics::ReadOutcome::HitOwn)
    );
}

#[tokio::test]
async fn get_records_pdp_dependency_metric() {
    use crate::domain::ports::metrics::{Dep, DepOp, Outcome};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("mykey");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());
    let metrics = FakeMetrics::new();
    plugin
        .put(&ctx, &TenantId(tenant), &k, SecretValue::from("v1"), None)
        .await
        .expect("put");
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "mykey".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    svc.get(&ctx, &k).await.expect("get ok");

    assert!(
        metrics
            .deps()
            .contains(&(Dep::Pdp, DepOp::Evaluate, Outcome::Success)),
        "get must record a PDP dependency metric, got {:?}",
        metrics.deps()
    );
}

#[tokio::test]
async fn put_records_pdp_dependency_metric() {
    use crate::domain::ports::metrics::{Dep, DepOp, Outcome};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("putkey");

    let metrics = FakeMetrics::new();
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        metrics.clone(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("put ok");

    assert!(
        metrics
            .deps()
            .contains(&(Dep::Pdp, DepOp::Evaluate, Outcome::Success)),
        "put must record a PDP dependency metric, got {:?}",
        metrics.deps()
    );
}

#[tokio::test]
async fn delete_records_pdp_dependency_metric() {
    use crate::domain::ports::metrics::{Dep, DepOp, Outcome};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("delkey");

    let metrics = FakeMetrics::new();
    // The PDP is consulted only for a resolved row (prefetch model), so seed one.
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "delkey".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        metrics.clone(),
    );

    svc.delete(&ctx, &k, None).await.expect("delete");

    assert!(
        metrics
            .deps()
            .contains(&(Dep::Pdp, DepOp::Evaluate, Outcome::Success)),
        "delete must record a PDP dependency metric, got {:?}",
        metrics.deps()
    );
}

#[tokio::test]
async fn get_inherited_shared_from_parent_sets_is_inherited() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, child);
    let k = key("shared-key");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());

    // Seed plugin with value at parent tenant, no owner (shared).
    plugin
        .put(
            &ctx,
            &TenantId(parent),
            &k,
            SecretValue::from("parent-val"),
            None,
        )
        .await
        .expect("put");

    // Seed repo: parent has a shared active row.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(parent),
        reference: "shared-key".into(),
        sharing: SharingMode::Shared,
        owner_id: OwnerId(Uuid::nil()),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    // chain: [child, parent]
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo, dir, mock_enforcer(), plugin);

    let result = svc.get(&ctx, &k).await.expect("get ok");
    assert!(result.is_some());
    let resp = result.unwrap();
    assert!(resp.is_inherited, "must be inherited from parent");
    assert_eq!(resp.owner_tenant_id.0, parent);
    assert_eq!(resp.sharing, SharingMode::Shared);
}

#[tokio::test]
async fn get_tenant_mode_not_inherited_by_child() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, child);
    let k = key("tenant-key");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());

    // Parent has a tenant-mode row; tenant-mode is NOT inherited by children.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(parent),
        reference: "tenant-key".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(Uuid::nil()),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo, dir, mock_enforcer(), plugin);

    let result = svc.get(&ctx, &k).await.expect("get ok");
    // Tenant-mode secret at parent → not visible from child → Miss.
    assert!(result.is_none());
}

#[tokio::test]
async fn get_private_owner_match_only() {
    let tenant = Uuid::new_v4();
    let owner_a = Uuid::new_v4();
    let owner_b = Uuid::new_v4();
    // Subject is B; row belongs to A (private).
    let ctx = make_ctx(owner_b, tenant);
    let k = key("private-key");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());

    // A has a private row.
    let row_id = Uuid::new_v4();
    repo.seed(SecretRow {
        id: row_id,
        tenant_id: TenantId(tenant),
        reference: "private-key".into(),
        sharing: SharingMode::Private,
        owner_id: OwnerId(owner_a),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    // Seed plugin for A.
    plugin
        .put(
            &make_ctx(owner_a, tenant),
            &TenantId(tenant),
            &k,
            SecretValue::from("secret-a"),
            Some(&OwnerId(owner_a)),
        )
        .await
        .expect("put");

    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
    );

    // B can't see A's private secret.
    let result = svc.get(&ctx, &k).await.expect("get ok");
    assert!(
        result.is_none(),
        "private row for A must not be visible to B"
    );
}

#[tokio::test]
async fn get_shadowing_private_beats_inherited() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, child);
    let k = key("shadow-key");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());

    // Child has a private row for subject.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(child),
        reference: "shadow-key".into(),
        sharing: SharingMode::Private,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    // Parent has a shared row.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(parent),
        reference: "shadow-key".into(),
        sharing: SharingMode::Shared,
        owner_id: OwnerId(Uuid::nil()),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    // Seed plugin: child private, parent shared.
    plugin
        .put(
            &ctx,
            &TenantId(child),
            &k,
            SecretValue::from("child-private"),
            Some(&OwnerId(subject)),
        )
        .await
        .expect("put child private");
    plugin
        .put(
            &ctx,
            &TenantId(parent),
            &k,
            SecretValue::from("parent-shared"),
            None,
        )
        .await
        .expect("put parent shared");

    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo, dir, mock_enforcer(), plugin);

    let result = svc.get(&ctx, &k).await.expect("get ok");
    assert!(result.is_some());
    let resp = result.unwrap();
    assert!(
        !resp.is_inherited,
        "child-private must shadow parent-shared"
    );
    assert_eq!(resp.owner_tenant_id.0, child);
    assert_eq!(resp.value.as_bytes(), b"child-private");
}

#[tokio::test]
async fn put_create_conflict_returns_conflict() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("existing-key");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());

    // Seed existing active row.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "existing-key".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
    );

    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("val"),
            WriteSpec::create(SharingMode::Tenant),
        )
        .await;
    assert!(matches!(result, Err(DomainError::Conflict)));
}

#[tokio::test]
async fn put_shared_coexists_with_private() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("priv-key");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());

    // Existing private row.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "priv-key".into(),
        sharing: SharingMode::Private,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
    );

    // PUT Shared for the same ref CREATES a coexisting non-private secret — it
    // does not "transition" the private one. Per design §4.1 a tenant/shared and
    // a private secret coexist under one reference (distinct partial-unique keys).
    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("val"),
            WriteSpec::upsert(SharingMode::Shared),
        )
        .await;
    assert!(result.is_ok(), "expected coexistence Ok, got {result:?}");

    let counts = repo.inventory().await.expect("inventory");
    assert_eq!(counts.private, 1, "private secret must remain");
    assert_eq!(counts.shared, 1, "shared secret created alongside private");
}

#[tokio::test]
async fn put_omitting_sharing_preserves_existing_shared_mode() {
    // A value rotation that omits `sharing` (handler sets preserve_sharing)
    // must NOT narrow a `shared` secret back to `tenant` (review finding #8).
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("rotate-me");

    let plugin = FakePlugin::new();
    plugin.seed_fence_key();
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "rotate-me".into(),
        sharing: SharingMode::Shared,
        owner_id: OwnerId(Uuid::nil()),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
    );

    // Rotate the value with sharing omitted: class default `Tenant`, but
    // `preserve_sharing(true)`. The old default-narrowing bug flipped Shared
    // to Tenant here.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("rotated"),
        WriteSpec::upsert(SharingMode::Tenant).preserve_sharing(true),
    )
    .await
    .expect("rotate ok");

    let rows = repo.rows();
    let row = rows
        .iter()
        .find(|r| r.reference == "rotate-me")
        .expect("row present");
    assert_eq!(
        row.sharing,
        SharingMode::Shared,
        "omitted sharing must preserve the stored Shared mode, not narrow to Tenant"
    );
}

#[tokio::test]
async fn put_omitting_sharing_rotates_existing_private_secret() {
    // A rotation with omitted sharing on a reference that only has the caller's
    // PRIVATE secret must rotate THAT secret, not silently create a tenant row
    // GET never returns (the PUT would 204 while GET still shows the old value).
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("rotate-priv");

    let plugin = FakePlugin::new();
    plugin.seed_fence_key();
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "rotate-priv".into(),
        sharing: SharingMode::Private,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
    );

    // Handler translates an omitted `sharing` to (Tenant default, preserve).
    svc.put(
        &ctx,
        &k,
        SecretValue::from("rotated"),
        WriteSpec::upsert(SharingMode::Tenant).preserve_sharing(true),
    )
    .await
    .expect("rotate private ok");

    let counts = repo.inventory().await.expect("inventory");
    assert_eq!(counts.private, 1, "the private secret is rotated in place");
    assert_eq!(
        counts.tenant, 0,
        "no tenant row is created by an omitted-sharing rotation"
    );
    // The new value landed under the private class, not the tenant class.
    assert!(
        plugin.contains(&TenantId(tenant), &k, Some(&OwnerId(subject))),
        "rotated value written to the private key class"
    );
    assert!(
        !plugin.contains(&TenantId(tenant), &k, None),
        "no value written to the tenant key class"
    );
}

/// Regression (fence-key cache thrash): a permanently-poisoned row (a
/// by-design, fail-closed fingerprint mismatch, e.g. from a crosswise LWW
/// interleave) must NOT re-read the fence key from the backend on every get.
/// The mismatch-triggered refresh is cooldown-gated and reuses the cache, so
/// repeated poisoned reads collapse to a single cold load rather than a backend
/// round-trip (and a global cache eviction) per request.
#[tokio::test]
async fn poisoned_reads_do_not_rethread_the_fence_key_each_time() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("poisoned");

    let plugin = FakePlugin::new();
    plugin.seed_fence_key();
    // Backend value present, but the row's stored fingerprint can never match
    // it under the fence key — the permanent poison the fence produces.
    plugin.seed_value(&TenantId(tenant), &k, None, b"backend-value");

    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "poisoned".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(Uuid::nil()),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: Some(vec![0u8; 32]),
        fp_key_id: Some(crate::domain::secret::fence::CURRENT_FENCE_KEY_ID),
    });

    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
    );

    for _ in 0..8 {
        let got = svc.get(&ctx, &k).await.expect("get");
        assert!(got.is_none(), "poisoned value fails closed as a miss");
    }

    // Bounded regardless of read count: the cold load plus a single forced
    // reload on the first mismatch (which heals a genuinely stale key). Every
    // later poisoned read reuses the cache within the cooldown. Without the
    // guard this would be ~2 backend reads *per* get (16 here) and evict the
    // shared cache each time.
    assert!(
        plugin.fence_key_gets() <= 2,
        "poisoned reads must not re-thread the fence key on every request (got {})",
        plugin.fence_key_gets()
    );
}

#[tokio::test]
async fn delete_only_own_tenant_404_when_inherited_only() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, child);
    let k = key("ancestor-key");

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());

    // Only the parent has the row (shared).
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(parent),
        reference: "ancestor-key".into(),
        sharing: SharingMode::Shared,
        owner_id: OwnerId(Uuid::nil()),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });

    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(child)), // child-only chain for find_own
        mock_enforcer(),
        plugin,
    );

    let result = svc.delete(&ctx, &k, None).await;
    assert!(
        matches!(result, Err(DomainError::NotFound)),
        "must return NotFound for inherited-only key"
    );
}

#[tokio::test]
async fn read_gate_denied_when_tenant_out_of_scope() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("some-key");

    let plugin = FakePlugin::new();
    // A resolvable row whose PDP scope excludes the caller's tenant: the read
    // is an anti-enumeration 404, and the cross-tenant denial is recorded.
    let repo = Arc::new(FakeSecretRepo::with_scope_allows(false));
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "some-key".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let metrics = FakeMetrics::new();

    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    let result = svc.get(&ctx, &k).await;
    assert!(
        matches!(result, Ok(None)),
        "out-of-scope read must be an anti-enumeration 404, got {result:?}"
    );
    assert_eq!(
        metrics.cross_tenant_denied_count(),
        1,
        "cross_tenant_denied metric must be recorded"
    );
}

// ── H2: UnsupportedTransition maps to CredStoreError::UnsupportedTransition ──

#[test]
fn unsupported_transition_domain_error_maps_to_sdk_variant() {
    use credstore_sdk::CredStoreError;

    use crate::domain::error::DomainError;

    let domain_err = DomainError::UnsupportedTransition {
        detail: "cannot move between private and tenant/shared".to_owned(),
    };
    let sdk_err = CredStoreError::from(domain_err);
    assert!(
        matches!(sdk_err, CredStoreError::UnsupportedTransition { .. }),
        "expected UnsupportedTransition, got {sdk_err:?}"
    );
    assert!(
        !matches!(sdk_err, CredStoreError::InvalidSecretRef { .. }),
        "must not map to InvalidSecretRef"
    );
}

// ── M3: PDP-denied / eval-failed after Authorizer-port removal ───────────────

#[tokio::test]
async fn get_returns_not_found_when_pdp_denies() {
    // Prefetch model: the PDP is consulted only for a resolved secret, and a
    // denial is indistinguishable from a missing one (anti-enumeration 404).
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("some-key");

    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "some-key".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        deny_enforcer(),
        FakePlugin::new(),
    );

    let result = svc.get(&ctx, &k).await;
    assert!(
        matches!(result, Ok(None)),
        "PDP denial on a resolved secret must be a 404, got {result:?}"
    );
}

#[tokio::test]
async fn get_returns_service_unavailable_when_pdp_fails() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("some-key");

    // A PDP *outage* (distinct from a denial) must surface as 503 — so the row
    // must resolve first for the PDP to be consulted at all.
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "some-key".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        failing_enforcer(),
        FakePlugin::new(),
    );

    let result = svc.get(&ctx, &k).await;
    assert!(
        matches!(result, Err(DomainError::ServiceUnavailable { .. })),
        "expected ServiceUnavailable from failing_enforcer, got {result:?}"
    );
}

#[tokio::test]
async fn operations_return_service_unavailable_when_type_resolver_fails() {
    use crate::domain::secret::test_support::FailingTypeResolver;

    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("some-key");

    // Registry outage (resolver → 503) must fail every operation closed:
    // the resolved row's type gates get/delete, the requested type gates put.
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "some-key".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let selector = Arc::new(FakePluginSelector::new(FakePlugin::new()));
    let svc = Service::new(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        selector,
        Arc::new(FailingTypeResolver),
        Arc::new(NoopMetrics),
        test_reaper_settings(),
    );

    let got = svc.get(&ctx, &k).await;
    assert!(
        matches!(got, Err(DomainError::ServiceUnavailable { .. })),
        "get must fail closed on a registry outage, got {got:?}"
    );
    let put = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::upsert(SharingMode::Tenant),
        )
        .await;
    assert!(
        matches!(put, Err(DomainError::ServiceUnavailable { .. })),
        "put must fail closed on a registry outage, got {put:?}"
    );
    let del = svc.delete(&ctx, &k, None).await;
    assert!(
        matches!(del, Err(DomainError::ServiceUnavailable { .. })),
        "delete must fail closed on a registry outage, got {del:?}"
    );
}

#[tokio::test]
async fn put_returns_access_denied_when_pdp_denies() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("some-key");

    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        deny_enforcer(),
        FakePlugin::new(),
    );

    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::upsert(SharingMode::Tenant),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::AccessDenied { .. })),
        "expected AccessDenied from deny_enforcer, got {result:?}"
    );
}

#[tokio::test]
async fn delete_returns_access_denied_when_pdp_denies() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("some-key");

    // Prefetch model: the row must resolve for the PDP to be consulted. Delete
    // reveals existence to the owner, so a PDP denial is a plain 403.
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "some-key".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        deny_enforcer(),
        FakePlugin::new(),
    );

    let result = svc.delete(&ctx, &k, None).await;
    assert!(
        matches!(result, Err(DomainError::AccessDenied { .. })),
        "expected AccessDenied from deny_enforcer, got {result:?}"
    );
}

// ── map_plugin_err (pure mapping) ─────────────────────────────────────────────

#[test]
fn map_plugin_err_covers_all_variants() {
    use credstore_sdk::CredStoreError;

    use crate::domain::secret::service::map_plugin_err;

    assert!(matches!(
        map_plugin_err(CredStoreError::NotFound),
        DomainError::NotFound
    ));
    assert!(matches!(
        map_plugin_err(CredStoreError::AccessDenied),
        DomainError::AccessDenied { .. }
    ));
    assert!(matches!(
        map_plugin_err(CredStoreError::ServiceUnavailable {
            detail: "x".to_owned(),
            retry_after: Some(std::time::Duration::from_secs(2)),
        }),
        DomainError::ServiceUnavailable { .. }
    ));
    assert!(matches!(
        map_plugin_err(CredStoreError::NoPluginAvailable),
        DomainError::ServiceUnavailable { .. }
    ));
    assert!(matches!(
        map_plugin_err(CredStoreError::Conflict),
        DomainError::Conflict
    ));
    assert!(matches!(
        map_plugin_err(CredStoreError::InvalidSecretRef {
            reason: "r".to_owned()
        }),
        DomainError::Internal { .. }
    ));
    assert!(matches!(
        map_plugin_err(CredStoreError::UnsupportedTransition {
            detail: "d".to_owned()
        }),
        DomainError::Internal { .. }
    ));
    assert!(matches!(
        map_plugin_err(CredStoreError::Internal("boom".to_owned())),
        DomainError::Internal { .. }
    ));
}

#[test]
fn no_plugin_available_maps_to_distinct_non_retryable_unavailable() {
    use credstore_sdk::CredStoreError;

    use crate::domain::secret::service::map_plugin_err;

    let mapped = map_plugin_err(CredStoreError::NoPluginAvailable);
    // Operator misconfiguration: a distinct, stable detail and no `retry_after`,
    // distinguishable from a transient backend outage.
    assert!(
        matches!(
            &mapped,
            DomainError::ServiceUnavailable { detail, retry_after, .. }
                if detail.contains("no storage plugin registered") && retry_after.is_none()
        ),
        "NoPluginAvailable must map to a distinct non-retryable ServiceUnavailable, got {mapped:?}"
    );
}

// ── review follow-up regressions ──────────────────────────────────────────────

#[test]
fn plugin_unavailable_detail_is_curated_off_the_wire() {
    use crate::domain::secret::service::map_plugin_err;
    use credstore_sdk::CredStoreError;
    // A plugin's raw ServiceUnavailable detail (which a future vault-backed
    // plugin could populate with backend specifics or secret material) must
    // not reach the domain error / 503 wire response — a fixed, safe detail is
    // substituted; the raw text stays in the server log only.
    let mapped = map_plugin_err(CredStoreError::ServiceUnavailable {
        detail: "KMS failed while storing s3cr3t-plaintext".to_owned(),
        retry_after: None,
    });
    match mapped {
        DomainError::ServiceUnavailable { detail, .. } => {
            assert_eq!(detail, "storage backend unavailable");
            assert!(
                !detail.contains("s3cr3t"),
                "plugin detail must not leak: {detail}"
            );
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn get_folds_plugin_access_denied_into_anti_enumeration_miss() {
    // A resolved row whose backend denies the read must surface as the
    // anti-enumeration 404 (Ok(None)), never a 403 that reveals existence.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("backend-denied");
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "backend-denied".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::with_get_denied(),
    );
    let res = svc.get(&ctx, &k).await;
    assert!(
        matches!(res, Ok(None)),
        "backend AccessDenied on GET must be a 404 miss, got {res:?}"
    );
}

#[tokio::test]
async fn pdp_denial_is_not_a_dependency_health_error() {
    use crate::domain::ports::metrics::{Dep, DepOp, Outcome};
    // A PDP *denial* is a normal authorization decision — the Pdp/Evaluate
    // dependency-health metric must record Success, not Error, so routine
    // denials don't inflate the PDP error rate / trip false outage alerts.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("denied");
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "denied".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        deny_enforcer(),
        FakePlugin::new(),
        metrics.clone(),
    );
    assert!(svc.get(&ctx, &k).await.expect("denial is a miss").is_none());
    assert!(
        metrics
            .deps()
            .iter()
            .any(|(dep, op, outcome)| *dep == Dep::Pdp
                && *op == DepOp::Evaluate
                && *outcome == Outcome::Success),
        "a PDP denial must record the Pdp/Evaluate dependency as Success, got {:?}",
        metrics.deps()
    );
}

#[tokio::test]
async fn if_match_loser_does_not_clobber_backend_value() {
    use crate::domain::secret::model::WritePrecondition;
    // #1: under If-Match the version CAS is claimed BEFORE the backend write,
    // so a losing writer (touch matches 0 rows) never calls plugin.put and
    // cannot overwrite the winner's value. `with_touch_not_found` forces the
    // 0-row touch that models losing the race.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("cas");
    let repo = Arc::new(FakeSecretRepo::with_touch_not_found());
    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );
    // Seed v1 (create path uses insert_provisioning + mark_active, not touch).
    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create seeds v1");
    let generation = svc.get(&ctx, &k).await.expect("get").expect("present").id;
    // If-Match:"<id>.1" write that loses the CAS (touch → 0 rows).
    let res = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v2-loser"),
            WriteSpec::upsert(SharingMode::Tenant).with_precondition(Some(
                WritePrecondition::Version {
                    id: generation,
                    version: 1,
                },
            )),
        )
        .await;
    assert!(
        matches!(res, Err(DomainError::VersionConflict)),
        "losing the CAS must be a VersionConflict, got {res:?}"
    );
    // The loser never reached plugin.put: the backend still holds v1 and the
    // version never advanced (its own touch matched 0 rows).
    let got = svc
        .get(&ctx, &k)
        .await
        .expect("get")
        .expect("still present");
    assert_eq!(
        got.value.as_bytes(),
        b"v1",
        "loser must not have clobbered the value"
    );
    assert_eq!(got.version, 1, "loser must not have bumped the version");
}

#[tokio::test]
async fn create_only_conflict_is_authorized_before_it_leaks_existence() {
    use credstore_sdk::SecretType as SdkSecretType;
    // #4: an unauthorized caller must not distinguish a create-only conflict
    // (409, the secret exists) from a plain denial — the PDP gate runs before
    // the 409. A caller denied the type gets 403 whether or not the row exists.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let api_key = SdkSecretType::from_name("api-key").expect("known");
    let (enforcer, _resolver) =
        crate::domain::secret::test_support::type_deny_enforcer(vec![api_key.gts_id().to_owned()]);
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "existing".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: api_key.uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        enforcer,
        FakePlugin::new(),
    );
    let err = svc
        .put(
            &ctx,
            &key("existing"),
            SecretValue::from("v"),
            WriteSpec::create(SharingMode::Tenant),
        )
        .await
        .expect_err("create-only over an existing denied-type secret");
    assert!(
        matches!(err, DomainError::AccessDenied { .. }),
        "must be a 403 denial (not a 409 revealing the secret exists), got {err:?}"
    );
}

// ── getter + reaper loop ──────────────────────────────────────────────────────

#[test]
fn reaper_tick_secs_reflects_config() {
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(Uuid::new_v4())),
        mock_enforcer(),
        FakePlugin::new(),
    );
    assert_eq!(svc.reaper_tick_secs(), 60);
}

#[tokio::test]
async fn reap_and_refresh_reaps_provisioning_and_refreshes_inventory() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "prov".to_owned(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(owner),
        status: SecretStatus::Provisioning,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "act".to_owned(),
        sharing: SharingMode::Shared,
        owner_id: OwnerId(owner),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    svc.reap_and_refresh().await;
    assert_eq!(
        repo.inventory().await.expect("inventory").provisioning,
        0,
        "the stuck provisioning row was reaped"
    );
}

// ── private-mode + backend-miss branches ──────────────────────────────────────

#[tokio::test]
async fn put_create_then_update_private_secret() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("priv-key");
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    // Create saga with Private sharing (plugin_owner = Some).
    svc.put(
        &ctx,
        &k,
        SecretValue::new(b"s".to_vec()),
        WriteSpec::upsert(SharingMode::Private),
    )
    .await
    .expect("create private");
    // Update path on the existing private row (still private).
    svc.put(
        &ctx,
        &k,
        SecretValue::new(b"s2".to_vec()),
        WriteSpec::upsert(SharingMode::Private),
    )
    .await
    .expect("update private");

    let got = svc.get(&ctx, &k).await.expect("get").expect("some");
    assert_eq!(got.sharing, SharingMode::Private);
}

#[tokio::test]
async fn get_resolved_row_without_backend_value_is_not_found() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("ghost");
    let repo = Arc::new(FakeSecretRepo::new());
    // Active row exists, but the plugin never received a value for it.
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "ghost".to_owned(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    let err = svc
        .get(&ctx, &k)
        .await
        .expect_err("row resolves but backend has no value");
    assert!(matches!(err, DomainError::NotFound));
}

#[tokio::test]
async fn delete_private_secret_removes_row() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("del-priv");
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::new(b"x".to_vec()),
        WriteSpec::upsert(SharingMode::Private),
    )
    .await
    .expect("create");
    svc.delete(&ctx, &k, None).await.expect("delete private");
    assert!(svc.get(&ctx, &k).await.expect("get").is_none());
}

// ── versioning (option a) ─────────────────────────────────────────────────────

#[tokio::test]
async fn create_starts_at_version_one_then_overwrite_bumps() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("ver");
    let repo = Arc::new(FakeSecretRepo::new());
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    let row = repo
        .find_for_write(
            &AccessScope::for_tenant(tenant),
            TenantId(tenant),
            OwnerId(subject),
            &k,
            SharingMode::Tenant,
        )
        .await
        .expect("find")
        .expect("row");
    assert_eq!(row.version, 1, "create seeds version 1");

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v2"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("overwrite");
    let row = repo
        .find_for_write(
            &AccessScope::for_tenant(tenant),
            TenantId(tenant),
            OwnerId(subject),
            &k,
            SharingMode::Tenant,
        )
        .await
        .expect("find")
        .expect("row");
    assert_eq!(row.version, 2, "overwrite bumps to 2");
}

#[tokio::test]
async fn put_create_race_resolves_to_update() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("race");

    // The winner's row exists but is still Provisioning (invisible to
    // find_for_write). The fake promotes it to Active when our insert conflicts,
    // simulating the winner finishing its saga — so the bounded retry resolves
    // to the update path deterministically.
    let repo = Arc::new(FakeSecretRepo::with_promote_on_conflict(true));
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "race".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Provisioning,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    // PUT (create_only = false) loses the create race, then resolves to update.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("v"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("race resolves to update");
    let row = repo
        .find_for_write(
            &AccessScope::for_tenant(tenant),
            TenantId(tenant),
            OwnerId(subject),
            &k,
            SharingMode::Tenant,
        )
        .await
        .expect("find")
        .expect("row");
    assert_eq!(row.version, 2, "the loser touched the winner's row: 1 -> 2");
}

#[tokio::test]
async fn put_create_race_exhausted_returns_conflict() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("stuck");

    // Winner stuck mid-saga: a Provisioning row that never promotes. PUT's
    // bounded retry never sees it Active -> retry-safe 409.
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "stuck".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Provisioning,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::upsert(SharingMode::Tenant),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::Conflict)),
        "exhausted retry -> 409, got {result:?}"
    );
}

#[tokio::test]
async fn failed_create_does_not_wedge_the_reference() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("wedge");

    // Backend hiccup: the first plugin.put (mid create-saga) fails, then recovers.
    let plugin = FakePlugin::with_put_failures(1);
    // Pre-seed the fence key so the injected failure hits the VALUE write
    // (mid create-saga), not the fence-key bootstrap put.
    plugin.seed_fence_key();
    let repo = Arc::new(FakeSecretRepo::new());
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        FakeMetrics::new(),
    );

    // Create: the provisioning row is inserted, then the backend put fails.
    let first = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::create(SharingMode::Tenant),
        )
        .await;
    assert!(
        first.is_err(),
        "backend put failure must surface, got {first:?}"
    );

    // The failed create must compensate its provisioning row rather than leave
    // it to wedge the reference until the reaper runs.
    assert_eq!(
        repo.inventory().await.expect("inventory").provisioning,
        0,
        "failed create must roll back the provisioning row"
    );

    // A retry now succeeds instead of hitting a misleading 409.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("v"),
        WriteSpec::create(SharingMode::Tenant),
    )
    .await
    .expect("retry after a failed create must succeed, not 409");
}

#[tokio::test]
async fn put_denied_when_tenant_out_of_scope_inserts_no_row() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("oos");

    // PDP allow-with-constraints scope that excludes the caller's own tenant.
    let repo = Arc::new(FakeSecretRepo::with_scope_allows(false));
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        metrics.clone(),
    );

    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::upsert(SharingMode::Tenant),
        )
        .await;

    assert!(
        matches!(result, Err(DomainError::AccessDenied { .. })),
        "out-of-scope write must be denied, got {result:?}"
    );
    // The scope_unchecked insert must never run: no orphan provisioning row.
    assert_eq!(
        repo.inventory().await.expect("inventory").provisioning,
        0,
        "denied write must not insert an orphan provisioning row"
    );
    assert_eq!(
        metrics.cross_tenant_denied_count(),
        1,
        "cross_tenant_denied metric must be recorded on the write path"
    );
}

#[tokio::test]
async fn failed_create_records_rollback_success_metric() {
    use crate::domain::ports::metrics::Outcome;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("rollback-ok");

    let plugin = FakePlugin::with_put_failures(1);
    // Pre-seed the fence key so the injected failure hits the VALUE write
    // (mid create-saga), not the fence-key bootstrap put.
    plugin.seed_fence_key();
    let metrics = FakeMetrics::new();
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    _ = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::create(SharingMode::Tenant),
        )
        .await;

    assert_eq!(
        metrics.provisioning_rollbacks(),
        vec![Outcome::Success],
        "a compensated create must record one successful rollback"
    );
}

#[tokio::test]
async fn failed_rollback_records_error_metric() {
    use crate::domain::ports::metrics::Outcome;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("rollback-fail");

    // Backend put fails, then the rollback delete also fails — the reference
    // stays wedged, the case worth alerting on.
    let plugin = FakePlugin::with_put_failures(1);
    // Pre-seed the fence key so the injected failure hits the VALUE write
    // (mid create-saga), not the fence-key bootstrap put.
    plugin.seed_fence_key();
    let metrics = FakeMetrics::new();
    let svc = make_service(
        Arc::new(FakeSecretRepo::with_delete_failure()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    _ = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::create(SharingMode::Tenant),
        )
        .await;

    assert_eq!(
        metrics.provisioning_rollbacks(),
        vec![Outcome::Error],
        "a failed rollback must record an Error-outcome metric"
    );
}

#[tokio::test]
async fn post_create_only_race_returns_conflict_immediately() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("conly");

    // A Provisioning winner row; create_only POST must 409 without retrying.
    // (No promote_on_conflict needed: create_only returns before the retry loop.)
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "conly".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Provisioning,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::create(SharingMode::Tenant),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::Conflict)),
        "create-only race -> 409, got {result:?}"
    );
}

// ── optimistic concurrency (If-Match precondition) ───────────────────────────

#[tokio::test]
async fn put_if_match_matching_version_overwrites_and_bumps() {
    use crate::domain::secret::model::WritePrecondition;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("oc");
    let repo = Arc::new(FakeSecretRepo::new());
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    // Create at version 1, then overwrite with the matching If-Match.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    let generation = svc.get(&ctx, &k).await.expect("get").expect("present").id;
    svc.put(
        &ctx,
        &k,
        SecretValue::from("v2"),
        WriteSpec::upsert(SharingMode::Tenant).with_precondition(Some(
            WritePrecondition::Version {
                id: generation,
                version: 1,
            },
        )),
    )
    .await
    .expect("matching If-Match must succeed");

    let got = svc.get(&ctx, &k).await.expect("get").expect("some");
    assert_eq!(got.version, 2, "matching If-Match bumps the version");
    assert_eq!(got.value.as_bytes(), b"v2");
}

#[tokio::test]
async fn put_overwrite_touch_zero_rows_maps_to_version_conflict() {
    // Overwrite path: the backend `plugin.put` commits, but the gated `touch`
    // matches 0 rows because the row was concurrently deleted/reaped between
    // `find_for_write` and the version bump. Even without a precondition this
    // must surface a retryable VersionConflict (canonical Aborted/409) rather
    // than acknowledging a write no active row makes readable.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("oc-put-race");
    // `touch_not_found` only forces `touch` to return 0 rows; create (which uses
    // insert_provisioning + mark_active) still seeds the row, so the second put
    // takes the overwrite path.
    let repo = Arc::new(FakeSecretRepo::with_touch_not_found());
    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create seeds the row");

    let res = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v2"),
            WriteSpec::upsert(SharingMode::Tenant),
        )
        .await;
    assert!(
        matches!(res, Err(DomainError::VersionConflict)),
        "row-vanished-on-commit (no precondition) -> VersionConflict, got {res:?}"
    );
}

#[tokio::test]
async fn put_if_match_stale_version_conflicts_without_writing() {
    use crate::domain::secret::model::WritePrecondition;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("oc-stale");
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    let generation = svc.get(&ctx, &k).await.expect("get").expect("present").id;

    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v2"),
            WriteSpec::upsert(SharingMode::Tenant).with_precondition(Some(
                WritePrecondition::Version {
                    id: generation,
                    version: 99,
                },
            )),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::VersionConflict)),
        "stale If-Match must conflict, got {result:?}"
    );

    // The stale write must NOT have touched the backend value or the version.
    let got = svc.get(&ctx, &k).await.expect("get").expect("some");
    assert_eq!(got.version, 1, "stale write must not bump the version");
    assert_eq!(
        got.value.as_bytes(),
        b"v1",
        "stale write must not overwrite"
    );
}

#[tokio::test]
async fn put_if_match_on_missing_secret_conflicts() {
    use crate::domain::secret::model::WritePrecondition;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("oc-missing");
    let repo = Arc::new(FakeSecretRepo::new());
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    let result = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v"),
            WriteSpec::upsert(SharingMode::Tenant).with_precondition(Some(
                WritePrecondition::Version {
                    id: Uuid::new_v4(),
                    version: 1,
                },
            )),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::VersionConflict)),
        "If-Match on a non-existent secret must conflict, got {result:?}"
    );
    assert_eq!(
        repo.inventory().await.expect("inventory").provisioning,
        0,
        "conflicting create must not insert a row"
    );
}

#[tokio::test]
async fn delete_if_match_stale_version_conflicts() {
    use crate::domain::secret::model::WritePrecondition;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("oc-del");
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        FakeMetrics::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    let generation = svc.get(&ctx, &k).await.expect("get").expect("present").id;

    let result = svc
        .delete(
            &ctx,
            &k,
            Some(WritePrecondition::Version {
                id: generation,
                version: 99,
            }),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::VersionConflict)),
        "stale If-Match delete must conflict, got {result:?}"
    );
    // The secret must still be there.
    assert!(
        svc.get(&ctx, &k).await.expect("get").is_some(),
        "stale delete must not remove the secret"
    );

    // Matching validator deletes.
    svc.delete(
        &ctx,
        &k,
        Some(WritePrecondition::Version {
            id: generation,
            version: 1,
        }),
    )
    .await
    .expect("matching If-Match delete must succeed");
    assert!(svc.get(&ctx, &k).await.expect("get").is_none());
}

#[tokio::test]
async fn delete_if_match_race_maps_zero_rows_to_version_conflict() {
    use crate::domain::secret::model::WritePrecondition;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("oc-del-race");

    // Row exists at v1 (find_own + pre-check pass), but the gated
    // mark_deprovisioning matches 0 rows — the row moved/vanished between the
    // pre-check and the commit.
    let repo = Arc::new(FakeSecretRepo::with_mark_not_found());
    let row_id = Uuid::new_v4();
    repo.seed(SecretRow {
        id: row_id,
        tenant_id: TenantId(tenant),
        reference: "oc-del-race".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    // Under a version precondition, the 0-row flip is an optimistic-lock
    // conflict, not a misleading 404.
    let conflict = svc
        .delete(
            &ctx,
            &k,
            Some(WritePrecondition::Version {
                id: row_id,
                version: 1,
            }),
        )
        .await;
    assert!(
        matches!(conflict, Err(DomainError::VersionConflict)),
        "0-row mark under precondition -> VersionConflict, got {conflict:?}"
    );

    // Without a precondition, the same 0-row flip stays a NotFound.
    let plain = svc.delete(&ctx, &k, None).await;
    assert!(
        matches!(plain, Err(DomainError::NotFound)),
        "0-row mark without precondition -> NotFound, got {plain:?}"
    );
}

// ── deprovisioning saga ───────────────────────────────────────────────────────

#[tokio::test]
async fn delete_backend_failure_leaves_deprovisioning_row_and_hides_secret() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("dp-fail");
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::with_delete_failures(1);
    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");

    // Backend delete fails: the caller gets the error, the row stays
    // deprovisioning, and the secret already no longer resolves.
    let err = svc.delete(&ctx, &k, None).await.expect_err("backend fails");
    assert!(matches!(err, DomainError::Internal { .. }), "got {err:?}");
    let rows = repo.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, SecretStatus::Deprovisioning);
    assert!(
        svc.get(&ctx, &k).await.expect("get").is_none(),
        "deprovisioning secret must not resolve"
    );

    // A DELETE retry resumes the saga (plugin recovered) and finishes cleanup.
    svc.delete(&ctx, &k, None)
        .await
        .expect("retry resumes saga");
    assert!(repo.rows().is_empty(), "row removed after resumed saga");
}

#[tokio::test]
async fn delete_while_deprovisioning_recreate_conflicts_until_cleanup() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("dp-held");
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::with_delete_failures(1);
    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    svc.delete(&ctx, &k, None)
        .await
        .expect_err("backend delete fails; row wedges in deprovisioning");

    // The deprovisioning row still holds the unique index: a re-create of the
    // same reference conflicts (retryable) until cleanup completes.
    let err = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v2"),
            WriteSpec::upsert(SharingMode::Tenant),
        )
        .await
        .expect_err("name still held");
    assert!(matches!(err, DomainError::Conflict), "got {err:?}");
}

#[tokio::test]
async fn reaper_completes_stuck_deprovisioning_and_reconciles_backend() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("dp-reap");
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::with_delete_failures(1);
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
        metrics.clone(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    svc.delete(&ctx, &k, None)
        .await
        .expect_err("backend delete fails once");

    // Value still in the backend, row stuck deprovisioning.
    assert!(plugin.contains(&TenantId(tenant), &k, None));

    // The reaper retries the backend delete and removes the row.
    svc.reap_and_refresh().await;
    assert!(repo.rows().is_empty(), "stuck deprovisioning row reaped");
    assert!(
        !plugin.contains(&TenantId(tenant), &k, None),
        "backend value reconciled by the reaper"
    );
    assert_eq!(metrics.deprovisioning_reaped_total(), 1);
}

#[tokio::test]
async fn reaper_keeps_deprovisioning_row_while_backend_still_fails() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("dp-keep");
    let repo = Arc::new(FakeSecretRepo::new());
    // Fails the saga's delete AND the reaper's retry.
    let plugin = FakePlugin::with_delete_failures(2);
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    svc.delete(&ctx, &k, None)
        .await
        .expect_err("backend delete fails");

    svc.reap_and_refresh().await;
    let rows = repo.rows();
    assert_eq!(
        rows.len(),
        1,
        "row must be kept while the backend still holds the value"
    );
    assert_eq!(rows[0].status, SecretStatus::Deprovisioning);
    assert_eq!(metrics.deprovisioning_reaped_total(), 0);

    // Next tick, the backend recovered: cleanup completes.
    svc.reap_and_refresh().await;
    assert!(repo.rows().is_empty());
    assert_eq!(metrics.deprovisioning_reaped_total(), 1);
}

#[tokio::test]
async fn reaper_reconciles_backend_for_stuck_provisioning_rows() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let k = key("pv-orphan");
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
        metrics.clone(),
    );

    // Simulate a crash between plugin.put and mark_active: provisioning row
    // plus an orphaned backend value.
    let id = Uuid::new_v4();
    repo.seed(SecretRow {
        id,
        tenant_id: TenantId(tenant),
        reference: "pv-orphan".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Provisioning,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let ctx = make_ctx(subject, tenant);
    plugin
        .put(
            &ctx,
            &TenantId(tenant),
            &k,
            SecretValue::from("orphan"),
            None,
        )
        .await
        .expect("seed backend value");

    svc.reap_and_refresh().await;
    assert!(repo.rows().is_empty(), "stuck provisioning row reaped");
    assert!(
        !plugin.contains(&TenantId(tenant), &k, None),
        "orphaned backend value reconciled"
    );
    assert_eq!(metrics.provisioning_reaped_total(), 1);
}

/// Regression: a slow create whose `mark_active` lands *between* the reaper's
/// list and its delete must survive — the reaper must not delete the row nor
/// the backend value out from under a create the client saw succeed. The
/// status-gated delete loses the race and leaves the now-active secret intact.
#[tokio::test]
async fn reaper_does_not_reap_provisioning_row_that_raced_to_active() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let k = key("pv-won-race");
    // list_stale_pending returns the row as Provisioning but flips the stored
    // row to Active — exactly the mark_active-after-list interleaving.
    let repo = Arc::new(FakeSecretRepo::with_provisioning_promoted_on_list());
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
        metrics.clone(),
    );

    let id = Uuid::new_v4();
    repo.seed(SecretRow {
        id,
        tenant_id: TenantId(tenant),
        reference: "pv-won-race".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Provisioning,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let ctx = make_ctx(subject, tenant);
    plugin
        .put(
            &ctx,
            &TenantId(tenant),
            &k,
            SecretValue::from("winner"),
            None,
        )
        .await
        .expect("seed backend value");

    svc.reap_and_refresh().await;

    let rows = repo.rows();
    assert_eq!(rows.len(), 1, "the raced-to-active row must survive");
    assert_eq!(rows[0].status, SecretStatus::Active);
    assert!(
        plugin.contains(&TenantId(tenant), &k, None),
        "backend value of the succeeded create must NOT be deleted"
    );
    assert_eq!(metrics.provisioning_reaped_total(), 0, "nothing was reaped");
}

// ── secret types ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn typed_create_enforces_allow_sharing_and_returns_type() {
    use credstore_sdk::{ExpiryWrite, SecretType, WriteOptions};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("typed");
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    let personal = WriteOptions {
        secret_type: Some(
            SecretType::from_name("personal-token")
                .expect("known")
                .into(),
        ),
        expires_at: ExpiryWrite::Preserve,
        precondition: None,
    };
    // personal-token is private-only: tenant sharing is a type violation.
    let err = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("t"),
            WriteSpec::upsert(SharingMode::Tenant).with_opts(personal.clone()),
        )
        .await
        .expect_err("sharing not allowed for type");
    assert!(
        matches!(err, DomainError::TypeViolation { reason, .. } if reason == "SHARING_NOT_ALLOWED_FOR_TYPE"),
    );

    // Private is fine; the read reports the type.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("t"),
        WriteSpec::upsert(SharingMode::Private).with_opts(personal),
    )
    .await
    .expect("private personal token ok");
    let got = svc.get(&ctx, &k).await.expect("get").expect("resolves");
    assert_eq!(
        got.secret_type,
        SecretType::from_name("personal-token")
            .expect("known")
            .gts_id()
    );
}

#[tokio::test]
async fn secret_type_is_immutable_on_overwrite() {
    use credstore_sdk::{ExpiryWrite, SecretType, WriteOptions};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("immutable");
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("k"),
        WriteSpec::upsert(SharingMode::Tenant).with_opts(WriteOptions {
            secret_type: Some(SecretType::from_name("api-key").expect("known").into()),
            expires_at: ExpiryWrite::Preserve,
            precondition: None,
        }),
    )
    .await
    .expect("create api-key");

    // Explicit differing type on overwrite is rejected.
    let err = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("k2"),
            WriteSpec::upsert(SharingMode::Tenant).with_opts(WriteOptions {
                secret_type: Some(SecretType::generic().into()),
                expires_at: ExpiryWrite::Preserve,
                precondition: None,
            }),
        )
        .await
        .expect_err("type immutable");
    assert!(matches!(err, DomainError::TypeViolation { reason, .. } if reason == "TYPE_IMMUTABLE"));

    // Absent type inherits the existing one — overwrite succeeds, type kept.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("k3"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("untyped overwrite keeps type");
    let got = svc.get(&ctx, &k).await.expect("get").expect("resolves");
    assert_eq!(
        got.secret_type,
        SecretType::from_name("api-key").expect("known").gts_id()
    );
}

#[tokio::test]
async fn expired_secret_stops_resolving_and_is_swept_via_deprovisioning() {
    use credstore_sdk::{ExpiryWrite, SecretType, WriteOptions};
    use time::{Duration as TimeDuration, OffsetDateTime};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("expiring");
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
        metrics.clone(),
    );

    // The window must comfortably exceed one create+get. It is generous
    // because nextest runs each test in its own process, so this test pays the
    // crypto backend's one-time initialization (AWS-LC, on the first fence op)
    // inside the timed section — a tight 20ms budget flaked on slow CI runners.
    let ttl = TimeDuration::milliseconds(500);
    svc.put(
        &ctx,
        &k,
        SecretValue::from("short-lived"),
        WriteSpec::upsert(SharingMode::Tenant).with_opts(WriteOptions {
            secret_type: Some(SecretType::from_name("bearer-token").expect("known").into()),
            expires_at: ExpiryWrite::Set(OffsetDateTime::now_utc() + ttl),
            precondition: None,
        }),
    )
    .await
    .expect("create expiring secret");
    assert!(svc.get(&ctx, &k).await.expect("get").is_some());

    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    // Expired: resolution misses even before the reaper runs.
    assert!(
        svc.get(&ctx, &k).await.expect("get").is_none(),
        "expired secret must not resolve"
    );

    // The reaper flips it to deprovisioning and completes the cleanup
    // (fake list_stale_pending treats every non-active row as stale).
    svc.reap_and_refresh().await;
    assert!(repo.rows().is_empty(), "expired row fully reaped");
    assert!(
        !plugin.contains(&TenantId(tenant), &k, None),
        "backend value reconciled"
    );
    assert_eq!(metrics.deprovisioning_reaped_total(), 1);
}

#[tokio::test]
async fn expiry_rejected_for_non_expirable_type() {
    use credstore_sdk::{ExpiryWrite, WriteOptions};
    use time::{Duration as TimeDuration, OffsetDateTime};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    // generic is not expirable: expires_at is a type violation.
    let err = svc
        .put(
            &ctx,
            &key("no-expiry"),
            SecretValue::from("v"),
            WriteSpec::upsert(SharingMode::Tenant).with_opts(WriteOptions {
                secret_type: None,
                expires_at: ExpiryWrite::Set(OffsetDateTime::now_utc() + TimeDuration::hours(1)),
                precondition: None,
            }),
        )
        .await
        .expect_err("generic has no expiry");
    assert!(
        matches!(err, DomainError::TypeViolation { reason, .. } if reason == "EXPIRY_NOT_SUPPORTED_FOR_TYPE"),
    );
}

// ── per-type authorization ────────────────────────────────────────────────────

#[tokio::test]
async fn per_type_pdp_denial_hides_reads_and_forbids_writes() {
    use credstore_sdk::{ExpiryWrite, SecretType, WriteOptions};
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("typed-authz");
    let api_key = SecretType::from_name("api-key").expect("known");

    // PDP allows the base type but denies the concrete api-key type.
    let (enforcer, resolver) =
        crate::domain::secret::test_support::type_deny_enforcer(vec![api_key.gts_id().to_owned()]);
    let repo = Arc::new(FakeSecretRepo::new());
    // Seed an existing active api-key secret directly (writes are denied).
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "typed-authz".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: api_key.uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let svc = make_service_noop(
        repo,
        Arc::new(FakeDir::single(tenant)),
        enforcer,
        FakePlugin::new(),
    );

    // Read: per-type denial is the anti-enumeration 404, not a 403.
    assert!(
        svc.get(&ctx, &k).await.expect("get ok").is_none(),
        "type-denied secret must resolve as not-found"
    );

    // Overwrite: operation-level denial.
    let err = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("v2"),
            WriteSpec::upsert(SharingMode::Tenant),
        )
        .await
        .expect_err("typed write denied");
    assert!(
        matches!(err, DomainError::AccessDenied { .. }),
        "got {err:?}"
    );

    // Create of a new typed secret: denied before any side effect.
    let err = svc
        .put(
            &ctx,
            &key("new-typed"),
            SecretValue::from("v"),
            WriteSpec::upsert(SharingMode::Tenant).with_opts(WriteOptions {
                secret_type: Some(api_key.into()),
                expires_at: ExpiryWrite::Preserve,
                precondition: None,
            }),
        )
        .await
        .expect_err("typed create denied");
    assert!(
        matches!(err, DomainError::AccessDenied { .. }),
        "got {err:?}"
    );

    // Delete: operation-level denial.
    let err = svc
        .delete(&ctx, &k, None)
        .await
        .expect_err("typed delete denied");
    assert!(
        matches!(err, DomainError::AccessDenied { .. }),
        "got {err:?}"
    );

    // The PEP targeted the concrete type id (not only the base type).
    let seen = resolver.seen_resource_types.lock().expect("lock").clone();
    assert!(
        seen.iter().any(|t| t == api_key.gts_id()),
        "PDP must be evaluated against the concrete type id, saw: {seen:?}"
    );
}

#[tokio::test]
async fn generic_secrets_evaluate_the_full_concrete_type() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("plain");

    let (enforcer, resolver) = crate::domain::secret::test_support::type_deny_enforcer(Vec::new());
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        enforcer,
        FakePlugin::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("generic write");
    assert!(svc.get(&ctx, &k).await.expect("get").is_some());

    // Every credstore operation authorizes against the secret's full concrete
    // type — including `generic` — never the bare base type. This is what lets
    // a policy target `...generic.v1~` specifically.
    let generic_id = credstore_sdk::SecretType::generic().gts_id();
    let seen = resolver.seen_resource_types.lock().expect("lock").clone();
    assert!(!seen.is_empty(), "the PDP must be consulted");
    assert!(
        seen.iter().all(|t| t == generic_id),
        "generic operations must evaluate the full generic type id {generic_id:?}, saw: {seen:?}"
    );
}

// ── value-fingerprint fence (docs/features/001-value-fingerprint-fence.md) ───

/// The fence key `FakePlugin::seed_fence_key` installs.
const TEST_FENCE_KEY: [u8; 32] = [42u8; 32];

#[tokio::test]
async fn crosswise_lww_desync_reads_fail_closed_not_disclosed() {
    use crate::domain::ports::metrics::{FenceVerify, ReadOutcome};
    use crate::domain::secret::fence;
    // The poisoned end-state of two crosswise unconditional PUTs (finding #2):
    // Alice touched the row last (sharing=shared, fp of HER value), but Bob's
    // tenant-intent value landed last in the backend. Without the fence a
    // child-tenant GET would serve Bob's value under Alice's `shared` label.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("crosswise");

    let plugin = FakePlugin::new();
    plugin.seed_fence_key();
    let fp_alice = fence::compute_fp(&TEST_FENCE_KEY, b"value-alice");
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "crosswise".into(),
        sharing: SharingMode::Shared,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 2,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: Some(fp_alice),
        fp_key_id: Some(fence::CURRENT_FENCE_KEY_ID),
    });
    // Bob's value is what the backend actually holds.
    plugin
        .put(
            &ctx,
            &TenantId(tenant),
            &k,
            SecretValue::from("value-bob"),
            None,
        )
        .await
        .expect("backend value");

    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo,
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    // Fail closed: no value under a foreign sharing label, ever.
    let got = svc.get(&ctx, &k).await.expect("get is Ok");
    assert!(
        got.is_none(),
        "a fingerprint mismatch must be an anti-enumeration miss, got a value"
    );
    assert!(
        metrics.fence_verifies().contains(&FenceVerify::Mismatch),
        "the mismatch must be observable, got {:?}",
        metrics.fence_verifies()
    );
    assert_eq!(metrics.last_read_outcome(), Some(ReadOutcome::Miss));

    // Any subsequent successful PUT heals the reference.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("value-heal"),
        WriteSpec::upsert(SharingMode::Shared),
    )
    .await
    .expect("healing put");
    let healed = svc.get(&ctx, &k).await.expect("get").expect("healed");
    assert_eq!(healed.value.as_bytes(), b"value-heal");
}

#[tokio::test]
async fn clean_write_then_read_verifies_ok() {
    use crate::domain::ports::metrics::FenceVerify;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("clean");
    let metrics = FakeMetrics::new();
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
        metrics.clone(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    let got = svc.get(&ctx, &k).await.expect("get").expect("present");
    assert_eq!(got.value.as_bytes(), b"v1");
    assert_eq!(
        metrics.fence_verifies(),
        vec![FenceVerify::Ok],
        "an API-created secret must verify Ok (stamped on create)"
    );
}

#[tokio::test]
async fn overwrite_restamps_the_fingerprint() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("restamp");
    let repo = Arc::new(FakeSecretRepo::new());
    let svc = make_service_noop(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create");
    let fp1 = repo.rows()[0].value_fp.clone().expect("stamped on create");

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v2"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("overwrite");
    let fp2 = repo.rows()[0].value_fp.clone().expect("still stamped");
    assert_ne!(fp1, fp2, "a new value must get a new fingerprint");
    // And the overwritten secret still reads back (fp matches the new value).
    let got = svc.get(&ctx, &k).await.expect("get").expect("present");
    assert_eq!(got.value.as_bytes(), b"v2");
}

#[tokio::test]
async fn aba_recreate_rejects_stale_generation_validator() {
    use crate::domain::secret::model::WritePrecondition;
    // Finding #1: delete + recreate restarts the version counter at 1, so a
    // bare-version validator from the earlier generation would match again.
    // The generation-bound validator must reject it.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("aba");
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        FakePlugin::new(),
    );

    // Generation 1.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("gen1"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create gen1");
    let gen1 = svc.get(&ctx, &k).await.expect("get").expect("gen1");
    let stale_validator = WritePrecondition::Version {
        id: gen1.id,
        version: gen1.version,
    };

    // Delete + recreate: generation 2, version counter restarts at 1.
    svc.delete(&ctx, &k, None).await.expect("delete gen1");
    svc.put(
        &ctx,
        &k,
        SecretValue::from("gen2"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("create gen2");
    let gen2 = svc.get(&ctx, &k).await.expect("get").expect("gen2");
    assert_eq!(
        gen2.version, gen1.version,
        "the ABA precondition: version counters coincide across generations"
    );
    assert_ne!(gen2.id, gen1.id, "a recreated secret is a new generation");

    // The stale validator (same version number, old generation) must NOT match.
    let res = svc
        .put(
            &ctx,
            &k,
            SecretValue::from("stale-overwrite"),
            WriteSpec::upsert(SharingMode::Tenant).with_precondition(Some(stale_validator)),
        )
        .await;
    assert!(
        matches!(res, Err(DomainError::VersionConflict)),
        "a stale generation's validator must conflict, got {res:?}"
    );
    let unchanged = svc.get(&ctx, &k).await.expect("get").expect("present");
    assert_eq!(unchanged.value.as_bytes(), b"gen2", "no lost update");

    // The current generation's validator works.
    svc.put(
        &ctx,
        &k,
        SecretValue::from("gen2-v2"),
        WriteSpec::upsert(SharingMode::Tenant).with_precondition(Some(
            WritePrecondition::Version {
                id: gen2.id,
                version: gen2.version,
            },
        )),
    )
    .await
    .expect("current validator must match");
}

#[tokio::test]
async fn legacy_seeded_row_serves_and_backfills_without_version_bump() {
    use crate::domain::ports::metrics::{FenceVerify, Outcome};
    use crate::domain::secret::fence;
    // Out-of-band seeding contract: row inserted with value_fp = NULL, value
    // placed directly in the backend. Served on trust, stamped on first read,
    // ETag/version untouched by the backfill.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("seeded");

    let plugin = FakePlugin::new();
    plugin.seed_fence_key();
    plugin.seed_value(&TenantId(tenant), &k, None, b"seeded-value");
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "seeded".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    });
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    let got = svc.get(&ctx, &k).await.expect("get").expect("served");
    assert_eq!(got.value.as_bytes(), b"seeded-value");
    assert_eq!(got.version, 1, "backfill must not bump the version");
    assert_eq!(metrics.fence_verifies(), vec![FenceVerify::Legacy]);
    assert_eq!(metrics.fence_backfills(), vec![Outcome::Success]);

    // The row is now fenced with the fp of the seeded value under key id 1.
    let row = &repo.rows()[0];
    let expected_fp = fence::compute_fp(&TEST_FENCE_KEY, b"seeded-value");
    assert_eq!(row.value_fp.as_deref(), Some(expected_fp.as_slice()));
    assert_eq!(row.fp_key_id, Some(fence::CURRENT_FENCE_KEY_ID));
    assert_eq!(row.version, 1);

    // A second read verifies Ok (no longer legacy, no second backfill).
    let again = svc.get(&ctx, &k).await.expect("get").expect("served");
    assert_eq!(again.value.as_bytes(), b"seeded-value");
    assert_eq!(
        metrics.fence_verifies(),
        vec![FenceVerify::Legacy, FenceVerify::Ok]
    );
    assert_eq!(metrics.fence_backfills(), vec![Outcome::Success]);
}

#[tokio::test]
async fn fence_key_bootstrap_persists_the_key_in_the_backend() {
    use crate::domain::secret::fence;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("boot");
    let plugin = FakePlugin::new();
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
    );

    let fence_ref = key(fence::FENCE_KEY_REF);
    assert!(
        !plugin.contains(&TenantId(Uuid::nil()), &fence_ref, None),
        "virgin deployment: no fence key yet"
    );
    svc.put(
        &ctx,
        &k,
        SecretValue::from("v"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("first write bootstraps the key");
    assert!(
        plugin.contains(&TenantId(Uuid::nil()), &fence_ref, None),
        "the generated fence key must be persisted under the reserved nil-tenant entry"
    );
    // And the write it fenced reads back.
    assert!(svc.get(&ctx, &k).await.expect("get").is_some());
}

#[tokio::test]
async fn fence_key_bootstrap_adopts_existing_key_without_overwriting() {
    // Bootstrap must ADOPT a fence key already present in the backend (as if a
    // peer replica won the first-boot race), never regenerate/overwrite it —
    // overwriting would restart the race and poison peers' fingerprints.
    use crate::domain::secret::fence;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("boot");
    let plugin = FakePlugin::new();

    // Pre-seed a distinctive fence key under the reserved nil-tenant entry.
    let seeded = vec![7u8; fence::FENCE_KEY_LEN];
    let fence_ref = key(fence::FENCE_KEY_REF);
    plugin.seed_value(&TenantId(Uuid::nil()), &fence_ref, None, &seeded);

    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
    );

    svc.put(
        &ctx,
        &k,
        SecretValue::from("v"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("write adopts the existing fence key");

    let stored = plugin
        .get(&ctx, &TenantId(Uuid::nil()), &fence_ref, None)
        .await
        .expect("plugin get ok")
        .expect("fence key still present");
    assert_eq!(
        stored.as_bytes(),
        seeded.as_slice(),
        "bootstrap must adopt the existing fence key, never overwrite it"
    );
    // The write, fenced under the adopted key, verifies and reads back.
    assert!(svc.get(&ctx, &k).await.expect("get").is_some());
}

#[tokio::test]
async fn stale_cached_key_self_heals_via_refresh_on_mismatch() {
    use crate::domain::ports::metrics::FenceVerify;
    use crate::domain::secret::fence;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);

    let plugin = FakePlugin::new();
    let repo = Arc::new(FakeSecretRepo::new());
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
        metrics.clone(),
    );

    // Warm this replica's cache: the first write generates + caches a key.
    let warm = key("warm");
    svc.put(
        &ctx,
        &warm,
        SecretValue::from("w"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("warm the key cache");

    // Another replica re-created the backend key entry (K2) and wrote a
    // secret stamped under it; this replica still caches the old key.
    let k2 = [7u8; 32];
    let fence_ref = key(fence::FENCE_KEY_REF);
    plugin.seed_value(&TenantId(Uuid::nil()), &fence_ref, None, &k2);
    let k = key("foreign");
    plugin.seed_value(&TenantId(tenant), &k, None, b"vx");
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "foreign".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: Some(fence::compute_fp(&k2, b"vx")),
        fp_key_id: Some(fence::CURRENT_FENCE_KEY_ID),
    });

    // The stale cached key fails the first verify; the one-shot refresh
    // re-reads K2 from the backend and the read succeeds.
    let got = svc.get(&ctx, &k).await.expect("get").expect("self-healed");
    assert_eq!(got.value.as_bytes(), b"vx");
    assert!(
        metrics.fence_verifies().contains(&FenceVerify::Ok),
        "refresh-on-mismatch must converge to Ok, got {:?}",
        metrics.fence_verifies()
    );
    assert!(
        !metrics.fence_verifies().contains(&FenceVerify::Mismatch),
        "a healed read must not count as a mismatch"
    );
}

#[tokio::test]
async fn reaper_backfill_sweep_stamps_unread_seeded_rows() {
    use crate::domain::ports::metrics::Outcome;
    use crate::domain::secret::fence;
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);

    let plugin = FakePlugin::new();
    plugin.seed_fence_key();
    let repo = Arc::new(FakeSecretRepo::new());
    let seed_row = |reference: &str| SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: reference.to_owned(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: None,
        fp_key_id: None,
    };
    repo.seed(seed_row("swept-a"));
    repo.seed(seed_row("swept-b"));
    // A seeded row whose backend value is missing: nothing to stamp.
    repo.seed(seed_row("valueless"));
    plugin.seed_value(&TenantId(tenant), &key("swept-a"), None, b"va");
    plugin.seed_value(&TenantId(tenant), &key("swept-b"), None, b"vb");
    let _ = ctx; // rows seeded directly; no API traffic in this test

    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin,
        metrics.clone(),
    );

    svc.reap_and_refresh().await;

    let rows = repo.rows();
    let fp_of = |reference: &str| {
        rows.iter()
            .find(|r| r.reference == reference)
            .expect("row")
            .value_fp
            .clone()
    };
    assert_eq!(
        fp_of("swept-a").as_deref(),
        Some(fence::compute_fp(&TEST_FENCE_KEY, b"va").as_slice())
    );
    assert!(fp_of("swept-b").is_some());
    assert!(
        fp_of("valueless").is_none(),
        "a row with no backend value stays unfenced (served-on-trust semantics)"
    );
    assert_eq!(
        metrics.fence_backfills(),
        vec![Outcome::Success, Outcome::Success]
    );
}

#[tokio::test]
async fn fence_key_reference_is_unreachable_through_the_api() {
    use credstore_sdk::CredStorePluginClientV1;

    use crate::domain::secret::fence;
    // The reserved entry has no metadata row, so no API path resolves it; a
    // tenant writing the same reference gets an ordinary tenant-scoped secret
    // that never collides with the nil-tenant key entry.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let fence_ref = key(fence::FENCE_KEY_REF);

    let plugin = FakePlugin::new();
    plugin.seed_fence_key();
    let svc = make_service_noop(
        Arc::new(FakeSecretRepo::new()),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        plugin.clone(),
    );

    // Not resolvable: no row exists for it.
    assert!(
        svc.get(&ctx, &fence_ref)
            .await
            .expect("get is Ok")
            .is_none(),
        "the reserved fence-key entry must not resolve through the API"
    );

    // A tenant PUT to the same reference is an ordinary tenant secret…
    svc.put(
        &ctx,
        &fence_ref,
        SecretValue::from("own-secret"),
        WriteSpec::upsert(SharingMode::Tenant),
    )
    .await
    .expect("tenant-scoped write under the same name is fine");
    let got = svc
        .get(&ctx, &fence_ref)
        .await
        .expect("get")
        .expect("their own secret");
    assert_eq!(got.value.as_bytes(), b"own-secret");

    // …and the nil-tenant key entry is untouched.
    let nil_ctx = make_ctx(Uuid::nil(), Uuid::nil());
    let stored = plugin
        .get(&nil_ctx, &TenantId(Uuid::nil()), &fence_ref, None)
        .await
        .expect("plugin get")
        .expect("key entry present");
    assert_eq!(
        stored.as_bytes(),
        TEST_FENCE_KEY,
        "a tenant write must never clobber the reserved key entry"
    );
}

// ── review-finding follow-ups (#9) ───────────────────────────────────────────

#[tokio::test]
async fn delete_with_no_plugin_fails_without_marking_deprovisioning() {
    use crate::domain::secret::test_support::NoPluginSelector;
    // #9: DELETE must resolve the plugin BEFORE flipping the row to
    // deprovisioning (symmetric with put's fail-fast). With no plugin the
    // delete 503s and the row stays Active — the secret keeps resolving and
    // the reference is not wedged.
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let ctx = make_ctx(subject, tenant);
    let k = key("np-del");

    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: "np-del".into(),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(subject),
        status: SecretStatus::Active,
        version: 1,
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_fp: Some(vec![1u8; 32]),
        fp_key_id: Some(1),
    });
    let svc = Service::new(
        repo.clone(),
        Arc::new(FakeDir::single(tenant)),
        mock_enforcer(),
        Arc::new(NoPluginSelector),
        catalog_type_resolver(),
        Arc::new(NoopMetrics),
        test_reaper_settings(),
    );

    let res = svc.delete(&ctx, &k, None).await;
    assert!(
        matches!(res, Err(DomainError::ServiceUnavailable { .. })),
        "no plugin -> 503, got {res:?}"
    );
    // The row must NOT have been flipped to deprovisioning.
    let rows = repo.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].status,
        SecretStatus::Active,
        "delete must not mark deprovisioning when no plugin is available"
    );
}
