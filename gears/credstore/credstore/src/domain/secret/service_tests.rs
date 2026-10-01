//! Unit tests for the credential-store domain [`Service`] (ADR-0004: the
//! credential surface; ADR-0006: immutable value versions).
//!
//! This file covers the credential/secret split (`get`/`get_secret`), the
//! merged write surface (`put`/`patch`), suppression (`fallback`), the write
//! protocol (create, overwrite, orphaned puts, lost and ambiguous CAS,
//! concurrent last-writer-wins, with and without `destroy` support), the
//! re-read-once read protocol, and delete with its outbox purge.

use std::sync::Arc;

use credstore_sdk::{CredentialPatch, CredentialStatus, CredentialWrite};
use credstore_sdk::{
    DestroySelector, Fallback as SdkFallback, InheritanceStatus, OwnerId, PatchField, SecretRef,
    SecretType, SecretValue, SharingMode, TenantId, ValueVersion,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    CredStoreMetricsPort, Dep, DepOp, Outcome, ReadOutcome, ReadRetryOutcome,
};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use crate::domain::secret::model::{Fallback, PutPrecondition, SecretStatus, WritePrecondition};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::service::{ListSettings, Service};
use crate::domain::secret::test_support::*;

fn key(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid ref")
}

fn test_list_settings() -> ListSettings {
    ListSettings {
        max_limit: 200,
        secret_mode_cap: 25,
    }
}

fn make_service(
    repo: Arc<dyn SecretRepo>,
    plugin: Arc<FakePlugin>,
    dir: Arc<dyn TenantDirectory>,
    enforcer: authz_resolver_sdk::PolicyEnforcer,
    metrics: Arc<dyn CredStoreMetricsPort>,
) -> Service {
    Service::new(
        repo,
        dir,
        enforcer,
        Arc::new(FakePluginSelector::new(plugin)) as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        metrics,
        test_list_settings(),
    )
}

fn make_service_noop(
    repo: Arc<dyn SecretRepo>,
    plugin: Arc<FakePlugin>,
    dir: Arc<dyn TenantDirectory>,
) -> Service {
    make_service(repo, plugin, dir, mock_enforcer(), Arc::new(NoopMetrics))
}

// ── WritePrecondition (patch/delete) helpers ────────────────────────────────

fn exists() -> WritePrecondition {
    WritePrecondition::Exists
}

fn matches(id: Uuid, version: i64) -> WritePrecondition {
    WritePrecondition::Version { id, version }
}

// ── PutPrecondition (put) helpers ───────────────────────────────────────────

fn create_only() -> PutPrecondition {
    PutPrecondition::CreateOnly
}

fn put_exists() -> PutPrecondition {
    PutPrecondition::Exists
}

fn put_matches(id: Uuid, version: i64) -> PutPrecondition {
    PutPrecondition::Version { id, version }
}

// ── CredentialWrite / CredentialPatch builders ──────────────────────────────

/// A `CredentialWrite` for a create (`secret_type` required by ADR-0004),
/// generic type, `fallback: inherit`, no expiry.
fn write_create(sharing: SharingMode, value: &str) -> CredentialWrite {
    write_create_typed(sharing, value, "generic")
}

fn write_create_typed(sharing: SharingMode, value: &str, type_name: &str) -> CredentialWrite {
    CredentialWrite {
        secret_type: Some(SecretType::from_name(type_name).expect("known type").into()),
        sharing,
        fallback: SdkFallback::Inherit,
        expires_at: None,
        secret: Some(SecretValue::from(value)),
    }
}

/// A `CredentialWrite` for a replace (`secret_type: None` — must equal
/// stored), generic defaults otherwise.
fn write_replace(sharing: SharingMode, value: &str) -> CredentialWrite {
    CredentialWrite {
        secret_type: None,
        sharing,
        fallback: SdkFallback::Inherit,
        expires_at: None,
        secret: Some(SecretValue::from(value)),
    }
}

/// A `CredentialWrite` for a create with an explicit `null` value (ADR-0004
/// Amendment B): the row is inserted `declared`, `fallback` as given.
fn write_create_null(sharing: SharingMode, fallback: SdkFallback) -> CredentialWrite {
    CredentialWrite {
        secret_type: Some(SecretType::generic().into()),
        sharing,
        fallback,
        expires_at: None,
        secret: None,
    }
}

/// A `CredentialWrite` for a replace with an explicit `null` value
/// (ADR-0004 Amendment B): removes an existing value (active row) or leaves
/// an already value-less row's value untouched (declared row), depending on
/// the target's current state.
fn write_replace_null(sharing: SharingMode, fallback: SdkFallback) -> CredentialWrite {
    CredentialWrite {
        secret_type: None,
        sharing,
        fallback,
        expires_at: None,
        secret: None,
    }
}

fn empty_patch() -> CredentialPatch {
    CredentialPatch {
        secret_type: None,
        sharing: None,
        fallback: None,
        expires_at: PatchField::Absent,
        secret: PatchField::Absent,
    }
}

fn patch_value(value: &str) -> CredentialPatch {
    CredentialPatch {
        secret: PatchField::Set(SecretValue::from(value)),
        ..empty_patch()
    }
}

fn patch_sharing(sharing: SharingMode) -> CredentialPatch {
    CredentialPatch {
        sharing: Some(sharing),
        ..empty_patch()
    }
}

fn patch_suppress() -> CredentialPatch {
    CredentialPatch {
        fallback: Some(SdkFallback::None),
        secret: PatchField::Null,
        ..empty_patch()
    }
}

// ── basic read/write/delete happy paths ─────────────────────────────────────

#[tokio::test]
async fn get_own_tenant_secret_returns_hit_own() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        plugin.clone(),
        dir,
        mock_enforcer(),
        metrics.clone(),
    );
    let ctx = make_ctx(subject, tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v1"),
        create_only(),
    )
    .await
    .expect("create");

    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"v1");
    assert_eq!(metrics.last_read_outcome(), Some(ReadOutcome::HitOwn));

    let cred = svc.get(&ctx, &key("k")).await.expect("get").expect("some");
    assert_eq!(cred.inheritance, InheritanceStatus::Own);
    assert_eq!(cred.status, CredentialStatus::Active);
}

#[tokio::test]
async fn get_inherited_shared_from_parent_sets_inherited_status() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);

    let parent_ctx = make_ctx(subject, parent);
    svc.put(
        &parent_ctx,
        &key("shared-k"),
        write_create(SharingMode::Shared, "shared-v"),
        create_only(),
    )
    .await
    .expect("create at parent");

    let child_ctx = make_ctx(Uuid::new_v4(), child);
    let got = svc
        .get_secret(&child_ctx, &key("shared-k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"shared-v");

    let cred = svc
        .get(&child_ctx, &key("shared-k"))
        .await
        .expect("get")
        .expect("some");
    assert_eq!(cred.inheritance, InheritanceStatus::Inherited);
    assert_eq!(cred.status, CredentialStatus::None);
    assert!(
        cred.validator.is_none(),
        "no own row => no strong validator"
    );
}

#[tokio::test]
async fn get_tenant_mode_not_inherited_by_child() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);

    let parent_ctx = make_ctx(Uuid::new_v4(), parent);
    svc.put(
        &parent_ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");

    let child_ctx = make_ctx(Uuid::new_v4(), child);
    let got = svc
        .get_secret(&child_ctx, &key("k"))
        .await
        .expect("get_secret");
    assert!(got.is_none(), "Tenant-mode secret must not be inherited");
    assert!(svc.get(&child_ctx, &key("k")).await.expect("get").is_none());
}

#[tokio::test]
async fn get_private_owner_match_only() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);

    let owner_ctx = make_ctx(owner, tenant);
    svc.put(
        &owner_ctx,
        &key("k"),
        write_create(SharingMode::Private, "v"),
        create_only(),
    )
    .await
    .expect("create");

    let other_ctx = make_ctx(Uuid::new_v4(), tenant);
    assert!(
        svc.get_secret(&other_ctx, &key("k"))
            .await
            .expect("get_secret")
            .is_none()
    );
    assert!(
        svc.get_secret(&owner_ctx, &key("k"))
            .await
            .expect("get_secret")
            .is_some()
    );
}

#[tokio::test]
async fn get_shadowing_private_beats_inherited() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);

    let parent_ctx = make_ctx(owner, parent);
    svc.put(
        &parent_ctx,
        &key("k"),
        write_create(SharingMode::Shared, "shared"),
        create_only(),
    )
    .await
    .expect("create at parent");

    let child_ctx = make_ctx(owner, child);
    svc.put(
        &child_ctx,
        &key("k"),
        write_create(SharingMode::Private, "private"),
        create_only(),
    )
    .await
    .expect("create private at child");

    let got = svc
        .get_secret(&child_ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"private");
    let cred = svc
        .get(&child_ctx, &key("k"))
        .await
        .expect("get")
        .expect("some");
    // The child's own private row wins resolution, but the parent's shared
    // row under the same reference is a resolvable ancestor candidate, so
    // the own row *shadows* it rather than simply "being the only option":
    // per ADR-0004 that is `Overridden`, not `Own`.
    assert_eq!(cred.inheritance, InheritanceStatus::Overridden);
}

// ── dependency metrics ────────────────────────────────────────────────────────

#[tokio::test]
async fn get_secret_records_pdp_dependency_metric() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(repo.clone(), plugin, dir, mock_enforcer(), metrics.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    assert!(
        svc.get_secret(&ctx, &key("absent"))
            .await
            .expect("get_secret")
            .is_none()
    );
    // No row resolves, so the PDP is never consulted (S09 prefetch) —
    // assert the read miss instead.
    assert_eq!(metrics.last_read_outcome(), Some(ReadOutcome::Miss));
}

#[tokio::test]
async fn put_records_pdp_dependency_metric() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(repo, plugin, dir, mock_enforcer(), metrics.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    assert!(
        metrics
            .deps()
            .iter()
            .any(|(d, op, o)| *d == Dep::Pdp && *op == DepOp::Evaluate && *o == Outcome::Success)
    );
    assert!(
        metrics
            .deps()
            .iter()
            .any(|(d, op, _)| *d == Dep::Plugin && *op == DepOp::PluginPut)
    );
}

#[tokio::test]
async fn delete_records_pdp_dependency_metric() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(repo, plugin, dir, mock_enforcer(), metrics.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    svc.delete(&ctx, &key("k"), exists()).await.expect("delete");
    assert!(
        metrics
            .deps()
            .iter()
            .any(|(d, op, o)| *d == Dep::Pdp && *op == DepOp::Evaluate && *o == Outcome::Success)
    );
}

// ── write protocol: create ───────────────────────────────────────────────────

#[tokio::test]
async fn create_starts_at_version_one_then_overwrite_bumps() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let outcome = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v1"),
            create_only(),
        )
        .await
        .expect("create");
    assert!(outcome.created);
    assert_eq!(outcome.validator.version, 1);
    let row1 = repo.rows()[0].clone();
    assert_eq!(row1.version, 1);
    assert_eq!(row1.status, SecretStatus::Active);
    assert_eq!(row1.value_version, Some(ValueVersion::new("1")));

    let outcome2 = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "v2"),
            put_matches(row1.id, 1),
        )
        .await
        .expect("overwrite");
    assert!(!outcome2.created);
    assert_eq!(outcome2.validator.version, 2);
    let row2 = repo.rows()[0].clone();
    assert_eq!(row2.version, 2);
    assert_eq!(row2.id, row1.id, "the record id (and store key) is stable");
    assert_eq!(
        row2.value_version,
        Some(ValueVersion::new("2")),
        "each write gets the next value version under the same key"
    );
    assert_eq!(
        plugin.versions(&row2.store_key()),
        vec!["2"],
        "the rotated version was destroyed inline"
    );

    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"v2");
}

#[tokio::test]
async fn put_create_conflict_returns_conflict() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo, plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v1"),
        create_only(),
    )
    .await
    .expect("first create");
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v2"),
            create_only(),
        )
        .await
        .expect_err("second create conflicts");
    assert!(matches!(err, DomainError::Conflict));
}

#[tokio::test]
async fn put_create_without_type_is_rejected() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo, plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let write = CredentialWrite {
        secret_type: None,
        sharing: SharingMode::Tenant,
        fallback: SdkFallback::Inherit,
        expires_at: None,
        secret: Some(SecretValue::from("v")),
    };
    let err = svc
        .put(&ctx, &key("k"), write, create_only())
        .await
        .expect_err("type is required on create");
    assert!(matches!(
        err,
        DomainError::InvalidRequest {
            reason: crate::domain::secret::typing::reasons::TYPE_REQUIRED,
            ..
        }
    ));
}

#[tokio::test]
async fn put_shared_coexists_with_private() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(owner, tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "tenant-v"),
        create_only(),
    )
    .await
    .expect("tenant create");
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Private, "priv-v"),
        create_only(),
    )
    .await
    .expect("private create coexists");
    assert_eq!(repo.rows().len(), 2);
}

// ── write protocol: overwrite CAS / preconditions ────────────────────────────

#[tokio::test]
async fn put_if_match_matching_version_overwrites_and_bumps() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v1"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "v2"),
        put_matches(row.id, row.version),
    )
    .await
    .expect("matching version overwrites");
    assert_eq!(repo.rows()[0].version, 2);
}

#[tokio::test]
async fn put_if_match_stale_version_conflicts_without_writing() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v1"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "v2"),
            put_matches(row.id, 99),
        )
        .await
        .expect_err("stale version conflicts");
    assert!(matches!(err, DomainError::VersionConflict));
    // precheck rejects before any backend write.
    assert_eq!(repo.rows()[0].version, 1);
}

#[tokio::test]
async fn put_if_match_on_missing_secret_conflicts() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo, plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let err = svc
        .put(
            &ctx,
            &key("absent"),
            write_replace(SharingMode::Tenant, "v"),
            put_exists(),
        )
        .await
        .expect_err("update of a missing secret conflicts");
    assert!(matches!(err, DomainError::VersionConflict));
}

// ── ADR-0006 core: orphaned puts, lost and ambiguous CAS, destroy ────────────

/// Create `k` (tenant-shared, value `v1`) and return the stored row.
async fn create_k(
    svc: &Service,
    repo: &FakeSecretRepo,
    ctx: &toolkit_security::SecurityContext,
    value: &str,
) -> crate::domain::secret::model::SecretRow {
    svc.put(
        ctx,
        &key("k"),
        write_create(SharingMode::Tenant, value),
        create_only(),
    )
    .await
    .expect("create");
    repo.rows()[0].clone()
}

#[tokio::test]
async fn ambiguous_cas_failure_keeps_the_new_version_and_the_old_value_serves() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "old").await;
    let key_k = row.store_key();
    let destroys = plugin.destroy_calls().len();

    // plugin.put succeeds, but the repo's CAS fails as if the DB were
    // unreachable: the commit may or may not have happened.
    repo.fail_next_switch_value(1);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "torn"),
            put_matches(row.id, row.version),
        )
        .await
        .expect_err("ambiguous CAS failure propagates");
    assert!(matches!(err, DomainError::Internal { .. }));

    // Old value still serves; row untouched.
    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"old");
    assert_eq!(repo.rows()[0].value_version, Some(ValueVersion::new("1")));

    // The version is NOT destroyed: the row may point at it.
    assert_eq!(plugin.versions(&key_k), vec!["1", "2"]);
    assert_eq!(plugin.destroy_calls().len(), destroys, "no destroy at all");

    // The next successful write's destroy(Below) reclaims the orphan.
    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "next"),
        put_matches(row.id, row.version),
    )
    .await
    .expect("next write");
    assert_eq!(repo.rows()[0].value_version, Some(ValueVersion::new("3")));
    assert_eq!(plugin.versions(&key_k), vec!["3"]);
}

#[tokio::test]
async fn plugin_put_failure_leaves_the_row_untouched_and_destroys_nothing() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "old").await;
    let destroys = plugin.destroy_calls().len();

    plugin.fail_next_puts(1);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "new"),
            put_matches(row.id, row.version),
        )
        .await
        .expect_err("put failure");
    assert!(matches!(err, DomainError::Internal { .. }));
    assert_eq!(repo.rows()[0].version, row.version);
    assert_eq!(repo.rows()[0].value_version, row.value_version);
    assert_eq!(plugin.destroy_calls().len(), destroys);
}

#[tokio::test]
async fn put_whose_ack_is_lost_is_503_and_the_orphan_is_destroyed_by_the_next_write() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "old").await;
    let key_k = row.store_key();

    plugin.fail_next_puts_after_persisting(1);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "lost-ack"),
            put_exists(),
        )
        .await
        .expect_err("lost ack");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert_eq!(
        plugin.versions(&key_k),
        vec!["1", "2"],
        "orphan version above the pointer"
    );
    assert_eq!(repo.rows()[0].value_version, Some(ValueVersion::new("1")));

    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "next"),
        put_exists(),
    )
    .await
    .expect("next write");
    assert_eq!(plugin.versions(&key_k), vec!["3"]);
}

#[tokio::test]
async fn lost_cas_destroys_its_own_version_and_the_winner_serves() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "winner").await;
    let key_k = row.store_key();
    let destroys = plugin.destroy_calls().len();

    // Force this writer's CAS to report "lost" (Ok(None)) - models a
    // concurrent writer's switch_value having already moved the row.
    repo.force_next_switch_value_none(1);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "loser"),
            put_matches(row.id, row.version),
        )
        .await
        .expect_err("lost CAS is a conflict");
    assert!(matches!(err, DomainError::VersionConflict));

    assert_eq!(
        plugin.destroy_calls()[destroys..],
        [(
            key_k.clone(),
            DestroySelector::Exactly(ValueVersion::new("2"))
        )],
        "the loser destroys exactly its own version"
    );
    assert_eq!(plugin.versions(&key_k), vec!["1"]);

    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"winner");
}

#[tokio::test]
async fn lost_cas_without_destroy_support_makes_no_destroy_call() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::without_destroy();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "winner").await;
    let key_k = row.store_key();

    repo.force_next_switch_value_none(1);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "loser"),
            put_matches(row.id, row.version),
        )
        .await
        .expect_err("lost CAS is a conflict");
    assert!(matches!(err, DomainError::VersionConflict));
    assert!(plugin.destroy_calls().is_empty());
    assert_eq!(
        plugin.versions(&key_k),
        vec!["1", "2"],
        "without destroy the unreferenced version stays until record delete"
    );
    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"winner");
}

#[tokio::test]
async fn exists_writer_re_reads_and_retries_once_after_a_lost_cas() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "old").await;
    let key_k = row.store_key();

    repo.force_next_switch_value_none(1);
    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "new"),
        put_exists(),
    )
    .await
    .expect("the retry commits");

    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"new");
    assert_eq!(repo.rows()[0].value_version, Some(ValueVersion::new("3")));
    assert_eq!(
        plugin.versions(&key_k),
        vec!["3"],
        "the lost put and the old version are destroyed"
    );
}

#[tokio::test]
async fn exists_writer_that_loses_twice_returns_a_conflict() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "old").await;
    let key_k = row.store_key();

    repo.force_next_switch_value_none(2);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "new"),
            put_exists(),
        )
        .await
        .expect_err("second loss");
    assert!(matches!(err, DomainError::VersionConflict));
    assert_eq!(
        plugin.versions(&key_k),
        vec!["1"],
        "both lost versions destroyed"
    );
}

#[tokio::test]
async fn exists_patch_with_a_secret_also_retries_once() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    create_k(&svc, &repo, &ctx, "old").await;

    repo.force_next_switch_value_none(1);
    svc.patch(&ctx, &key("k"), patch_value("patched"), exists())
        .await
        .expect("retry commits");
    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"patched");
}

#[tokio::test]
async fn two_exists_writers_sequentially_last_pointer_wins_older_versions_destroyed() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "v1").await;
    let key_k = row.store_key();

    for v in ["v2", "v3"] {
        svc.put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, v),
            put_exists(),
        )
        .await
        .expect("Exists overwrite");
    }

    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"v3", "last writer wins");
    assert_eq!(repo.rows()[0].value_version, Some(ValueVersion::new("3")));
    assert_eq!(plugin.versions(&key_k), vec!["3"]);
    assert!(
        plugin
            .destroy_calls()
            .iter()
            .all(|(_, sel)| matches!(sel, DestroySelector::Below(_))),
        "committed writes destroy by position only"
    );
}

#[tokio::test]
async fn failed_destroy_is_ignored_counted_and_the_next_write_cleans_up() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        plugin.clone(),
        dir,
        mock_enforcer(),
        metrics.clone(),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "v1").await;
    let key_k = row.store_key();
    // The create's own destroy(Below) already ran; fail the next one.
    let before = metrics.destroy_failed_total();
    plugin.fail_next_destroys(1);

    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "v2"),
        put_exists(),
    )
    .await
    .expect("overwrite succeeds despite the destroy failure");
    assert_eq!(metrics.destroy_failed_total(), before + 1);
    assert_eq!(
        plugin.versions(&key_k),
        vec!["1", "2"],
        "old version stays below the pointer"
    );

    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "v3"),
        put_exists(),
    )
    .await
    .expect("next write");
    assert_eq!(plugin.versions(&key_k), vec!["3"]);
}

#[tokio::test]
async fn without_destroy_support_nothing_is_ever_destroyed_and_reads_still_work() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::without_destroy();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "v1").await;
    let key_k = row.store_key();

    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "v2"),
        put_exists(),
    )
    .await
    .expect("rotate");
    svc.patch(&ctx, &key("k"), patch_suppress(), exists())
        .await
        .expect("remove the secret");
    svc.patch(&ctx, &key("k"), patch_value("v3"), exists())
        .await
        .expect("set it again");

    assert!(plugin.destroy_calls().is_empty(), "destroy is never called");
    assert!(plugin.delete_key_calls().is_empty(), "nor delete_key");
    assert_eq!(
        plugin.versions(&key_k),
        vec!["1", "2", "3"],
        "rotated and removed versions stay until the record is deleted"
    );
    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"v3");

    // Record deletion is unchanged: row delete plus an outbox purge.
    svc.delete(&ctx, &key("k"), exists()).await.expect("delete");
    assert_eq!(repo.purged_keys(), vec![key_k]);
}

#[tokio::test]
async fn removing_the_secret_destroys_below_and_exactly_old_and_never_delete_key() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "v1").await;
    let key_k = row.store_key();
    let before = plugin.destroy_calls().len();

    svc.patch(&ctx, &key("k"), patch_suppress(), exists())
        .await
        .expect("remove the secret");

    let calls = plugin.destroy_calls();
    assert_eq!(
        calls[before..],
        [
            (
                key_k.clone(),
                DestroySelector::Below(ValueVersion::new("1"))
            ),
            (
                key_k.clone(),
                DestroySelector::Exactly(ValueVersion::new("1"))
            ),
        ]
    );
    assert!(
        plugin.delete_key_calls().is_empty(),
        "never delete_key here"
    );
    assert!(plugin.versions(&key_k).is_empty());
    assert_eq!(repo.rows()[0].status, SecretStatus::Declared);
    assert_eq!(repo.rows()[0].value_version, None);
}

#[tokio::test]
async fn metadata_only_patch_makes_no_store_call() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "v1").await;
    let key_k = row.store_key();
    let destroys = plugin.destroy_calls().len();

    svc.patch(
        &ctx,
        &key("k"),
        patch_sharing(SharingMode::Shared),
        exists(),
    )
    .await
    .expect("metadata patch");

    assert_eq!(repo.rows()[0].version, row.version + 1);
    assert_eq!(repo.rows()[0].value_version, row.value_version);
    assert_eq!(plugin.versions(&key_k), vec!["1"]);
    assert_eq!(plugin.destroy_calls().len(), destroys);
    assert_eq!(plugin.get_calls(), 0);
}

#[tokio::test]
async fn read_re_reads_once_and_serves_the_current_version_when_the_pointer_moved() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        plugin.clone(),
        dir,
        mock_enforcer(),
        metrics.clone(),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "old").await;
    let key_k = row.store_key();

    // A concurrent write already landed: the pointer moves to a new version
    // right after the *next* resolve, and the old version is destroyed.
    let new_version = plugin.seed(&key_k, b"new");
    repo.switch_after_next_resolve(row.id, new_version);
    plugin.drop_version(&key_k, &ValueVersion::new("1"));

    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(
        got.secret.as_bytes(),
        b"new",
        "the retry must serve the current (post-switch) version"
    );
    assert_eq!(metrics.read_retries(), vec![ReadRetryOutcome::Recovered]);
}

#[tokio::test]
async fn read_that_misses_twice_is_503_never_a_stale_or_empty_value() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(
        repo.clone(),
        plugin.clone(),
        dir,
        mock_enforcer(),
        metrics.clone(),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "old").await;

    // The pointer moved to a version that is gone too.
    repo.switch_after_next_resolve(row.id, ValueVersion::new("99"));
    plugin.fail_next_gets_with_not_found(1);
    let err = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect_err("second miss");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert_eq!(metrics.read_retries(), vec![ReadRetryOutcome::SecondMiss]);
}

#[tokio::test]
async fn read_miss_on_an_unmoved_pointer_is_503() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    create_k(&svc, &repo, &ctx, "old").await;

    plugin.fail_next_gets_with_not_found(1);
    let err = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect_err("the version is gone but the row still names it");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
}

#[tokio::test]
async fn declared_row_reads_never_call_the_plugin() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("d"),
        write_create_null(SharingMode::Tenant, SdkFallback::None),
        create_only(),
    )
    .await
    .expect("declared create");

    assert!(
        svc.get_secret(&ctx, &key("d"))
            .await
            .expect("get_secret")
            .is_none()
    );
    svc.get(&ctx, &key("d")).await.expect("get");
    assert_eq!(plugin.get_calls(), 0);
}

#[tokio::test]
async fn delete_enqueues_the_key_purge_and_makes_no_plugin_call() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let row = create_k(&svc, &repo, &ctx, "v1").await;
    let destroys = plugin.destroy_calls().len();

    svc.delete(&ctx, &key("k"), exists()).await.expect("delete");

    assert!(repo.rows().is_empty());
    assert_eq!(repo.purged_keys(), vec![row.store_key()]);
    assert!(
        plugin.delete_key_calls().is_empty(),
        "the purge is the outbox handler's job, not the request path's"
    );
    assert_eq!(plugin.destroy_calls().len(), destroys);
}

#[tokio::test]
async fn delete_then_create_only_put_under_the_same_reference_succeeds() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("reused"),
        write_create(SharingMode::Tenant, "old"),
        create_only(),
    )
    .await
    .expect("create");
    let old = repo.rows()[0].clone();

    svc.delete(&ctx, &key("reused"), exists())
        .await
        .expect("delete");
    assert!(repo.rows().is_empty());

    svc.put(
        &ctx,
        &key("reused"),
        write_create(SharingMode::Tenant, "new"),
        create_only(),
    )
    .await
    .expect("recreate under the same reference succeeds immediately");

    let new = repo.rows()[0].clone();
    assert_ne!(new.id, old.id, "a re-create mints a new record id");
    assert_ne!(new.store_key(), old.store_key());
    let got = svc
        .get_secret(&ctx, &key("reused"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"new");
    // The lagging purge targets the old key only; the new value is untouched.
    assert_eq!(repo.purged_keys(), vec![old.store_key()]);
    assert_eq!(plugin.versions(&new.store_key()), vec!["1"]);
}

#[tokio::test]
async fn create_over_an_expired_own_row_is_a_conflict_and_changes_nothing() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let old = create_k(&svc, &repo, &ctx, "old").await;
    // `generic` is non-expirable, so expire the stored row directly.
    repo.force_expire(old.id);

    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "fresh"),
            create_only(),
        )
        .await
        .expect_err("the expired record is visible: renew or delete it");
    assert!(matches!(err, DomainError::Conflict));

    let rows = repo.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, old.id, "the expired record is not replaced");
    assert!(repo.purged_keys().is_empty(), "no purge is enqueued");
}

// ── expiry applies to the secret, not to the record ─────────────────────────

#[tokio::test]
async fn expired_own_override_is_secret_expired_not_the_ancestors_value() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);

    svc.put(
        &make_ctx(Uuid::new_v4(), parent),
        &key("k"),
        write_create(SharingMode::Shared, "parent-value"),
        create_only(),
    )
    .await
    .expect("create at parent");
    let child_ctx = make_ctx(Uuid::new_v4(), child);
    svc.put(
        &child_ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "child-value"),
        create_only(),
    )
    .await
    .expect("create override at child");
    let own = repo
        .rows()
        .into_iter()
        .find(|r| r.tenant_id == TenantId(child))
        .expect("child row");
    repo.force_expire(own.id);

    let err = svc
        .get_secret(&child_ctx, &key("k"))
        .await
        .expect_err("the expired override is decisive");
    assert!(matches!(err, DomainError::SecretExpired), "{err:?}");

    // The metadata read still shows the record, with the derived status and
    // its normal validator.
    let cred = svc
        .get(&child_ctx, &key("k"))
        .await
        .expect("get")
        .expect("the record stays visible");
    assert_eq!(cred.status, CredentialStatus::Expired);
    assert_eq!(cred.inheritance, InheritanceStatus::Overridden);
    assert!(cred.expires_at.is_some());
    let validator = cred.validator.expect("normal validator");
    assert_eq!((validator.id, validator.version), (own.id, own.version));

    // The point read with the secret selected fails the same way, with or
    // without an administrative field alongside it.
    for fields in [
        vec!["secret".to_owned()],
        vec!["status".to_owned(), "secret".to_owned()],
    ] {
        let err = svc
            .get_item(&child_ctx, &key("k"), Some(&fields))
            .await
            .expect_err("expired");
        assert!(matches!(err, DomainError::SecretExpired), "{err:?}");
    }
}

#[tokio::test]
async fn expired_decisive_ancestor_shared_record_is_secret_expired() {
    let grandparent = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent, grandparent]));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);

    for (tenant, value) in [(grandparent, "gp-value"), (parent, "p-value")] {
        svc.put(
            &make_ctx(Uuid::new_v4(), tenant),
            &key("k"),
            write_create(SharingMode::Shared, value),
            create_only(),
        )
        .await
        .expect("create shared");
    }
    let parent_row = repo
        .rows()
        .into_iter()
        .find(|r| r.tenant_id == TenantId(parent))
        .expect("parent row");
    repo.force_expire(parent_row.id);

    let child_ctx = make_ctx(Uuid::new_v4(), child);
    let err = svc
        .get_secret(&child_ctx, &key("k"))
        .await
        .expect_err("the nearest shared record is expired");
    assert!(
        matches!(err, DomainError::SecretExpired),
        "never the grandparent's value: {err:?}"
    );

    // Metadata: the inherited record is visible; the caller holds no own
    // row, so its own-row status stays `none` and `expires_at` is the past
    // instant.
    let cred = svc
        .get(&child_ctx, &key("k"))
        .await
        .expect("get")
        .expect("visible");
    assert_eq!(cred.inheritance, InheritanceStatus::Inherited);
    assert_eq!(cred.status, CredentialStatus::None);
    assert!(cred.expires_at.expect("expiry") <= OffsetDateTime::now_utc());
}

#[tokio::test]
async fn expired_decisive_record_blocks_even_a_declared_own_row_with_inherit() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    svc.put(
        &make_ctx(Uuid::new_v4(), parent),
        &key("k"),
        write_create(SharingMode::Shared, "p"),
        create_only(),
    )
    .await
    .expect("create shared");
    repo.force_expire(repo.rows()[0].id);

    // A `declared`/`inherit` own row does not compete, so the expired
    // ancestor remains decisive: `fallback` only governs `declared` records.
    let child_ctx = make_ctx(Uuid::new_v4(), child);
    svc.put(
        &child_ctx,
        &key("k"),
        write_create_null(SharingMode::Tenant, SdkFallback::Inherit),
        create_only(),
    )
    .await
    .expect("declare at child");
    let err = svc
        .get_secret(&child_ctx, &key("k"))
        .await
        .expect_err("expired");
    assert!(matches!(err, DomainError::SecretExpired), "{err:?}");
}

#[tokio::test]
async fn secret_expired_is_only_disclosed_to_a_caller_who_may_read_the_secret() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let writer = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let row = create_k(&writer, &repo, &ctx, "v").await;
    repo.force_expire(row.id);

    let (enforcer, _) = action_deny_enforcer(
        SecretType::generic().gts_id().to_owned(),
        crate::domain::authz::actions::READ_SECRET,
    );
    let svc = make_service(repo.clone(), plugin, dir, enforcer, Arc::new(NoopMetrics));

    // No `read_secret`: the usual not-found answer, never `SECRET_EXPIRED`.
    assert!(
        svc.get_secret(&ctx, &key("k"))
            .await
            .expect("not an error")
            .is_none()
    );
    let fields = ["status".to_owned(), "secret".to_owned()];
    assert!(
        svc.get_item(&ctx, &key("k"), Some(&fields))
            .await
            .expect("not an error")
            .is_none()
    );
    // The record itself is still readable with `read`.
    let cred = svc
        .get(&ctx, &key("k"))
        .await
        .expect("get")
        .expect("visible");
    assert_eq!(cred.status, CredentialStatus::Expired);
}

#[tokio::test]
async fn patching_expires_at_renews_in_place_and_the_secret_is_served_again() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    // `personal-token` is expirable (and private-only).
    let write = CredentialWrite {
        expires_at: Some(OffsetDateTime::now_utc() + time::Duration::hours(1)),
        ..write_create_typed(SharingMode::Private, "tok", "personal-token")
    };
    svc.put(&ctx, &key("pt"), write, create_only())
        .await
        .expect("create");
    let row = repo.rows()[0].clone();
    repo.force_expire(row.id);
    assert!(matches!(
        svc.get_secret(&ctx, &key("pt")).await,
        Err(DomainError::SecretExpired)
    ));
    let expired = repo.rows()[0].clone();

    let validator = svc
        .patch(
            &ctx,
            &key("pt"),
            CredentialPatch {
                expires_at: PatchField::Set(OffsetDateTime::now_utc() + time::Duration::hours(2)),
                ..empty_patch()
            },
            matches(expired.id, expired.version),
        )
        .await
        .expect("renew the expired record in place");
    assert_eq!(validator.id, row.id, "same record, not a new one");
    assert_eq!(validator.version, expired.version + 1);

    let got = svc
        .get_secret(&ctx, &key("pt"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"tok");
    let cred = svc.get(&ctx, &key("pt")).await.expect("get").expect("some");
    assert_eq!(cred.status, CredentialStatus::Active);
}

#[tokio::test]
async fn put_with_if_match_star_renews_an_expired_record_in_place() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let old = create_k(&svc, &repo, &ctx, "old").await;
    repo.force_expire(old.id);

    // A replace of an expired record writes the whole record again; `generic`
    // is non-expirable, so the replace carries no expiry and the stored
    // instant is cleared.
    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "new"),
        put_exists(),
    )
    .await
    .expect("replace the expired record");
    assert_eq!(repo.rows()[0].id, old.id);
    let got = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect("get_secret")
        .expect("some");
    assert_eq!(got.secret.as_bytes(), b"new");
}

#[tokio::test]
async fn create_over_a_live_own_row_is_still_a_conflict() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    create_k(&svc, &repo, &ctx, "old").await;

    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "again"),
            create_only(),
        )
        .await
        .expect_err("conflict");
    assert!(matches!(err, DomainError::Conflict));
    assert!(repo.purged_keys().is_empty());
}

#[tokio::test]
async fn lost_create_race_destroys_the_loser_version_and_returns_conflict() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    // A concurrent create commits between this writer's read and its insert.
    repo.conflict_next_insert_active(1);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "loser"),
            create_only(),
        )
        .await
        .expect_err("unique violation is a conflict");
    assert!(matches!(err, DomainError::Conflict));

    let calls = plugin.destroy_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, DestroySelector::Exactly(ValueVersion::new("1")));
    assert!(!plugin.holds_key(&calls[0].0));
}

#[tokio::test]
async fn ambiguous_create_failure_keeps_the_version() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    repo.fail_next_insert_active(1);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect_err("ambiguous failure");
    assert!(matches!(err, DomainError::Internal { .. }));
    assert!(
        plugin.destroy_calls().is_empty(),
        "the commit may have happened: the version is not destroyed"
    );
}

// ── delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_private_secret_removes_row() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(owner, tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Private, "v"),
        create_only(),
    )
    .await
    .expect("create");
    svc.delete(&ctx, &key("k"), exists()).await.expect("delete");
    assert!(repo.rows().is_empty());
    assert!(
        svc.get_secret(&ctx, &key("k"))
            .await
            .expect("get_secret")
            .is_none()
    );
}

#[tokio::test]
async fn delete_only_own_tenant_404_when_inherited_only() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let svc = make_service_noop(repo, plugin, dir);

    let parent_ctx = make_ctx(Uuid::new_v4(), parent);
    svc.put(
        &parent_ctx,
        &key("k"),
        write_create(SharingMode::Shared, "v"),
        create_only(),
    )
    .await
    .expect("create at parent");

    let child_ctx = make_ctx(Uuid::new_v4(), child);
    let err = svc
        .delete(&child_ctx, &key("k"), exists())
        .await
        .expect_err("no own-tenant row at the child");
    assert!(matches!(err, DomainError::NotFound));
}

#[tokio::test]
async fn delete_if_match_stale_version_conflicts() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    let err = svc
        .delete(&ctx, &key("k"), matches(row.id, 99))
        .await
        .expect_err("stale version conflicts");
    assert!(matches!(err, DomainError::VersionConflict));
    assert_eq!(repo.rows().len(), 1, "row must survive a rejected delete");
}

#[tokio::test]
async fn delete_if_match_race_maps_zero_rows_to_version_conflict() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    // Simulate a race: the row is gone by the time delete_by_id's own
    // transaction runs, even though find_own/precheck (moments earlier) saw
    // it at `row.version`.
    repo.force_next_delete_by_id_not_found(1);
    let err = svc
        .delete(&ctx, &key("k"), matches(row.id, row.version))
        .await
        .expect_err("lost race under a version precondition is a conflict");
    assert!(matches!(err, DomainError::VersionConflict));
}

#[tokio::test]
async fn delete_needs_no_plugin_because_the_purge_runs_from_the_outbox() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    // Create with a real plugin, then rebuild the service without one.
    let plugin = FakePlugin::new();
    let svc = make_service_noop(repo.clone(), plugin, dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    let svc_no_plugin = Service::new(
        repo.clone(),
        dir,
        mock_enforcer(),
        Arc::new(NoPluginSelector),
        catalog_type_resolver(),
        Arc::new(NoopMetrics),
        test_list_settings(),
    );
    svc_no_plugin
        .delete(&ctx, &key("k"), exists())
        .await
        .expect("delete resolves no plugin: the outbox handler does, and retries");
    assert!(repo.rows().is_empty());
    assert_eq!(repo.purged_keys(), vec![row.store_key()]);
}

// ── PDP / scope gating ────────────────────────────────────────────────────────

#[tokio::test]
async fn read_gate_denied_when_tenant_out_of_scope() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::with_scope_allows(false));
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(repo.clone(), plugin, dir, mock_enforcer(), metrics.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    repo.seed(seeded_row(tenant, Uuid::new_v4(), "k", SharingMode::Tenant));
    let got = svc.get_secret(&ctx, &key("k")).await.expect("get_secret");
    assert!(got.is_none());
    assert_eq!(metrics.cross_tenant_denied_count(), 1);
}

#[tokio::test]
async fn get_secret_returns_not_found_when_pdp_denies() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service(
        repo.clone(),
        plugin,
        dir,
        deny_enforcer(),
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    repo.seed(seeded_row(tenant, Uuid::new_v4(), "k", SharingMode::Tenant));
    assert!(
        svc.get_secret(&ctx, &key("k"))
            .await
            .expect("get_secret")
            .is_none()
    );
}

#[tokio::test]
async fn get_secret_returns_service_unavailable_when_pdp_fails() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service(
        repo.clone(),
        plugin,
        dir,
        failing_enforcer(),
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    repo.seed(seeded_row(tenant, Uuid::new_v4(), "k", SharingMode::Tenant));
    let err = svc
        .get_secret(&ctx, &key("k"))
        .await
        .expect_err("pdp outage");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
}

#[tokio::test]
async fn operations_return_service_unavailable_when_type_resolver_fails() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = Service::new(
        repo,
        dir,
        mock_enforcer(),
        Arc::new(FakePluginSelector::new(plugin)),
        Arc::new(FailingTypeResolver),
        Arc::new(NoopMetrics),
        test_list_settings(),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect_err("registry outage");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
}

#[tokio::test]
async fn put_returns_access_denied_when_pdp_denies() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service(repo, plugin, dir, deny_enforcer(), Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect_err("pdp denies");
    assert!(matches!(err, DomainError::AccessDenied { .. }));
}

#[tokio::test]
async fn delete_returns_access_denied_when_pdp_denies() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    repo.seed(seeded_row(tenant, Uuid::new_v4(), "k", SharingMode::Tenant));
    let svc = make_service(repo, plugin, dir, deny_enforcer(), Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let err = svc
        .delete(&ctx, &key("k"), exists())
        .await
        .expect_err("pdp denies");
    assert!(matches!(err, DomainError::AccessDenied { .. }));
}

#[tokio::test]
async fn create_only_conflict_is_authorized_before_it_leaks_existence() {
    let tenant = Uuid::new_v4();
    let (enforcer, resolver) = type_deny_enforcer(vec![]);
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v2"),
            create_only(),
        )
        .await
        .expect_err("create-only conflict");
    assert!(matches!(err, DomainError::Conflict));
    assert!(
        !resolver
            .seen_resource_types
            .lock()
            .expect("lock")
            .is_empty()
    );
}

// ── reference-scoped grants (ADR-0010) ───────────────────────────────────────

/// Seed `refs` as tenant-shared generic credentials via a permissive service.
async fn seed_refs(
    repo: &Arc<FakeSecretRepo>,
    plugin: &Arc<FakePlugin>,
    dir: &Arc<FakeDir>,
    ctx: &toolkit_security::SecurityContext,
    refs: &[&str],
) {
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    for r in refs {
        svc.put(
            ctx,
            &key(r),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("seed");
    }
}

#[tokio::test]
async fn point_read_is_admitted_or_missing_by_reference() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    seed_refs(&repo, &plugin, &dir, &ctx, &["smtp-password", "other"]).await;

    let svc = make_service(
        repo,
        plugin,
        dir,
        reference_enforcer(&["smtp-password"], None),
        Arc::new(NoopMetrics),
    );
    let got = svc
        .get_secret(&ctx, &key("smtp-password"))
        .await
        .expect("get_secret")
        .expect("admitted");
    assert_eq!(got.secret.as_bytes(), b"v");
    assert!(
        svc.get(&ctx, &key("smtp-password"))
            .await
            .expect("get")
            .is_some()
    );
    assert!(
        svc.get_secret(&ctx, &key("other"))
            .await
            .expect("get_secret")
            .is_none()
    );
    assert!(svc.get(&ctx, &key("other")).await.expect("get").is_none());
}

#[tokio::test]
async fn a_non_admitted_decisive_child_override_is_a_miss_never_the_ancestor_value() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::new(vec![child, parent]));
    let ctx = make_ctx(owner, child);

    let child_type = SecretType::generic();
    let parent_type = SecretType::from_name("api-key").expect("known");
    let now = OffsetDateTime::now_utc();
    // The child's own (decisive, nearest) row is of a type the PDP excludes;
    // the ancestor's shared row is of an admitted type.
    for (tenant_id, sharing, ty) in [
        (child, SharingMode::Tenant, &child_type),
        (parent, SharingMode::Shared, &parent_type),
    ] {
        repo.seed(crate::domain::secret::model::SecretRow {
            id: Uuid::new_v4(),
            tenant_id: TenantId(tenant_id),
            reference: "r".to_owned(),
            sharing,
            owner_id: OwnerId(owner),
            status: SecretStatus::Active,
            version: 1,
            updated_at: now,
            secret_type_uuid: ty.uuid(),
            expires_at: None,
            value_version: Some(ValueVersion::new("1")),
            fallback: Fallback::Inherit,
        });
    }

    let permissive = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    assert!(
        permissive
            .get(&ctx, &key("r"))
            .await
            .expect("get")
            .is_some()
    );

    let (enforcer, _) = type_deny_enforcer(vec![child_type.gts_id().to_owned()]);
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    assert!(svc.get(&ctx, &key("r")).await.expect("get").is_none());
    assert!(
        svc.get_secret(&ctx, &key("r"))
            .await
            .expect("get_secret")
            .is_none(),
        "the ancestor's value must not be served in place of the decisive row"
    );
}

#[tokio::test]
async fn create_is_denied_by_a_reference_constraint_that_does_not_admit_the_key() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let svc = make_service(
        repo.clone(),
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        reference_enforcer(&["smtp-password"], None),
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let err = svc
        .put(
            &ctx,
            &key("other"),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect_err("not admitted");
    assert!(matches!(err, DomainError::AccessDenied { .. }));
    assert!(repo.rows().is_empty());

    svc.put(
        &ctx,
        &key("smtp-password"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("admitted reference");
    assert_eq!(repo.rows().len(), 1);
}

#[tokio::test]
async fn replace_patch_and_delete_of_a_non_admitted_reference_answer_as_missing() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    seed_refs(&repo, &plugin, &dir, &ctx, &["other"]).await;
    let svc = make_service(
        repo.clone(),
        plugin,
        dir,
        reference_enforcer(&["smtp-password"], None),
        Arc::new(NoopMetrics),
    );

    let err = svc
        .put(
            &ctx,
            &key("other"),
            write_replace(SharingMode::Tenant, "v2"),
            put_exists(),
        )
        .await
        .expect_err("replace of a missing row");
    assert!(matches!(err, DomainError::VersionConflict));

    let err = svc
        .patch(&ctx, &key("other"), patch_value("v3"), exists())
        .await
        .expect_err("patch of a missing row");
    assert!(matches!(err, DomainError::NotFound));

    let err = svc
        .delete(&ctx, &key("other"), exists())
        .await
        .expect_err("delete of a missing row");
    assert!(matches!(err, DomainError::NotFound));
    assert_eq!(repo.rows().len(), 1, "the row is untouched");
}

#[tokio::test]
async fn removing_the_secret_needs_write_secret_on_the_reference() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    seed_refs(&repo, &plugin, &dir, &ctx, &["smtp-password", "other"]).await;
    // `write` is unrestricted; `write_secret` only covers smtp-password.
    let svc = make_service(
        repo.clone(),
        plugin,
        dir,
        reference_enforcer(&["smtp-password"], Some(&["write_secret"])),
        Arc::new(NoopMetrics),
    );

    let err = svc
        .put(
            &ctx,
            &key("other"),
            write_replace_null(SharingMode::Tenant, SdkFallback::Inherit),
            put_exists(),
        )
        .await
        .expect_err("write_secret does not cover this reference");
    assert!(matches!(err, DomainError::AccessDenied { .. }));

    svc.put(
        &ctx,
        &key("smtp-password"),
        write_replace_null(SharingMode::Tenant, SdkFallback::Inherit),
        put_exists(),
    )
    .await
    .expect("write_secret covers smtp-password");
}

// ── plugin-error mapping ─────────────────────────────────────────────────────

#[test]
#[allow(
    clippy::cognitive_complexity,
    reason = "a flat enumeration of every PluginError variant's DomainError mapping; splitting \
              it would only scatter the one-to-one correspondence this test is checking"
)]
fn map_plugin_err_covers_all_variants() {
    use crate::domain::secret::service::map_plugin_err;
    use credstore_sdk::CredStoreError as E;
    assert!(matches!(map_plugin_err(E::NotFound), DomainError::NotFound));
    assert!(matches!(
        map_plugin_err(E::AccessDenied),
        DomainError::AccessDenied { .. }
    ));
    assert!(matches!(map_plugin_err(E::Conflict), DomainError::Conflict));
    assert!(matches!(
        map_plugin_err(E::ServiceUnavailable {
            detail: "x".into(),
            retry_after: None
        }),
        DomainError::ServiceUnavailable { .. }
    ));
    assert!(matches!(
        map_plugin_err(E::NoPluginAvailable),
        DomainError::ServiceUnavailable {
            retry_after: None,
            ..
        }
    ));
    assert!(matches!(
        map_plugin_err(E::invalid_ref("x")),
        DomainError::Internal { .. }
    ));
    assert!(matches!(
        map_plugin_err(E::unsupported_transition("x")),
        DomainError::Internal { .. }
    ));
    assert!(matches!(
        map_plugin_err(E::TypeViolation {
            reason: "R".into(),
            detail: "d".into()
        }),
        DomainError::Internal { .. }
    ));
    assert!(matches!(
        map_plugin_err(E::InvalidRequest {
            reason: "R".into(),
            detail: "d".into()
        }),
        DomainError::Internal { .. }
    ));
    assert!(matches!(
        map_plugin_err(E::Internal("x".into())),
        DomainError::Internal { .. }
    ));
}

#[test]
fn no_plugin_available_maps_to_distinct_non_retryable_unavailable() {
    use crate::domain::secret::service::map_plugin_err;
    let mapped = map_plugin_err(credstore_sdk::CredStoreError::NoPluginAvailable);
    match mapped {
        DomainError::ServiceUnavailable {
            retry_after,
            detail,
            ..
        } => {
            assert!(retry_after.is_none());
            assert_eq!(detail, "no storage plugin registered");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn plugin_unavailable_detail_is_curated_off_the_wire() {
    use crate::domain::secret::service::map_plugin_err;
    let mapped = map_plugin_err(credstore_sdk::CredStoreError::ServiceUnavailable {
        detail: "raw backend secret leak attempt".into(),
        retry_after: None,
    });
    match mapped {
        DomainError::ServiceUnavailable { detail, .. } => {
            assert_eq!(detail, "storage backend unavailable");
            assert!(!detail.contains("secret"));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn get_secret_folds_plugin_access_denied_into_anti_enumeration_miss() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::with_get_denied();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    repo.seed(seeded_row(tenant, Uuid::new_v4(), "k", SharingMode::Tenant));
    assert!(
        svc.get_secret(&ctx, &key("k"))
            .await
            .expect("get_secret")
            .is_none()
    );
}

#[tokio::test]
async fn pdp_denial_is_not_a_dependency_health_error() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let metrics = FakeMetrics::new();
    let svc = make_service(repo, plugin, dir, deny_enforcer(), metrics.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    assert!(
        svc.put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v"),
            create_only()
        )
        .await
        .is_err(),
        "pdp denies the write"
    );
    assert!(
        metrics
            .deps()
            .iter()
            .all(|(d, _, o)| *d != Dep::Pdp || *o == Outcome::Success)
    );
}

// ── typing / traits ───────────────────────────────────────────────────────────

#[tokio::test]
async fn typed_create_enforces_allow_sharing_and_returns_type() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo, plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    // personal-token only allows Private sharing.
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create_typed(SharingMode::Tenant, "v", "personal-token"),
            create_only(),
        )
        .await
        .expect_err("sharing not allowed for type");
    assert!(matches!(err, DomainError::TypeViolation { .. }));
}

#[tokio::test]
async fn secret_type_is_immutable_on_overwrite() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create generic");
    let row = repo.rows()[0].clone();

    let write = CredentialWrite {
        secret_type: Some(SecretType::from_name("api-key").expect("known").into()),
        ..write_replace(SharingMode::Tenant, "v2")
    };
    let err = svc
        .put(&ctx, &key("k"), write, put_matches(row.id, row.version))
        .await
        .expect_err("type change rejected");
    assert!(matches!(
        err,
        DomainError::TypeViolation {
            reason: crate::domain::secret::typing::reasons::TYPE_IMMUTABLE,
            ..
        }
    ));
}

#[tokio::test]
async fn expiry_rejected_for_non_expirable_type() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo, plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let write = CredentialWrite {
        expires_at: Some(OffsetDateTime::now_utc() + time::Duration::hours(1)),
        ..write_create(SharingMode::Tenant, "v") // generic: not expirable
    };
    let err = svc
        .put(&ctx, &key("k"), write, create_only())
        .await
        .expect_err("expiry on non-expirable type");
    assert!(matches!(err, DomainError::TypeViolation { .. }));
}

#[tokio::test]
async fn per_type_pdp_denial_hides_reads_and_forbids_writes() {
    let tenant = Uuid::new_v4();
    let api_key_gts = SecretType::from_name("api-key")
        .unwrap()
        .gts_id()
        .to_owned();
    let (enforcer, _resolver) = type_deny_enforcer(vec![api_key_gts]);
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_create_typed(SharingMode::Tenant, "v", "api-key"),
            create_only(),
        )
        .await
        .expect_err("denied type");
    assert!(matches!(err, DomainError::AccessDenied { .. }));
}

#[tokio::test]
async fn generic_secrets_evaluate_the_full_concrete_type() {
    let tenant = Uuid::new_v4();
    let (enforcer, resolver) = type_deny_enforcer(vec![]);
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let seen = resolver.seen_resource_types.lock().expect("lock").clone();
    assert!(
        seen.iter().any(|t| t.contains("generic")),
        "generic must still be evaluated as a concrete type: {seen:?}"
    );
}

// ── record generations (ADR-0006: no ABA) ────────────────────────────────────

#[tokio::test]
async fn aba_recreate_rejects_stale_generation_validator() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "gen1"),
        create_only(),
    )
    .await
    .expect("create gen1");
    let gen1 = repo.rows()[0].clone();
    svc.delete(&ctx, &key("k"), exists())
        .await
        .expect("delete gen1");
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "gen2"),
        create_only(),
    )
    .await
    .expect("create gen2");

    // A validator minted for gen1 must never match gen2, even if version
    // counters happen to coincide (both start at 1).
    let err = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "v3"),
            put_matches(gen1.id, gen1.version),
        )
        .await
        .expect_err("stale generation validator rejected");
    assert!(matches!(err, DomainError::VersionConflict));
}

// ── ADR-0004: walkthrough scenario (T1 shared, T2 overrides/rotates/
//    suppresses/deletes, T3 inherits from T1/T2) ────────────────────────────

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "a single end-to-end narrative walkthrough of override/rotate/suppress/delete \
              across three tenants; splitting it would break the story the test is telling"
)]
async fn walkthrough_t1_t2_t3_override_rotate_suppress_delete() {
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let t3 = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    // t3's chain is [t3, t2, t1]; t2's chain is [t2, t1]; t1's is [t1].
    let dir_t1 = Arc::new(FakeDir::single(t1));
    let dir_t2 = Arc::new(FakeDir::new(vec![t2, t1]));
    let dir_t3 = Arc::new(FakeDir::new(vec![t3, t2, t1]));

    let svc_t1 = make_service_noop(repo.clone(), plugin.clone(), dir_t1);
    let svc_t2 = make_service_noop(repo.clone(), plugin.clone(), dir_t2);
    let svc_t3 = make_service_noop(repo.clone(), plugin.clone(), dir_t3);

    let owner1 = Uuid::new_v4();
    let ctx1 = make_ctx(owner1, t1);
    let owner2 = Uuid::new_v4();
    let ctx2 = make_ctx(owner2, t2);
    let owner3 = Uuid::new_v4();
    let ctx3 = make_ctx(owner3, t3);
    let name = key("smtp-default");

    // Step 0: T1 publishes shared V1. T2/T3 inherit it.
    svc_t1
        .put(
            &ctx1,
            &name,
            write_create(SharingMode::Shared, "V1"),
            create_only(),
        )
        .await
        .expect("T1 publishes");
    assert_eq!(
        svc_t2
            .get_secret(&ctx2, &name)
            .await
            .expect("t2 get_secret")
            .expect("v1")
            .secret
            .as_bytes(),
        b"V1"
    );
    assert_eq!(
        svc_t3
            .get_secret(&ctx3, &name)
            .await
            .expect("t3 get_secret")
            .expect("v1")
            .secret
            .as_bytes(),
        b"V1"
    );
    assert_eq!(
        svc_t3
            .get(&ctx3, &name)
            .await
            .expect("t3 get")
            .expect("cred")
            .inheritance,
        InheritanceStatus::Inherited
    );

    // Step 1: T2 overrides with V2 (create-only).
    let outcome = svc_t2
        .put(
            &ctx2,
            &name,
            write_create(SharingMode::Shared, "V2"),
            create_only(),
        )
        .await
        .expect("T2 overrides");
    assert!(outcome.created);
    let t2_cred = svc_t2
        .get(&ctx2, &name)
        .await
        .expect("t2 get")
        .expect("cred");
    assert_eq!(t2_cred.inheritance, InheritanceStatus::Overridden);
    assert_eq!(t2_cred.status, CredentialStatus::Active);
    assert!(t2_cred.validator.is_some(), "own row => strong validator");
    assert_eq!(
        svc_t3
            .get_secret(&ctx3, &name)
            .await
            .expect("t3 get_secret")
            .expect("v2")
            .secret
            .as_bytes(),
        b"V2"
    );

    // Step 2: T2 rotates via PATCH {value}. Only write_secret is required —
    // assert via the fake PDP's recorded actions.
    let (rotate_enforcer, rotate_resolver) = type_recording_enforcer();
    let svc_t2_recording = make_service(
        repo.clone(),
        plugin.clone(),
        Arc::new(FakeDir::new(vec![t2, t1])),
        rotate_enforcer,
        Arc::new(NoopMetrics),
    );
    let validator = svc_t2_recording
        .patch(
            &ctx2,
            &name,
            patch_value("V3"),
            matches(
                t2_cred.validator.expect("some").id,
                t2_cred.validator.expect("some").version,
            ),
        )
        .await
        .expect("T2 rotates");
    assert_eq!(
        validator.version,
        t2_cred.validator.expect("some").version + 1
    );
    assert_eq!(
        svc_t3
            .get_secret(&ctx3, &name)
            .await
            .expect("t3 get_secret")
            .expect("v3")
            .secret
            .as_bytes(),
        b"V3"
    );
    let seen = rotate_resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()));
    assert!(
        !seen.contains(&crate::domain::authz::actions::WRITE.to_owned()),
        "a value-only PATCH must not evaluate `write`: {seen:?}"
    );

    // Step 3: T2 suppresses {"fallback": "none", "secret": null} — one
    // transaction. T2's row becomes declared/none; T2 and T3 now get None;
    // T2's record reads suppressed/declared; T1 untouched.
    let t2_after_rotate = svc_t2
        .get(&ctx2, &name)
        .await
        .expect("t2 get")
        .expect("cred");
    svc_t2
        .patch(
            &ctx2,
            &name,
            patch_suppress(),
            matches(
                t2_after_rotate.validator.expect("some").id,
                t2_after_rotate.validator.expect("some").version,
            ),
        )
        .await
        .expect("T2 suppresses");
    assert!(
        svc_t2
            .get_secret(&ctx2, &name)
            .await
            .expect("t2 get_secret")
            .is_none()
    );
    assert!(
        svc_t3
            .get_secret(&ctx3, &name)
            .await
            .expect("t3 get_secret")
            .is_none()
    );
    let t2_suppressed = svc_t2
        .get(&ctx2, &name)
        .await
        .expect("t2 get")
        .expect("cred");
    assert_eq!(t2_suppressed.status, CredentialStatus::Declared);
    assert_eq!(t2_suppressed.inheritance, InheritanceStatus::Suppressed);
    assert_eq!(
        svc_t1
            .get_secret(&ctx1, &name)
            .await
            .expect("t1 get_secret")
            .expect("still v1")
            .secret
            .as_bytes(),
        b"V1",
        "T1's own record and value are untouched"
    );

    // Step 4: T2 deletes — T2/T3 inherit T1 (V1) again, status: none.
    svc_t2
        .delete(
            &ctx2,
            &name,
            matches(
                t2_suppressed.validator.expect("some").id,
                t2_suppressed.validator.expect("some").version,
            ),
        )
        .await
        .expect("T2 deletes");
    assert_eq!(
        svc_t2
            .get_secret(&ctx2, &name)
            .await
            .expect("t2 get_secret")
            .expect("inherits v1 again")
            .secret
            .as_bytes(),
        b"V1"
    );
    assert_eq!(
        svc_t3
            .get_secret(&ctx3, &name)
            .await
            .expect("t3 get_secret")
            .expect("inherits v1 again")
            .secret
            .as_bytes(),
        b"V1"
    );
    let t2_final = svc_t2
        .get(&ctx2, &name)
        .await
        .expect("t2 get")
        .expect("cred");
    assert_eq!(t2_final.status, CredentialStatus::None);
    assert!(t2_final.validator.is_none());
    assert_eq!(t2_final.inheritance, InheritanceStatus::Inherited);
}

// ── ADR-0004: body-derived actions ───────────────────────────────────────────

#[tokio::test]
async fn put_evaluates_both_write_and_write_secret() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let (enforcer, resolver) = type_recording_enforcer();
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()));
}

#[tokio::test]
async fn patch_metadata_only_evaluates_write_alone() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    let (enforcer, resolver) = type_recording_enforcer();
    let svc_recording = make_service(repo.clone(), plugin, dir, enforcer, Arc::new(NoopMetrics));
    svc_recording
        .patch(
            &ctx,
            &key("k"),
            patch_sharing(SharingMode::Shared),
            matches(row.id, row.version),
        )
        .await
        .expect("metadata-only patch");
    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(!seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()));
}

#[tokio::test]
async fn patch_value_only_evaluates_write_secret_alone() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    let (enforcer, resolver) = type_recording_enforcer();
    let svc_recording = make_service(repo.clone(), plugin, dir, enforcer, Arc::new(NoopMetrics));
    svc_recording
        .patch(
            &ctx,
            &key("k"),
            patch_value("v2"),
            matches(row.id, row.version),
        )
        .await
        .expect("value-only patch");
    let seen = resolver.seen_actions();
    assert!(!seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()));
}

#[tokio::test]
async fn patch_both_metadata_and_value_evaluates_both_actions() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    let (enforcer, resolver) = type_recording_enforcer();
    let svc_recording = make_service(repo.clone(), plugin, dir, enforcer, Arc::new(NoopMetrics));
    let patch = CredentialPatch {
        sharing: Some(SharingMode::Shared),
        ..patch_value("v2")
    };
    svc_recording
        .patch(&ctx, &key("k"), patch, matches(row.id, row.version))
        .await
        .expect("both-fields patch");
    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()));
}

#[tokio::test]
async fn patch_denied_action_writes_nothing() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    // Deny write_secret specifically for the generic type.
    let generic_gts = SecretType::generic().gts_id().to_owned();
    let (enforcer, _resolver) =
        action_deny_enforcer(generic_gts, crate::domain::authz::actions::WRITE_SECRET);
    let svc_denied = make_service(repo.clone(), plugin, dir, enforcer, Arc::new(NoopMetrics));

    let patch = CredentialPatch {
        sharing: Some(SharingMode::Shared),
        ..patch_value("v2")
    };
    let err = svc_denied
        .patch(&ctx, &key("k"), patch, matches(row.id, row.version))
        .await
        .expect_err("write_secret denied");
    // `write_secret` on the base type excludes the record's type from the
    // PDP's type constraint, so the lookup under that scope does not find the
    // row: the same 404 as a missing record (a flat PDP denial is the 403).
    assert!(matches!(err, DomainError::NotFound));
    // Nothing was written: sharing and value both unchanged.
    assert_eq!(repo.rows()[0].sharing, SharingMode::Tenant);
    assert_eq!(repo.rows()[0].version, row.version);
}

#[tokio::test]
async fn patch_empty_is_rejected() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    let err = svc
        .patch(&ctx, &key("k"), empty_patch(), matches(row.id, row.version))
        .await
        .expect_err("empty patch rejected");
    assert!(matches!(
        err,
        DomainError::InvalidRequest {
            reason: crate::domain::secret::typing::reasons::EMPTY_PATCH,
            ..
        }
    ));
}

#[tokio::test]
async fn patch_metadata_no_op_keeps_version_and_returns_current_validator() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    // Same sharing as already stored: a genuine no-op.
    let validator = svc
        .patch(
            &ctx,
            &key("k"),
            patch_sharing(SharingMode::Tenant),
            matches(row.id, row.version),
        )
        .await
        .expect("no-op patch");
    assert_eq!(validator.id, row.id);
    assert_eq!(
        validator.version, row.version,
        "no-op must not bump the version"
    );
    assert_eq!(repo.rows()[0].version, row.version);
}

#[tokio::test]
async fn patch_type_immutable() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create generic");
    let row = repo.rows()[0].clone();

    let patch = CredentialPatch {
        secret_type: Some(SecretType::from_name("api-key").expect("known").into()),
        ..empty_patch()
    };
    let err = svc
        .patch(&ctx, &key("k"), patch, matches(row.id, row.version))
        .await
        .expect_err("type change rejected");
    assert!(matches!(
        err,
        DomainError::TypeViolation {
            reason: crate::domain::secret::typing::reasons::TYPE_IMMUTABLE,
            ..
        }
    ));
}

#[tokio::test]
async fn put_create_type_mismatch_with_inherited() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir_parent = Arc::new(FakeDir::single(parent));
    let dir_child = Arc::new(FakeDir::new(vec![child, parent]));
    let svc_parent = make_service_noop(repo.clone(), plugin.clone(), dir_parent);
    let svc_child = make_service_noop(repo.clone(), plugin.clone(), dir_child);

    let parent_ctx = make_ctx(owner, parent);
    svc_parent
        .put(
            &parent_ctx,
            &key("k"),
            write_create_typed(
                SharingMode::Shared,
                r#"{"username":"u","password":"p"}"#,
                "basic-auth",
            ),
            create_only(),
        )
        .await
        .expect("parent creates basic-auth");

    let child_ctx = make_ctx(Uuid::new_v4(), child);
    let err = svc_child
        .put(
            &child_ctx,
            &key("k"),
            write_create_typed(SharingMode::Shared, "{\"a\":1}", "generic"),
            create_only(),
        )
        .await
        .expect_err("type mismatch with inherited");
    assert!(matches!(
        err,
        DomainError::TypeViolation {
            reason: crate::domain::secret::typing::reasons::TYPE_MISMATCH_WITH_INHERITED,
            ..
        }
    ));
}

#[tokio::test]
async fn declared_inherit_own_row_reports_declared_status_inherited_inheritance() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir_parent = Arc::new(FakeDir::single(parent));
    let dir_child = Arc::new(FakeDir::new(vec![child, parent]));
    let svc_parent = make_service_noop(repo.clone(), plugin.clone(), dir_parent);
    let svc_child = make_service_noop(repo.clone(), plugin.clone(), dir_child);

    let parent_ctx = make_ctx(owner, parent);
    svc_parent
        .put(
            &parent_ctx,
            &key("k"),
            write_create(SharingMode::Shared, "V1"),
            create_only(),
        )
        .await
        .expect("parent publishes shared");

    let child_ctx = make_ctx(owner, child);
    svc_child
        .put(
            &child_ctx,
            &key("k"),
            write_create(SharingMode::Shared, "V2"),
            create_only(),
        )
        .await
        .expect("child overrides");
    let cred_before = svc_child
        .get(&child_ctx, &key("k"))
        .await
        .expect("get")
        .expect("cred");
    svc_child
        .patch(
            &child_ctx,
            &key("k"),
            patch_value_null_keep_inherit(),
            matches(
                cred_before.validator.expect("some").id,
                cred_before.validator.expect("some").version,
            ),
        )
        .await
        .expect("remove value, keep fallback: inherit");

    let cred = svc_child
        .get(&child_ctx, &key("k"))
        .await
        .expect("get")
        .expect("cred");
    assert_eq!(cred.status, CredentialStatus::Declared);
    assert_eq!(cred.inheritance, InheritanceStatus::Inherited);
    // The record's own row is `declared` (no value of its own) even though
    // the *value* resolves from the parent, so `owner_id` still reports the
    // child's own creator, not the parent's.
    assert_eq!(cred.owner_id, Some(OwnerId(owner)));
    assert_eq!(
        svc_child
            .get_secret(&child_ctx, &key("k"))
            .await
            .expect("get_secret")
            .expect("v1")
            .secret
            .as_bytes(),
        b"V1"
    );
}

fn patch_value_null_keep_inherit() -> CredentialPatch {
    CredentialPatch {
        secret: PatchField::Null,
        ..empty_patch()
    }
}

#[tokio::test]
async fn patch_value_on_declared_row_switches_it_to_active() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v1"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();

    svc.patch(
        &ctx,
        &key("k"),
        patch_value_null_keep_inherit(),
        matches(row.id, row.version),
    )
    .await
    .expect("remove value");
    assert_eq!(repo.rows()[0].status, SecretStatus::Declared);

    let declared = repo.rows()[0].clone();
    svc.patch(
        &ctx,
        &key("k"),
        patch_value("v2"),
        matches(declared.id, declared.version),
    )
    .await
    .expect("write value onto a declared row");
    assert_eq!(repo.rows()[0].status, SecretStatus::Active);
    assert_eq!(
        svc.get_secret(&ctx, &key("k"))
            .await
            .expect("get_secret")
            .expect("v2")
            .secret
            .as_bytes(),
        b"v2"
    );
}

// ── ADDENDUM 3: Credential.owner_id ─────────────────────────────────────────

#[tokio::test]
async fn get_reports_the_creators_subject_id_as_owner_id_for_an_own_row() {
    let tenant = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo, plugin, dir);
    let ctx = make_ctx(subject, tenant);

    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");

    let cred = svc.get(&ctx, &key("k")).await.expect("get").expect("some");
    assert_eq!(cred.owner_id, Some(OwnerId(subject)));
}

// ── ADR-0004: resolve_credential's weak-validator source ────────────────────

#[tokio::test]
async fn resolve_credential_carries_the_winner_identity_when_no_own_row() {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir_parent = Arc::new(FakeDir::single(parent));
    let dir_child = Arc::new(FakeDir::new(vec![child, parent]));
    let svc_parent = make_service_noop(repo.clone(), plugin.clone(), dir_parent);
    let svc_child = make_service_noop(repo.clone(), plugin.clone(), dir_child);

    let parent_ctx = make_ctx(owner, parent);
    svc_parent
        .put(
            &parent_ctx,
            &key("k"),
            write_create(SharingMode::Shared, "V1"),
            create_only(),
        )
        .await
        .expect("parent publishes");
    let parent_row = repo.rows()[0].clone();

    let child_ctx = make_ctx(Uuid::new_v4(), child);
    let (cred, weak_source) = svc_child
        .resolve_credential(&child_ctx, &key("k"))
        .await
        .expect("resolve_credential")
        .expect("some");
    assert!(cred.validator.is_none());
    // No own row in play: the ancestor's owner id must never leak across
    // the tenant boundary (ADDENDUM 3) — reads it in its own tenant's
    // context instead.
    assert!(cred.owner_id.is_none());
    assert_eq!(weak_source, Some((parent_row.id, parent_row.version)));
}

// ── ADR-0004 Amendment A: Service::get_item (projection-aware point read) ───

#[tokio::test]
async fn get_item_without_select_evaluates_read_only_and_carries_no_value() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let (enforcer, resolver) = type_recording_enforcer();
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");

    let item = svc
        .get_item(&ctx, &key("k"), None)
        .await
        .expect("get_item")
        .expect("item");
    assert!(item.value.is_none());
    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::READ.to_owned()));
    assert!(
        !seen.contains(&crate::domain::authz::actions::READ_SECRET.to_owned()),
        "no $select must never evaluate read_secret: {seen:?}"
    );
}

#[tokio::test]
async fn get_item_select_secret_evaluates_read_secret_only_and_carries_the_value() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let (enforcer, resolver) = type_recording_enforcer();
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");

    let fields = ["secret".to_owned()];
    let item = svc
        .get_item(&ctx, &key("k"), Some(&fields))
        .await
        .expect("get_item")
        .expect("item");
    assert_eq!(item.value.expect("value present").as_bytes(), b"v");
    assert_eq!(
        item.credential.reference.as_ref(),
        "k",
        "envelope fields ride along"
    );
    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::READ_SECRET.to_owned()));
    assert!(
        !seen.contains(&crate::domain::authz::actions::READ.to_owned()),
        "a pure value projection must not evaluate read: {seen:?}"
    );
}

#[tokio::test]
async fn get_item_select_sharing_and_secret_evaluates_both_actions() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let (enforcer, resolver) = type_recording_enforcer();
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");

    let fields = ["sharing".to_owned(), "secret".to_owned()];
    let item = svc
        .get_item(&ctx, &key("k"), Some(&fields))
        .await
        .expect("get_item")
        .expect("item");
    assert!(item.value.is_some());
    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::READ.to_owned()));
    assert!(seen.contains(&crate::domain::authz::actions::READ_SECRET.to_owned()));
}

#[tokio::test]
async fn get_item_denial_of_read_secret_with_secret_selected_returns_none() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc_setup = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc_setup
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");

    let denied_type = SecretType::generic().gts_id().to_owned();
    let (enforcer, _resolver) =
        action_deny_enforcer(denied_type, crate::domain::authz::actions::READ_SECRET);
    let svc = make_service(repo, plugin, dir, enforcer, Arc::new(NoopMetrics));

    let fields = ["secret".to_owned()];
    let item = svc
        .get_item(&ctx, &key("k"), Some(&fields))
        .await
        .expect("get_item");
    assert!(item.is_none());
}

#[tokio::test]
async fn get_item_suppressed_winner_with_secret_only_selected_returns_none_but_record_still_visible()
 {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();
    svc.patch(
        &ctx,
        &key("k"),
        patch_value_null_keep_inherit(),
        matches(row.id, row.version),
    )
    .await
    .expect("remove value -> declared");

    // A pure value-only projection of a value-less (declared) row is the
    // canonical miss, exactly as the withdrawn `GET …/secret` was.
    let value_only = ["secret".to_owned()];
    let item = svc
        .get_item(&ctx, &key("k"), Some(&value_only))
        .await
        .expect("get_item");
    assert!(item.is_none());

    // The same value-less row, projected together with an administrative
    // field, is a record with no `value` instead of a miss.
    let with_admin = ["sharing".to_owned(), "secret".to_owned()];
    let item = svc
        .get_item(&ctx, &key("k"), Some(&with_admin))
        .await
        .expect("get_item")
        .expect("record still visible");
    assert!(item.value.is_none());
    assert_eq!(item.credential.status, CredentialStatus::Declared);
}

#[tokio::test]
async fn get_item_rejects_an_unknown_select_field() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo, plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let fields = ["bogus".to_owned()];
    let err = svc
        .get_item(&ctx, &key("k"), Some(&fields))
        .await
        .expect_err("must reject");
    assert!(matches!(
        err,
        DomainError::InvalidRequest {
            reason: "INVALID_SELECT",
            ..
        }
    ));
}

// ── ADR-0004 Amendment B: create/replace with an explicit `null` value ──────

#[tokio::test]
async fn put_create_with_explicit_null_creates_a_declared_row_write_only() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let (enforcer, resolver) = type_recording_enforcer();
    let svc = make_service(
        repo.clone(),
        plugin.clone(),
        dir,
        enforcer,
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);

    let outcome = svc
        .put(
            &ctx,
            &key("k"),
            write_create_null(SharingMode::Tenant, SdkFallback::Inherit),
            create_only(),
        )
        .await
        .expect("create with null");
    assert!(outcome.created);
    assert_eq!(outcome.validator.version, 1);

    let row = repo.rows()[0].clone();
    assert_eq!(row.status, SecretStatus::Declared);
    assert!(row.value_version.is_none());
    assert!(
        repo.purged_keys().is_empty(),
        "a value-less create touches no store key"
    );
    assert!(
        plugin.destroy_calls().is_empty() && plugin.get_calls() == 0,
        "a value-less create must never call the plugin"
    );
    assert!(!plugin.holds_key(&row.store_key()));

    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(
        !seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()),
        "a null create must evaluate write alone: {seen:?}"
    );
}

#[tokio::test]
async fn put_create_null_suppresses_an_inherited_credential_t1_t2_t3() {
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let t3 = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir_t1 = Arc::new(FakeDir::single(t1));
    let dir_t2 = Arc::new(FakeDir::new(vec![t2, t1]));
    let dir_t3 = Arc::new(FakeDir::new(vec![t3, t2, t1]));

    let svc_t1 = make_service_noop(repo.clone(), plugin.clone(), dir_t1);
    let svc_t3 = make_service_noop(repo.clone(), plugin.clone(), dir_t3);

    let ctx1 = make_ctx(Uuid::new_v4(), t1);
    let ctx2 = make_ctx(Uuid::new_v4(), t2);
    let ctx3 = make_ctx(Uuid::new_v4(), t3);
    let name = key("smtp-default");

    // T1 publishes a shared value; T3 (through T2) inherits it.
    svc_t1
        .put(
            &ctx1,
            &name,
            write_create(SharingMode::Shared, "V1"),
            create_only(),
        )
        .await
        .expect("T1 publishes");
    assert_eq!(
        svc_t3
            .get_secret(&ctx3, &name)
            .await
            .expect("t3 get_secret")
            .expect("v1")
            .secret
            .as_bytes(),
        b"V1"
    );

    // T2 suppresses the inherited credential without ever holding a value of
    // its own — one request, `write` alone (ADR-0004 Amendment B,
    // "Suppressing an inherited credential without ever holding a value of
    // your own is therefore one request too").
    let (enforcer, resolver) = type_recording_enforcer();
    let svc_t2_recording = make_service(
        repo.clone(),
        plugin.clone(),
        dir_t2.clone(),
        enforcer,
        Arc::new(NoopMetrics),
    );
    let outcome = svc_t2_recording
        .put(
            &ctx2,
            &name,
            write_create_null(SharingMode::Shared, SdkFallback::None),
            create_only(),
        )
        .await
        .expect("T2 suppresses with null");
    assert!(outcome.created);
    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(
        !seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()),
        "no value is named on either side of a null create: {seen:?}"
    );

    let svc_t2 = make_service_noop(repo.clone(), plugin.clone(), dir_t2);
    let t2_cred = svc_t2
        .get(&ctx2, &name)
        .await
        .expect("t2 get")
        .expect("cred");
    assert_eq!(t2_cred.status, CredentialStatus::Declared);
    assert_eq!(t2_cred.inheritance, InheritanceStatus::Suppressed);

    assert!(
        svc_t2
            .get_secret(&ctx2, &name)
            .await
            .expect("t2 get_secret")
            .is_none()
    );
    assert!(
        svc_t3
            .get_secret(&ctx3, &name)
            .await
            .expect("t3 get_secret")
            .is_none(),
        "T3 must miss once T2 suppresses the subtree"
    );
    assert_eq!(
        svc_t1
            .get_secret(&ctx1, &name)
            .await
            .expect("t1 get_secret")
            .expect("still v1")
            .secret
            .as_bytes(),
        b"V1",
        "T1's own record and value are untouched"
    );
}

#[tokio::test]
async fn put_null_on_active_row_removes_the_value_in_one_transaction_and_cleans_up_the_old_version()
{
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc_setup = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc_setup
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v1"),
            create_only(),
        )
        .await
        .expect("create");
    let row = repo.rows()[0].clone();
    let old_version = row.value_version.clone().expect("value_version");

    let (enforcer, resolver) = type_recording_enforcer();
    let svc = make_service(
        repo.clone(),
        plugin.clone(),
        dir,
        enforcer,
        Arc::new(NoopMetrics),
    );
    let outcome = svc
        .put(
            &ctx,
            &key("k"),
            write_replace_null(SharingMode::Tenant, SdkFallback::Inherit),
            put_matches(row.id, row.version),
        )
        .await
        .expect("replace with null");
    assert!(!outcome.created);
    assert_eq!(outcome.validator.version, row.version + 1);

    let after = repo.rows()[0].clone();
    assert_eq!(after.status, SecretStatus::Declared);
    assert!(after.value_version.is_none());
    assert!(
        !plugin.contains(&row.store_key(), &old_version),
        "the removed version must be destroyed"
    );
    assert!(
        plugin.delete_key_calls().is_empty(),
        "removing a secret never deletes the key"
    );

    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(
        seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()),
        "removing an existing value requires write_secret: {seen:?}"
    );
}

#[tokio::test]
async fn put_null_on_declared_row_replaces_metadata_without_write_secret() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc_setup = make_service_noop(repo.clone(), plugin.clone(), dir.clone());
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc_setup
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v1"),
            create_only(),
        )
        .await
        .expect("create");
    let row = repo.rows()[0].clone();
    svc_setup
        .patch(
            &ctx,
            &key("k"),
            patch_value_null_keep_inherit(),
            matches(row.id, row.version),
        )
        .await
        .expect("remove value -> declared");
    let declared = repo.rows()[0].clone();
    assert_eq!(declared.status, SecretStatus::Declared);

    let (enforcer, resolver) = type_recording_enforcer();
    let svc = make_service(repo.clone(), plugin, dir, enforcer, Arc::new(NoopMetrics));
    // A genuine metadata change (sharing, still non-private) so this does
    // not take the no-op shortcut below.
    let outcome = svc
        .put(
            &ctx,
            &key("k"),
            write_replace_null(SharingMode::Shared, SdkFallback::Inherit),
            put_matches(declared.id, declared.version),
        )
        .await
        .expect("metadata-only replace");
    assert!(!outcome.created);
    assert_eq!(outcome.validator.version, declared.version + 1);
    let after = repo.rows()[0].clone();
    assert_eq!(after.sharing, SharingMode::Shared);
    assert_eq!(after.status, SecretStatus::Declared);

    let seen = resolver.seen_actions();
    assert!(seen.contains(&crate::domain::authz::actions::WRITE.to_owned()));
    assert!(
        !seen.contains(&crate::domain::authz::actions::WRITE_SECRET.to_owned()),
        "replacing an already value-less row's metadata needs write alone: {seen:?}"
    );
}

#[tokio::test]
async fn put_null_on_declared_row_is_a_no_op_when_metadata_is_unchanged() {
    let tenant = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let dir = Arc::new(FakeDir::single(tenant));
    let svc = make_service_noop(repo.clone(), plugin, dir);
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create(SharingMode::Tenant, "v1"),
        create_only(),
    )
    .await
    .expect("create");
    let row = repo.rows()[0].clone();
    svc.patch(
        &ctx,
        &key("k"),
        patch_value_null_keep_inherit(),
        matches(row.id, row.version),
    )
    .await
    .expect("remove value -> declared");
    let declared = repo.rows()[0].clone();

    let outcome = svc
        .put(
            &ctx,
            &key("k"),
            write_replace_null(declared.sharing, SdkFallback::Inherit),
            put_matches(declared.id, declared.version),
        )
        .await
        .expect("no-op replace");
    assert!(!outcome.created);
    assert_eq!(
        outcome.validator.version, declared.version,
        "a metadata-unchanged null replace must not bump the version"
    );
    assert_eq!(repo.rows()[0].version, declared.version);
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn seeded_row(
    tenant: Uuid,
    owner: Uuid,
    reference: &str,
    sharing: SharingMode,
) -> crate::domain::secret::model::SecretRow {
    crate::domain::secret::model::SecretRow {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: reference.to_owned(),
        sharing,
        owner_id: OwnerId(owner),
        status: SecretStatus::Active,
        version: 1,
        updated_at: OffsetDateTime::now_utc(),
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_version: Some(ValueVersion::new("1")),
        fallback: Fallback::Inherit,
    }
}

// ── anti-enumeration: authorize before any row lookup (DESIGN 7.1) ──────────
//
// A caller without permission on a record MUST NOT be able to tell whether it
// exists: the PDP decision comes first, the caller's own row second, the
// precondition last.

/// One tenant with: `k` (generic, tenant-shared) and `a` (api-key). The
/// enforcer denies the generic type only, so the caller is permitted for
/// api-key but not for the type of `k`.
fn partial_world() -> (Service, Arc<FakeSecretRepo>, SecurityContextPair) {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let generic_gts = SecretType::generic().gts_id().to_owned();
    let (enforcer, _) = type_deny_enforcer(vec![generic_gts]);
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(seeded_row(tenant, owner, "k", SharingMode::Tenant));
    let mut a = seeded_row(tenant, owner, "a", SharingMode::Tenant);
    a.secret_type_uuid = SecretType::from_name("api-key").expect("type").uuid();
    repo.seed(a);
    let svc = make_service(
        repo.clone(),
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        enforcer,
        Arc::new(NoopMetrics),
    );
    (svc, repo, (make_ctx(owner, tenant), tenant, owner))
}

type SecurityContextPair = (toolkit_security::SecurityContext, Uuid, Uuid);

/// A world where the caller may do everything; `k` exists, `gone` does not.
fn permitted_world() -> (Service, Arc<FakeSecretRepo>, SecurityContextPair) {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(seeded_row(tenant, owner, "k", SharingMode::Tenant));
    let svc = make_service_noop(
        repo.clone(),
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
    );
    (svc, repo, (make_ctx(owner, tenant), tenant, owner))
}

/// A world where the caller may do nothing; `k` exists, `gone` does not.
fn denied_world() -> (Service, Arc<FakeSecretRepo>, SecurityContextPair) {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(seeded_row(tenant, owner, "k", SharingMode::Tenant));
    let svc = make_service(
        repo.clone(),
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        deny_enforcer(),
        Arc::new(NoopMetrics),
    );
    (svc, repo, (make_ctx(owner, tenant), tenant, owner))
}

fn assert_denied<T: std::fmt::Debug>(r: &Result<T, DomainError>, what: &str) {
    assert!(
        matches!(r, Err(DomainError::AccessDenied { .. })),
        "{what}: expected AccessDenied, got {r:?}"
    );
}

// delete

#[tokio::test]
async fn delete_without_permission_answers_the_same_for_existing_and_missing() {
    let (svc, repo, (ctx, ..)) = denied_world();
    assert_denied(&svc.delete(&ctx, &key("k"), exists()).await, "existing");
    assert_denied(&svc.delete(&ctx, &key("gone"), exists()).await, "missing");
    // A precondition that would not match must not change the answer.
    assert_denied(
        &svc.delete(&ctx, &key("k"), matches(Uuid::new_v4(), 99))
            .await,
        "existing, wrong If-Match",
    );
    assert_eq!(repo.rows().len(), 1, "nothing deleted");
}

#[tokio::test]
async fn delete_of_a_row_of_an_unpermitted_type_is_the_missing_answer() {
    let (svc, repo, (ctx, ..)) = partial_world();
    for (what, pre) in [
        ("If-Match *", exists()),
        ("wrong If-Match", matches(Uuid::new_v4(), 99)),
    ] {
        let on_unpermitted = svc.delete(&ctx, &key("k"), pre).await;
        assert!(
            matches!(on_unpermitted, Err(DomainError::NotFound)),
            "{what}: {on_unpermitted:?}"
        );
    }
    let on_missing = svc.delete(&ctx, &key("gone"), exists()).await;
    assert!(matches!(on_missing, Err(DomainError::NotFound)));
    assert_eq!(repo.rows().len(), 2, "nothing deleted");
}

#[tokio::test]
async fn delete_by_a_permitted_caller_keeps_409_and_404() {
    let (svc, repo, (ctx, ..)) = permitted_world();
    let wrong = svc
        .delete(&ctx, &key("k"), matches(Uuid::new_v4(), 99))
        .await;
    assert!(matches!(wrong, Err(DomainError::VersionConflict)));
    let missing = svc.delete(&ctx, &key("gone"), exists()).await;
    assert!(matches!(missing, Err(DomainError::NotFound)));
    svc.delete(&ctx, &key("k"), exists()).await.expect("delete");
    assert!(repo.rows().is_empty());
}

#[tokio::test]
async fn denied_and_not_found_writes_are_not_audited() {
    let (svc, _repo, (ctx, ..)) = partial_world();
    let audit = RecordingAudit::new();
    let svc = svc.with_audit(audit.clone());
    assert!(svc.delete(&ctx, &key("k"), exists()).await.is_err());
    assert!(svc.delete(&ctx, &key("gone"), exists()).await.is_err());
    assert!(
        svc.patch(&ctx, &key("k"), patch_value("x"), exists())
            .await
            .is_err()
    );
    assert!(audit.events().is_empty(), "{:?}", audit.events());
}

#[tokio::test]
async fn pdp_outage_is_503_even_for_a_missing_target() {
    let tenant = Uuid::new_v4();
    let svc = make_service(
        Arc::new(FakeSecretRepo::new()),
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        failing_enforcer(),
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let r = svc.delete(&ctx, &key("gone"), exists()).await;
    assert!(matches!(r, Err(DomainError::ServiceUnavailable { .. })));
    let r = svc
        .put(
            &ctx,
            &key("gone"),
            write_replace(SharingMode::Tenant, "v"),
            put_exists(),
        )
        .await;
    assert!(matches!(r, Err(DomainError::ServiceUnavailable { .. })));
}

// replace (PUT with If-Match)

#[tokio::test]
async fn replace_without_permission_answers_the_same_for_existing_and_missing() {
    let (svc, repo, (ctx, ..)) = denied_world();
    let before = repo.rows()[0].clone();
    for (what, k, pre) in [
        ("existing", "k", put_exists()),
        ("missing", "gone", put_exists()),
        (
            "existing, wrong If-Match",
            "k",
            put_matches(Uuid::new_v4(), 99),
        ),
    ] {
        let r = svc
            .put(&ctx, &key(k), write_replace(SharingMode::Tenant, "v"), pre)
            .await;
        assert_denied(&r, what);
    }
    // The request names the type: authorized on it, whatever exists.
    for k in ["k", "gone"] {
        let r = svc
            .put(
                &ctx,
                &key(k),
                write_create_typed(SharingMode::Tenant, "v", "generic"),
                put_exists(),
            )
            .await;
        assert_denied(&r, "named type");
    }
    assert_eq!(repo.rows()[0].version, before.version);
}

#[tokio::test]
async fn replace_of_a_row_of_an_unpermitted_type_is_the_missing_answer() {
    let (svc, repo, (ctx, ..)) = partial_world();
    let k = repo
        .rows()
        .into_iter()
        .find(|r| r.reference == "k")
        .expect("k");
    let cases = [
        ("If-Match *", put_exists()),
        ("right If-Match", put_matches(k.id, k.version)),
        ("wrong If-Match", put_matches(Uuid::new_v4(), 99)),
    ];
    for (what, pre) in cases {
        let r = svc
            .put(
                &ctx,
                &key("k"),
                write_replace(SharingMode::Tenant, "v"),
                pre,
            )
            .await;
        assert!(
            matches!(r, Err(DomainError::VersionConflict)),
            "{what}: {r:?}"
        );
    }
    let missing = svc
        .put(
            &ctx,
            &key("gone"),
            write_replace(SharingMode::Tenant, "v"),
            put_exists(),
        )
        .await;
    assert!(matches!(missing, Err(DomainError::VersionConflict)));
    let after = repo
        .rows()
        .into_iter()
        .find(|r| r.reference == "k")
        .expect("k");
    assert_eq!(after.version, k.version, "the row was not touched");
}

#[tokio::test]
async fn replace_by_a_permitted_caller_keeps_its_409s() {
    let (svc, repo, (ctx, ..)) = permitted_world();
    let wrong = svc
        .put(
            &ctx,
            &key("k"),
            write_replace(SharingMode::Tenant, "v"),
            put_matches(Uuid::new_v4(), 99),
        )
        .await;
    assert!(matches!(wrong, Err(DomainError::VersionConflict)));
    let missing = svc
        .put(
            &ctx,
            &key("gone"),
            write_replace(SharingMode::Tenant, "v"),
            put_exists(),
        )
        .await;
    assert!(matches!(missing, Err(DomainError::VersionConflict)));
    let k = repo.rows()[0].clone();
    svc.put(
        &ctx,
        &key("k"),
        write_replace(SharingMode::Tenant, "v"),
        put_matches(k.id, k.version),
    )
    .await
    .expect("replace");
}

#[tokio::test]
async fn replace_naming_a_different_type_is_type_immutable_only_when_permitted_on_both() {
    // Permitted on generic and api-key: the stored type is the caller's to
    // know, so the type change is reported as such.
    let (svc, _repo, (ctx, ..)) = permitted_world();
    let r = svc
        .put(
            &ctx,
            &key("k"),
            write_create_typed(SharingMode::Tenant, "v", "api-key"),
            put_exists(),
        )
        .await;
    assert!(matches!(r, Err(DomainError::TypeViolation { .. })), "{r:?}");
    // Permitted on api-key only: the generic row is "missing".
    let (svc, _repo, (ctx, ..)) = partial_world();
    let r = svc
        .put(
            &ctx,
            &key("k"),
            write_create_typed(SharingMode::Tenant, "v", "api-key"),
            put_exists(),
        )
        .await;
    assert!(matches!(r, Err(DomainError::VersionConflict)), "{r:?}");
}

// patch

#[tokio::test]
async fn patch_without_permission_answers_the_same_for_existing_and_missing() {
    let (svc, repo, (ctx, ..)) = denied_world();
    for (what, k, pre) in [
        ("existing", "k", exists()),
        ("missing", "gone", exists()),
        ("existing, wrong If-Match", "k", matches(Uuid::new_v4(), 99)),
    ] {
        let r = svc.patch(&ctx, &key(k), patch_value("x"), pre).await;
        assert_denied(&r, what);
        let r = svc
            .patch(&ctx, &key(k), patch_sharing(SharingMode::Shared), exists())
            .await;
        assert_denied(&r, what);
    }
    assert_eq!(repo.rows()[0].version, 1);
}

#[tokio::test]
async fn patch_of_a_row_of_an_unpermitted_type_is_the_missing_answer() {
    let (svc, repo, (ctx, ..)) = partial_world();
    for (what, k, pre) in [
        ("unpermitted type", "k", exists()),
        (
            "unpermitted type, wrong If-Match",
            "k",
            matches(Uuid::new_v4(), 99),
        ),
        ("missing", "gone", exists()),
    ] {
        let r = svc.patch(&ctx, &key(k), patch_value("x"), pre).await;
        assert!(matches!(r, Err(DomainError::NotFound)), "{what}: {r:?}");
        let r = svc
            .patch(&ctx, &key(k), patch_sharing(SharingMode::Shared), exists())
            .await;
        assert!(matches!(r, Err(DomainError::NotFound)), "{what}: {r:?}");
    }
    let k = repo
        .rows()
        .into_iter()
        .find(|r| r.reference == "k")
        .expect("k");
    assert_eq!(k.version, 1, "the row was not touched");
}

#[tokio::test]
async fn patch_by_a_permitted_caller_keeps_409_and_404() {
    let (svc, _repo, (ctx, ..)) = permitted_world();
    let wrong = svc
        .patch(
            &ctx,
            &key("k"),
            patch_value("x"),
            matches(Uuid::new_v4(), 99),
        )
        .await;
    assert!(matches!(wrong, Err(DomainError::VersionConflict)));
    let missing = svc
        .patch(&ctx, &key("gone"), patch_value("x"), exists())
        .await;
    assert!(matches!(missing, Err(DomainError::NotFound)));
}

#[tokio::test]
async fn empty_patch_is_rejected_independently_of_existence() {
    let (svc, _repo, (ctx, ..)) = denied_world();
    for k in ["k", "gone"] {
        let r = svc.patch(&ctx, &key(k), empty_patch(), exists()).await;
        assert!(
            matches!(r, Err(DomainError::InvalidRequest { .. })),
            "{r:?}"
        );
    }
}

// create (PUT with If-None-Match: *)

#[tokio::test]
async fn create_without_permission_answers_the_same_over_an_existing_and_a_free_name() {
    let (svc, repo, (ctx, ..)) = denied_world();
    for k in ["k", "gone"] {
        let r = svc
            .put(
                &ctx,
                &key(k),
                write_create(SharingMode::Tenant, "v"),
                create_only(),
            )
            .await;
        assert_denied(&r, k);
    }
    assert_eq!(repo.rows().len(), 1, "nothing created");
}

#[tokio::test]
async fn create_over_a_row_of_an_unpermitted_type_answers_like_any_collision() {
    let (svc, repo, (ctx, ..)) = partial_world();
    // `k` is generic (not writable by the caller), `a` is api-key (writable):
    // a name collision is the same 409 whatever the occupant is.
    for k in ["k", "a"] {
        let r = svc
            .put(
                &ctx,
                &key(k),
                write_create_typed(SharingMode::Tenant, "v", "api-key"),
                create_only(),
            )
            .await;
        assert!(matches!(r, Err(DomainError::Conflict)), "{k}: {r:?}");
    }
    // A free name still creates.
    svc.put(
        &ctx,
        &key("free"),
        write_create_typed(SharingMode::Tenant, "v", "api-key"),
        create_only(),
    )
    .await
    .expect("create");
    assert_eq!(repo.rows().len(), 3);
}

#[tokio::test]
async fn create_by_a_permitted_caller_keeps_409_and_201() {
    let (svc, _repo, (ctx, ..)) = permitted_world();
    let r = svc
        .put(
            &ctx,
            &key("k"),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await;
    assert!(matches!(r, Err(DomainError::Conflict)));
    let out = svc
        .put(
            &ctx,
            &key("new"),
            write_create(SharingMode::Tenant, "v"),
            create_only(),
        )
        .await
        .expect("create");
    assert!(out.created);
}

#[tokio::test]
async fn create_without_a_type_is_rejected_independently_of_existence() {
    let (svc, _repo, (ctx, ..)) = denied_world();
    for k in ["k", "gone"] {
        let r = svc
            .put(
                &ctx,
                &key(k),
                write_replace(SharingMode::Tenant, "v"),
                create_only(),
            )
            .await;
        assert!(
            matches!(r, Err(DomainError::InvalidRequest { .. })),
            "{r:?}"
        );
    }
}

// ── one PDP evaluation per action, whatever the number of types (ADR-0010) ──
//
// The PDP answers the BASE credential type with a constraint on the type
// property, which the secure ORM applies in SQL. The number of evaluations is
// the number of actions the operation needs - never a function of how many
// credential types exist or which ones the tenant holds.

/// A tenant holding one tenant-shared row for EVERY built-in credential type
/// (`t-<name>` references), served by a counting PDP.
type ManyTypesWorld = (
    Service,
    Arc<FakeSecretRepo>,
    Arc<CountingAuthZResolver>,
    SecurityContextPair,
    Vec<(String, Uuid)>,
);

fn many_types_world() -> ManyTypesWorld {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let mut refs = Vec::new();
    for d in credstore_sdk::SECRET_TYPE_CATALOG {
        let Some(type_uuid) = credstore_sdk::types::type_uuid(d.gts_id) else {
            continue;
        };
        let reference = format!("t-{}", refs.len());
        let mut row = seeded_row(tenant, owner, &reference, SharingMode::Tenant);
        row.secret_type_uuid = type_uuid;
        repo.seed(row);
        refs.push((reference, type_uuid));
    }
    assert!(refs.len() >= 5, "the catalog must hold several types");
    let (enforcer, counter) = counting_enforcer();
    let svc = make_service(
        repo.clone(),
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        enforcer,
        Arc::new(NoopMetrics),
    );
    (
        svc,
        repo,
        counter,
        (make_ctx(owner, tenant), tenant, owner),
        refs,
    )
}

#[tokio::test]
async fn one_pdp_call_per_action_for_delete_patch_and_replace_regardless_of_type_count() {
    let (svc, repo, counter, (ctx, _, _), refs) = many_types_world();
    let before = |c: &CountingAuthZResolver| c.calls();

    // delete: ONE `delete` evaluation.
    let base = before(&counter);
    let (reference, _) = &refs[0];
    let row = repo
        .rows()
        .into_iter()
        .find(|r| &r.reference == reference)
        .expect("row");
    svc.delete(&ctx, &key(reference), matches(row.id, row.version))
        .await
        .expect("delete");
    assert_eq!(counter.calls() - base, 1, "delete: one evaluation");

    // patch, metadata only: ONE `write` evaluation.
    let base = before(&counter);
    let (reference, _) = &refs[1];
    svc.patch(
        &ctx,
        &key(reference),
        patch_sharing(SharingMode::Shared),
        exists(),
    )
    .await
    .expect("patch metadata");
    assert_eq!(counter.calls() - base, 1, "patch metadata: one evaluation");

    // patch, value only: ONE `write_secret` evaluation.
    let base = before(&counter);
    svc.patch(&ctx, &key(reference), patch_value("v2"), exists())
        .await
        .expect("patch value");
    assert_eq!(counter.calls() - base, 1, "patch value: one evaluation");

    // replace with a value: `write` + `write_secret`, one evaluation each.
    let base = before(&counter);
    svc.put(
        &ctx,
        &key(reference),
        write_replace(SharingMode::Shared, "v3"),
        put_exists(),
    )
    .await
    .expect("replace");
    assert_eq!(
        counter.calls() - base,
        2,
        "replace: one evaluation per action"
    );
}

#[tokio::test]
async fn one_pdp_call_per_action_for_reads_and_lists_regardless_of_type_count() {
    let (svc, _repo, counter, (ctx, _, _), refs) = many_types_world();

    let base = counter.calls();
    let page = svc
        .list(&ctx, &toolkit_odata::ODataQuery::new())
        .await
        .expect("list");
    assert_eq!(page.items.len(), refs.len(), "every type is listed");
    assert_eq!(counter.calls() - base, 1, "list: one evaluation");

    let base = counter.calls();
    svc.get(&ctx, &key(&refs[2].0)).await.expect("get");
    assert_eq!(counter.calls() - base, 1, "point read: one evaluation");
}

#[tokio::test]
async fn existing_row_operations_evaluate_the_base_type_and_create_the_concrete_type() {
    let (enforcer, resolver) = type_recording_enforcer();
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    let svc = make_service(
        repo,
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        enforcer,
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(owner, tenant);
    svc.put(
        &ctx,
        &key("k"),
        write_create_typed(SharingMode::Tenant, "v", "api-key"),
        create_only(),
    )
    .await
    .expect("create");
    svc.patch(
        &ctx,
        &key("k"),
        patch_sharing(SharingMode::Shared),
        exists(),
    )
    .await
    .expect("patch");
    let seen = resolver.seen_resource_types();
    let base = credstore_sdk::CREDENTIAL_RESOURCE_TYPE;
    let api_key = SecretType::from_name("api-key")
        .expect("known")
        .gts_id()
        .to_owned();
    // create: write + write_secret on the requested concrete type; patch: write on the base.
    assert_eq!(seen, vec![api_key.clone(), api_key, base.to_owned()]);
}

#[tokio::test]
async fn a_pdp_denial_is_403_for_every_target_whether_or_not_the_record_exists() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let repo = Arc::new(FakeSecretRepo::new());
    repo.seed(seeded_row(tenant, owner, "present", SharingMode::Tenant));
    let svc = make_service(
        repo,
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        deny_enforcer(),
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(owner, tenant);
    for reference in ["present", "absent"] {
        let k = key(reference);
        let denied = |r: Result<(), DomainError>| {
            assert!(
                matches!(r, Err(DomainError::AccessDenied { .. })),
                "{reference}: {r:?}"
            );
        };
        denied(svc.delete(&ctx, &k, exists()).await);
        denied(
            svc.patch(&ctx, &k, patch_sharing(SharingMode::Shared), exists())
                .await
                .map(|_| ()),
        );
        denied(
            svc.patch(&ctx, &k, patch_value("v"), exists())
                .await
                .map(|_| ()),
        );
        denied(
            svc.put(
                &ctx,
                &k,
                write_replace(SharingMode::Tenant, "v"),
                put_exists(),
            )
            .await
            .map(|_| ()),
        );
        denied(
            svc.put(
                &ctx,
                &k,
                write_create(SharingMode::Tenant, "v"),
                create_only(),
            )
            .await
            .map(|_| ()),
        );
    }
}

#[tokio::test]
async fn a_type_restricted_scope_hides_rows_of_other_types_and_leaves_them_untouched() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api_key = SecretType::from_name("api-key").expect("known");
    let repo = Arc::new(FakeSecretRepo::new());
    let mut hidden = seeded_row(tenant, owner, "hidden", SharingMode::Tenant);
    hidden.secret_type_uuid = api_key.uuid();
    repo.seed(hidden);
    repo.seed(seeded_row(tenant, owner, "visible", SharingMode::Tenant));
    // The caller's grants cover every type but api-key.
    let (enforcer, _) = type_deny_enforcer(vec![api_key.gts_id().to_owned()]);
    let svc = make_service(
        repo.clone(),
        FakePlugin::new(),
        Arc::new(FakeDir::single(tenant)),
        enforcer,
        Arc::new(NoopMetrics),
    );
    let ctx = make_ctx(owner, tenant);
    let hidden_key = key("hidden");

    assert!(matches!(
        svc.delete(&ctx, &hidden_key, exists()).await,
        Err(DomainError::NotFound)
    ));
    assert!(matches!(
        svc.patch(
            &ctx,
            &hidden_key,
            patch_sharing(SharingMode::Shared),
            exists()
        )
        .await,
        Err(DomainError::NotFound)
    ));
    assert!(matches!(
        svc.put(
            &ctx,
            &hidden_key,
            write_replace(SharingMode::Tenant, "v"),
            put_exists()
        )
        .await,
        Err(DomainError::VersionConflict)
    ));
    // A create over the taken name stays a plain 409 (the unique key has no
    // type), not a leak of the other type's row.
    assert!(matches!(
        svc.put(
            &ctx,
            &hidden_key,
            write_create(SharingMode::Tenant, "v"),
            create_only()
        )
        .await,
        Err(DomainError::Conflict)
    ));
    assert_eq!(repo.rows().len(), 2, "no row was deleted");
    let still_there = repo
        .rows()
        .into_iter()
        .find(|r| r.reference == "hidden")
        .expect("row");
    assert_eq!(still_there.version, 1);

    // The row of a permitted type is reachable as before.
    svc.delete(&ctx, &key("visible"), exists())
        .await
        .expect("delete visible");
}
