//! Targeted coverage for domain-layer branches left unexercised by the rest
//! of the suite: `PolicyService::list_retention_rules`'s non-admin
//! visibility filter (owner-kind/id pairing, dangling targets, error
//! propagation), `CleanupEngine`'s failure/race branches in the orphan and
//! retention sweeps, `FileService::create.rs`'s idempotency-replay error
//! paths and multipart-initiate compensation, a couple of small pure
//! `domain::multipart` branches, and the `list_versions` REST path that
//! exercises `manifests_for_versions`/`VersionDto`.
//!
//! Each `tests/*.rs` file is its own integration-test crate, so the small
//! test doubles below (`TestAuthorizer`, `FaultyRequireFileStore`,
//! `FaultyCleanupStore`) are self-contained copies of the patterns already
//! established in `tests/policy_authz_test.rs` / `tests/cleanup_test.rs`
//! rather than shared code.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::dto::VersionDto;
use file_storage::api::rest::handlers;
use file_storage::domain::audit::{AuditEntry, FileEvent};
use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer, actions};
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::{
    MultipartCompleteOutcome, MultipartUploadSession, MultipartUploadState, StoredCompleteResult,
};
use file_storage::domain::policy::{
    AgeRetention, MetadataRetention, PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope,
    StoredPolicy, StoredRetentionRule,
};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, DataPlanePort, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.domain_coverage_test.file.type.v1~");
const BASE: &str = "/api/file-storage/v1";

// ── shared test harness helpers ─────────────────────────────────────────────

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-domain-coverage-{}.db",
        Uuid::now_v7().simple()
    ));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    Arc::new(DBProvider::new(db))
}

fn base_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    }
}

fn ctx(tenant: Uuid, subject: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("valid SecurityContext")
}

/// Like [`ctx`] but stamping `subject_type`, for the `Self::actor_kind`
/// "app" normalization path.
fn ctx_with_type(tenant: Uuid, subject: Uuid, subject_type: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .subject_type(subject_type)
        .build()
        .expect("valid SecurityContext")
}

fn new_file(owner_id: Uuid, owner_kind: OwnerKind) -> NewFile {
    NewFile {
        owner_kind,
        owner_id,
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// A semantically valid retention-rule body (validation rejects
/// all-criteria-`None`), for tests whose focus is elsewhere.
fn valid_rule_body() -> RetentionRuleBody {
    RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 30 }),
        inactivity: None,
        metadata: None,
    }
}

// ── TestAuthorizer ───────────────────────────────────────────────────────────

/// Grants `READ`/`WRITE`/`DELETE` unconditionally; gates `ADMIN_POLICY` on
/// `is_admin`. `admin_probe_error`, when set, makes the `ADMIN_POLICY` probe
/// return `DomainError::InternalError` instead of `Ok`/`Forbidden` --
/// simulating an authorizer-side failure unrelated to permission, to prove
/// callers propagate it verbatim rather than folding it into "not admin".
#[derive(Default)]
struct TestAuthorizer {
    is_admin: AtomicBool,
    admin_probe_error: AtomicBool,
}

impl TestAuthorizer {
    fn new() -> Self {
        Self::default()
    }

    fn set_admin(&self, admin: bool) {
        self.is_admin.store(admin, Ordering::SeqCst);
    }

    fn set_admin_probe_error(&self, error: bool) {
        self.admin_probe_error.store(error, Ordering::SeqCst);
    }
}

#[async_trait]
impl Authorizer for TestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if action == actions::ADMIN_POLICY {
            if self.admin_probe_error.load(Ordering::SeqCst) {
                return Err(DomainError::InternalError);
            }
            return if self.is_admin.load(Ordering::SeqCst) {
                Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
            } else {
                Err(DomainError::Forbidden)
            };
        }
        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

// ── FaultyRequireFileStore (PolicyStore test double) ────────────────────────

/// A [`PolicyStore`] wrapper that makes `require_file` fail for one specific
/// `file_id` with an error OTHER than `FileNotFound`, delegating every other
/// method (and every other `file_id`) to a real [`Store`]. Used to prove
/// `PolicyService::list_retention_rules`'s per-rule `File`-scope resolution
/// propagates an unexpected store error instead of silently treating it like
/// a dangling target.
struct FaultyRequireFileStore {
    inner: Store,
    fault_file_id: Uuid,
}

#[async_trait]
impl PolicyStore for FaultyRequireFileStore {
    async fn require_file(&self, scope: &AccessScope, file_id: Uuid) -> Result<File, DomainError> {
        if file_id == self.fault_file_id {
            Err(DomainError::InternalError)
        } else {
            self.inner.require_file(scope, file_id).await
        }
    }

    async fn get_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        self.inner
            .get_policy(scope, tenant_id, policy_scope, scope_owner_id)
            .await
    }

    async fn upsert_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &PolicyBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        self.inner
            .upsert_policy(scope, tenant_id, policy_scope, scope_owner_id, body, now)
            .await
    }

    async fn list_retention_rules(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        self.inner.list_retention_rules(scope, tenant_id).await
    }

    async fn insert_retention_rule(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        retention_scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &RetentionRuleBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        self.inner
            .insert_retention_rule(
                scope,
                tenant_id,
                retention_scope,
                scope_target_id,
                body,
                now,
            )
            .await
    }

    async fn delete_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        self.inner.delete_retention_rule(scope, rule_id).await
    }

    async fn get_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<StoredRetentionRule>, DomainError> {
        self.inner.get_retention_rule(scope, rule_id).await
    }
}

// ── FaultyCleanupStore (CleanupStore test double) ───────────────────────────

/// A [`CleanupStore`] wrapper delegating every method to a real [`Store`],
/// except for a handful of configurable fault points used to drive
/// `CleanupEngine`'s error/race branches deterministically (mirrors
/// `tests/cleanup_test.rs`'s `FaultyListVersionsStore`, generalized to the
/// few extra methods this file's target lines need).
#[derive(Default)]
struct CleanupFaults {
    /// `get_file` errors for this one `file_id`; delegates otherwise.
    fault_get_file_for: Option<Uuid>,
    /// `list_expired_multipart_uploads` always errors.
    fault_list_expired_multipart: bool,
    /// `abort_multipart_upload` returns `Ok(false)` for this `upload_id`
    /// (simulating a lost CAS race) instead of delegating.
    force_abort_false_for: Option<Uuid>,
    /// `abort_multipart_upload` errors for this `upload_id` instead of
    /// delegating.
    force_abort_err_for: Option<Uuid>,
    /// `list_metadata_for_files` always errors.
    fault_list_metadata_for_files: bool,
}

struct FaultyCleanupStore {
    inner: Store,
    faults: CleanupFaults,
}

#[async_trait]
impl CleanupStore for FaultyCleanupStore {
    async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        self.inner
            .list_abandoned_pending_versions(older_than, now)
            .await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.delete_version(file_id, version_id, audit).await
    }

    async fn delete_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_pending_version(file_id, version_id, audit)
            .await
    }

    async fn list_expired_multipart_uploads(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        if self.faults.fault_list_expired_multipart {
            return Err(DomainError::InternalError);
        }
        self.inner.list_expired_multipart_uploads(now).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        if self.faults.force_abort_false_for == Some(upload_id) {
            return Ok(false);
        }
        if self.faults.force_abort_err_for == Some(upload_id) {
            return Err(DomainError::InternalError);
        }
        self.inner.abort_multipart_upload(upload_id, audit).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        self.inner.get_version(file_id, version_id).await
    }

    async fn list_all_retention_rules(&self) -> Result<Vec<StoredRetentionRule>, DomainError> {
        self.inner.list_all_retention_rules().await
    }

    async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        self.inner.list_all_files_for_sweep(after, limit).await
    }

    async fn list_metadata(&self, file_id: Uuid) -> Result<Vec<CustomMetadataEntry>, DomainError> {
        self.inner.list_metadata(file_id).await
    }

    async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        if self.faults.fault_list_metadata_for_files {
            return Err(DomainError::InternalError);
        }
        self.inner.list_metadata_for_files(file_ids).await
    }

    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        self.inner.list_versions(file_id).await
    }

    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError> {
        if self.faults.fault_get_file_for == Some(file_id) {
            return Err(DomainError::InternalError);
        }
        self.inner
            .get_file(&AccessScope::allow_all(), file_id)
            .await
    }

    async fn has_in_progress_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        self.inner.has_in_progress_multipart_for_file(file_id).await
    }

    async fn delete_file_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_file_with_event(scope, file_id, audit, event)
            .await
    }

    async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
    }

    async fn delete_expired_idempotency_keys(
        &self,
        now: OffsetDateTime,
    ) -> Result<u64, DomainError> {
        self.inner.delete_expired_idempotency_keys(now).await
    }
}

// =============================================================================
// domain/policy_service.rs -- list_retention_rules non-admin visibility filter
// =============================================================================

/// A non-admin `File`-scope rule check must compare the owner PAIR
/// (`owner_kind`, `owner_id`), not just the id -- `user` and `app` are
/// disjoint owner spaces that can share a UUID. Also exercises: the
/// `Self::actor_kind`-style `"app"` subject-kind normalization, a
/// `File`-scope rule with no `scope_target_id` (skipped via `continue`,
/// reachable only by bypassing `create_retention_rule`'s validation), and
/// the per-listing owner-lookup cache (two rules on the same file: the
/// second hits the cache instead of re-querying).
#[tokio::test]
async fn list_retention_rules_file_scope_visible_only_when_owner_kind_and_id_both_match() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authz = Arc::new(TestAuthorizer::new());
    let authorizer: Arc<dyn Authorizer> = Arc::clone(&authz) as Arc<dyn Authorizer>;
    let store = Store::new(Arc::clone(&db));
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let svc = FileService::new(
        store,
        backends,
        issuer,
        Arc::clone(&authorizer),
        base_config(),
        None,
        None,
    );
    let policy_svc = PolicyService::new(Arc::clone(&policy_store), Arc::clone(&authorizer));

    let tenant = Uuid::now_v7();
    let admin_id = Uuid::now_v7();
    let app_subject_id = Uuid::now_v7();
    let ctx_admin = ctx(tenant, admin_id);
    let ctx_app = ctx_with_type(tenant, app_subject_id, "app");

    // Admin creates two files under the SAME numeric owner id but disjoint
    // owner kinds, plus retention rules targeting each.
    authz.set_admin(true);
    let file_app_id = svc
        .create_file_bare(&ctx_admin, new_file(app_subject_id, OwnerKind::App))
        .await
        .expect("create app-owned file");
    let file_user_id = svc
        .create_file_bare(&ctx_admin, new_file(app_subject_id, OwnerKind::User))
        .await
        .expect("create user-owned file with the same numeric owner id");

    let rule_app_1 = policy_svc
        .create_retention_rule(
            &ctx_admin,
            RetentionScope::File,
            Some(file_app_id),
            valid_rule_body(),
        )
        .await
        .expect("create first rule on the app-owned file");
    let rule_app_2 = policy_svc
        .create_retention_rule(
            &ctx_admin,
            RetentionScope::File,
            Some(file_app_id),
            valid_rule_body(),
        )
        .await
        .expect("create second rule on the SAME app-owned file (cache-hit path)");
    let rule_user = policy_svc
        .create_retention_rule(
            &ctx_admin,
            RetentionScope::File,
            Some(file_user_id),
            valid_rule_body(),
        )
        .await
        .expect("create rule on the user-owned file");

    authz.set_admin(false);
    let visible = policy_svc
        .list_retention_rules(&ctx_app)
        .await
        .expect("list_retention_rules for the app subject");
    let visible_ids: std::collections::HashSet<Uuid> = visible.iter().map(|r| r.rule_id).collect();

    assert!(
        visible_ids.contains(&rule_app_1.rule_id),
        "the app-owned file's rule must be visible to the matching app subject"
    );
    assert!(
        visible_ids.contains(&rule_app_2.rule_id),
        "a second rule on the same file must also be visible via the owner cache"
    );
    assert!(
        !visible_ids.contains(&rule_user.rule_id),
        "a user-owned file with the SAME numeric owner id must not leak its rule to the app subject"
    );
    assert_eq!(
        visible_ids.len(),
        2,
        "exactly the two app-owned-file rules must be visible, no more: {visible_ids:?}"
    );
}

/// A `File`-scope rule whose target file has since been deleted must be
/// invisible to a non-admin caller: `StoredRetentionRule` carries no
/// creator column, so once the file is gone there is no stored fact left to
/// tell who may still see it.
#[tokio::test]
async fn list_retention_rules_file_scope_deleted_target_is_invisible_for_nonadmin() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TestAuthorizer::new());
    let store = Store::new(Arc::clone(&db));
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let svc = FileService::new(
        store,
        backends,
        issuer,
        Arc::clone(&authorizer),
        base_config(),
        None,
        None,
    );
    let policy_svc = PolicyService::new(policy_store, authorizer);

    let tenant = Uuid::now_v7();
    let user = Uuid::now_v7();
    let ctx_user = ctx(tenant, user);

    let file_id = svc
        .create_file_bare(&ctx_user, new_file(user, OwnerKind::User))
        .await
        .expect("create own file");
    let rule = policy_svc
        .create_retention_rule(
            &ctx_user,
            RetentionScope::File,
            Some(file_id),
            valid_rule_body(),
        )
        .await
        .expect("create file-scope rule on own file");

    svc.delete_file(&ctx_user, file_id, Some("*"))
        .await
        .expect("delete the target file");

    let visible = policy_svc
        .list_retention_rules(&ctx_user)
        .await
        .expect("list_retention_rules after target deleted");
    assert!(
        !visible.iter().any(|r| r.rule_id == rule.rule_id),
        "a rule whose target file is gone must not be visible to a non-admin caller"
    );
}

/// A `require_file` failure OTHER than `FileNotFound` (e.g. a transient
/// store error) must propagate as-is, not be swallowed the same way a
/// dangling target is.
#[tokio::test]
async fn list_retention_rules_require_file_error_other_than_not_found_propagates() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    // Never actually created as a file -- `FaultyRequireFileStore` always
    // errors for it regardless, so its non-existence is irrelevant.
    let fault_file_id = Uuid::now_v7();

    let real_policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    real_policy_store
        .insert_retention_rule(
            &AccessScope::allow_all(),
            tenant,
            &RetentionScope::File,
            Some(fault_file_id),
            &valid_rule_body(),
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("insert rule directly, bypassing PolicyService");

    let faulty_store: Arc<dyn PolicyStore> = Arc::new(FaultyRequireFileStore {
        inner: store,
        fault_file_id,
    });
    let authorizer: Arc<dyn Authorizer> = Arc::new(TestAuthorizer::new()); // non-admin by default
    let policy_svc = PolicyService::new(faulty_store, authorizer);

    let ctx_user = ctx(tenant, subject);
    let result = policy_svc.list_retention_rules(&ctx_user).await;
    assert!(
        matches!(result, Err(DomainError::InternalError)),
        "a require_file failure other than FileNotFound must propagate, got {result:?}"
    );
}

/// An `ADMIN_POLICY` probe failure other than `Forbidden` must propagate
/// as-is rather than being treated as "not an admin".
#[tokio::test]
async fn list_retention_rules_admin_probe_unexpected_error_propagates() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store);
    let authz = Arc::new(TestAuthorizer::new());
    authz.set_admin_probe_error(true);
    let authorizer: Arc<dyn Authorizer> = authz;
    let policy_svc = PolicyService::new(policy_store, authorizer);

    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_user = ctx(tenant, subject);

    let result = policy_svc.list_retention_rules(&ctx_user).await;
    assert!(
        matches!(result, Err(DomainError::InternalError)),
        "an ADMIN_POLICY probe error other than Forbidden must propagate as-is, got {result:?}"
    );
}

// =============================================================================
// domain/cleanup.rs -- orphan reconciliation and retention-sweep error/race
// branches
// =============================================================================

fn cleanup_ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("valid SecurityContext")
}

/// A file lookup failure during the post-delete orphan pre-check must be
/// treated as "do not delete" (fail-safe), never as license to remove the
/// parent `files` row.
#[tokio::test]
async fn cleanup_reclaim_skips_orphan_check_when_file_lookup_fails() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = cleanup_ctx(tenant);
    let ticket = svc
        .create_file(&ctx, new_file(owner, OwnerKind::User), None, false)
        .await
        .expect("create_file");

    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyCleanupStore {
        inner: store.clone(),
        faults: CleanupFaults {
            fault_get_file_for: Some(ticket.file_id),
            ..Default::default()
        },
    });
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 0,
        },
    );

    let (pending_deleted, files_deleted) = engine
        .delete_abandoned_pending_version(ticket.file_id, ticket.version_id, 0, "mem", "/x/y")
        .await;
    assert_eq!(
        pending_deleted, 1,
        "the pending version row itself must still be reclaimed"
    );
    assert_eq!(
        files_deleted, 0,
        "the orphan-file check must be skipped (not assumed-orphan) when the file lookup errors"
    );

    let file = store
        .get_file(&AccessScope::allow_all(), ticket.file_id)
        .await
        .expect("get_file via the real, unfaulted store");
    assert!(
        file.is_some(),
        "the parent file row must still exist -- a failed lookup must never be treated as \
         license to delete it"
    );
}

/// `cleanup_expired_session_version`'s orphan-file follow-up must tolerate a
/// `file_id` that no longer resolves to any row at all (both the version
/// list and the file lookup legitimately come back empty), simply declining
/// to reclaim anything rather than erroring.
#[tokio::test]
async fn cleanup_expired_session_version_with_nonexistent_file_is_noop() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let store = Store::new(Arc::clone(&db));
    let cleanup_store: Arc<dyn CleanupStore> = Arc::new(store);
    let engine = CleanupEngine::new(
        cleanup_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 0,
        },
    );

    let now = OffsetDateTime::now_utc();
    let session = MultipartUploadSession {
        upload_id: Uuid::now_v7(),
        file_id: Uuid::now_v7(), // never created
        version_id: Uuid::now_v7(),
        backend_upload_handle: "nonexistent-handle".to_owned(),
        state: MultipartUploadState::InProgress,
        declared_mime: "application/octet-stream".to_owned(),
        mime_validated: false,
        declared_size: 0,
        part_size: 0,
        auto_bind: false,
        lease_until: None,
        complete_result: None,
        created_at: now,
        expires_at: now,
    };

    let reclaimed = engine.cleanup_expired_session_version(&session).await;
    assert_eq!(
        reclaimed, 0,
        "a session pointing at a file that never existed must reclaim nothing, not error"
    );
}

/// A `list_expired_multipart_uploads` failure must abort step 2 of the sweep
/// entirely (logged, `0` aborted) rather than panicking or partially
/// processing -- a genuinely expired session is left untouched for the next
/// sweep to retry.
#[tokio::test]
async fn sweep_skips_expired_multipart_step_when_listing_fails() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = cleanup_ctx(tenant);
    let ticket = svc
        .create_file(&ctx, new_file(owner, OwnerKind::User), None, false)
        .await
        .expect("create_file");

    let upload_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let past = now - time::Duration::hours(1);
    store
        .create_multipart_upload(
            upload_id,
            ticket.file_id,
            ticket.version_id,
            "fake-backend-handle",
            "application/octet-stream",
            0u64,
            0u64,
            false,
            past, // already expired
            now,
        )
        .await
        .expect("create backdated multipart session");

    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyCleanupStore {
        inner: store.clone(),
        faults: CleanupFaults {
            fault_list_expired_multipart: true,
            ..Default::default()
        },
    });
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 3600,
        },
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 0,
        "a listing failure must abort step 2 with zero aborted sessions"
    );

    let session = store
        .get_multipart_upload(upload_id)
        .await
        .expect("get_multipart_upload")
        .expect("session row must still exist");
    assert_eq!(
        session.state,
        MultipartUploadState::InProgress,
        "the expired session must be left untouched when the listing step itself fails"
    );
}

/// When `abort_multipart_upload`'s CAS loses a race (a concurrent
/// complete/abort already moved the session out of `in_progress`), the
/// sweep must leave the session completely alone -- no version cleanup, no
/// orphan-file reclaim, no count.
#[tokio::test]
async fn sweep_leaves_session_untouched_when_abort_cas_loses_race() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = cleanup_ctx(tenant);
    let ticket = svc
        .create_file(&ctx, new_file(owner, OwnerKind::User), None, false)
        .await
        .expect("create_file");

    let upload_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let past = now - time::Duration::hours(1);
    store
        .create_multipart_upload(
            upload_id,
            ticket.file_id,
            ticket.version_id,
            "fake-backend-handle",
            "application/octet-stream",
            0u64,
            0u64,
            false,
            past,
            now,
        )
        .await
        .expect("create backdated multipart session");

    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyCleanupStore {
        inner: store.clone(),
        faults: CleanupFaults {
            force_abort_false_for: Some(upload_id),
            ..Default::default()
        },
    });
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 3600,
        },
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 0,
        "a lost CAS must not count as an aborted session"
    );

    let session = store
        .get_multipart_upload(upload_id)
        .await
        .expect("get_multipart_upload")
        .expect("session row must still exist");
    assert_eq!(
        session.state,
        MultipartUploadState::InProgress,
        "a lost CAS must leave the session's DB state completely untouched"
    );
}

/// When `abort_multipart_upload` itself errors, the sweep must log and
/// continue rather than propagate -- the session is left for a later sweep.
#[tokio::test]
async fn sweep_leaves_session_untouched_when_abort_errors() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = cleanup_ctx(tenant);
    let ticket = svc
        .create_file(&ctx, new_file(owner, OwnerKind::User), None, false)
        .await
        .expect("create_file");

    let upload_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let past = now - time::Duration::hours(1);
    store
        .create_multipart_upload(
            upload_id,
            ticket.file_id,
            ticket.version_id,
            "fake-backend-handle",
            "application/octet-stream",
            0u64,
            0u64,
            false,
            past,
            now,
        )
        .await
        .expect("create backdated multipart session");

    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyCleanupStore {
        inner: store.clone(),
        faults: CleanupFaults {
            force_abort_err_for: Some(upload_id),
            ..Default::default()
        },
    });
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 3600,
        },
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 0,
        "an abort error must not count as an aborted session"
    );

    let session = store
        .get_multipart_upload(upload_id)
        .await
        .expect("get_multipart_upload")
        .expect("session row must still exist");
    assert_eq!(
        session.state,
        MultipartUploadState::InProgress,
        "an abort error must leave the session's DB state completely untouched"
    );
}

/// A `list_metadata_for_files` failure during the retention sweep must skip
/// (never expire) every file whose applicable rules have a metadata
/// criterion -- expiring on unreadable metadata would be a silent,
/// unrecoverable data-loss bug.
#[tokio::test]
async fn sweep_retention_expiry_skips_files_when_metadata_batch_fetch_fails() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = cleanup_ctx(tenant);
    let ticket = svc
        .create_file(&ctx, new_file(owner, OwnerKind::User), None, false)
        .await
        .expect("create_file");

    // A tenant-wide metadata-criterion rule -- inserted directly (bypassing
    // `PolicyService`'s validation) to isolate the sweep's own behaviour.
    store
        .insert_retention_rule(
            &AccessScope::allow_all(),
            tenant,
            &RetentionScope::Tenant,
            None,
            &RetentionRuleBody {
                age: None,
                inactivity: None,
                metadata: Some(MetadataRetention {
                    key: "purge".to_owned(),
                    value: "yes".to_owned(),
                }),
            },
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("insert metadata-criterion retention rule");

    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyCleanupStore {
        inner: store.clone(),
        faults: CleanupFaults {
            fault_list_metadata_for_files: true,
            ..Default::default()
        },
    });
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 3600,
        },
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.retention_expired_deleted, 0,
        "a file whose applicable rule needs metadata we could not read must not be expired"
    );

    let file = store
        .get_file(&AccessScope::allow_all(), ticket.file_id)
        .await
        .expect("get_file via the real, unfaulted store");
    assert!(
        file.is_some(),
        "the file must survive the sweep when its metadata could not be fetched"
    );
}

/// Companion positive-path test: when the metadata batch fetch succeeds, a
/// file whose custom metadata actually satisfies the rule's key/value IS
/// expired -- proving the prefetched-map lookup (the `Some` arm the fault
/// test above never reaches) is wired correctly end to end.
#[tokio::test]
async fn sweep_retention_expiry_deletes_file_matching_metadata_rule() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = cleanup_ctx(tenant);
    let mut nf = new_file(owner, OwnerKind::User);
    nf.custom_metadata = vec![CustomMetadataEntry {
        key: "purge".to_owned(),
        value: "yes".to_owned(),
    }];
    let ticket = svc
        .create_file(&ctx, nf, None, false)
        .await
        .expect("create_file with matching custom metadata");

    store
        .insert_retention_rule(
            &AccessScope::allow_all(),
            tenant,
            &RetentionScope::Tenant,
            None,
            &RetentionRuleBody {
                age: None,
                inactivity: None,
                metadata: Some(MetadataRetention {
                    key: "purge".to_owned(),
                    value: "yes".to_owned(),
                }),
            },
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("insert metadata-criterion retention rule");

    let cleanup_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let engine = CleanupEngine::new(
        cleanup_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 3600,
        },
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.retention_expired_deleted, 1,
        "a file whose custom metadata matches the rule's key/value must be expired"
    );

    let file = store
        .get_file(&AccessScope::allow_all(), ticket.file_id)
        .await
        .expect("get_file");
    assert!(
        file.is_none(),
        "the matching file must be gone after the sweep"
    );
}

// =============================================================================
// domain/service/create.rs -- idempotency-replay error paths and multipart-
// initiate compensation
// =============================================================================

/// `create_file_bare` must collect and validate every custom-metadata entry,
/// not just skip the check when at least one is present.
#[tokio::test]
async fn create_file_bare_persists_multiple_custom_metadata_entries() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant, owner);
    let mut nf = new_file(owner, OwnerKind::User);
    nf.custom_metadata = vec![
        CustomMetadataEntry {
            key: "k1".to_owned(),
            value: "v1".to_owned(),
        },
        CustomMetadataEntry {
            key: "k2".to_owned(),
            value: "v2".to_owned(),
        },
    ];

    let file_id = svc
        .create_file_bare(&ctx, nf)
        .await
        .expect("create_file_bare with multiple metadata entries must succeed");

    let (_file, meta) = svc
        .get_file_with_metadata(&ctx, file_id)
        .await
        .expect("get_file_with_metadata");
    assert_eq!(
        meta.len(),
        2,
        "both custom-metadata entries must be persisted"
    );
    assert!(meta.iter().any(|e| e.key == "k1" && e.value == "v1"));
    assert!(meta.iter().any(|e| e.key == "k2" && e.value == "v2"));
}

/// A stored idempotency ticket whose `response_body` cannot be deserialized
/// (e.g. written by an incompatible schema version) must surface as a
/// `Database` error on replay, not panic.
#[tokio::test]
async fn create_file_idempotent_replay_with_corrupted_stored_body_is_database_error() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant, owner);
    let key = "corrupt-body-key".to_owned();

    svc.create_file(
        &ctx,
        new_file(owner, OwnerKind::User),
        Some(key.clone()),
        false,
    )
    .await
    .expect("initial create must succeed");

    {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureUpdateExt;

        use file_storage::infra::storage::entity::idempotency_key::{
            Column as IdemCol, Entity as IdemEntity,
        };

        let conn = db.conn().expect("conn");
        IdemEntity::update_many()
            .col_expr(IdemCol::ResponseBody, Expr::value("not-json"))
            .filter(IdemCol::TenantId.eq(tenant))
            .filter(IdemCol::OwnerId.eq(owner))
            .filter(IdemCol::IdempotencyKey.eq(key.clone()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .expect("corrupt the stored idempotency response body directly");
    }

    let replay = svc
        .create_file(&ctx, new_file(owner, OwnerKind::User), Some(key), false)
        .await;
    assert!(
        matches!(replay, Err(DomainError::Database { .. })),
        "expected a Database error replaying a corrupted stored ticket, got {replay:?}"
    );
}

/// If the version row a stored idempotency ticket points at has since been
/// removed, replay must surface `VersionNotFound` rather than panicking on
/// a missing row.
#[tokio::test]
async fn create_file_idempotent_replay_after_version_row_removed_is_version_not_found() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant, owner);
    let key = "missing-version-key".to_owned();

    let ticket = svc
        .create_file(
            &ctx,
            new_file(owner, OwnerKind::User),
            Some(key.clone()),
            false,
        )
        .await
        .expect("initial create must succeed");

    {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureDeleteExt;

        use file_storage::infra::storage::entity::file_version::{
            Column as VerCol, Entity as VerEntity,
        };

        let conn = db.conn().expect("conn");
        VerEntity::delete_many()
            .filter(VerCol::VersionId.eq(ticket.version_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .expect("delete the pending version row directly");
    }

    let replay = svc
        .create_file(&ctx, new_file(owner, OwnerKind::User), Some(key), false)
        .await;
    match replay {
        Err(DomainError::VersionNotFound {
            file_id,
            version_id,
        }) => {
            assert_eq!(file_id, ticket.file_id);
            assert_eq!(version_id, ticket.version_id);
        }
        other => panic!("expected VersionNotFound, got {other:?}"),
    }
}

/// Compensating a multipart-initiate failure for a `file_id` that is
/// already gone (e.g. a concurrent sweep beat this call to it) must be a
/// silent no-op, never a panic or an error surfaced to the caller.
#[tokio::test]
async fn compensate_failed_multipart_initiate_missing_file_is_noop() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let ctx = ctx(Uuid::now_v7(), Uuid::now_v7());
    let phantom_file_id = Uuid::now_v7();

    // Must return (this is a () fn) without panicking.
    svc.compensate_failed_multipart_initiate(&ctx, phantom_file_id)
        .await;

    let audit_rows = store.list_audit(phantom_file_id).await.expect("list_audit");
    assert!(
        audit_rows.is_empty(),
        "compensating a file that never existed must not write any audit row"
    );
}

/// If the file already has a version by the time compensation runs (the
/// guard's declared-unreachable-in-practice branch), the guard must decline
/// rather than delete the file out from under whatever gave it that
/// version.
#[tokio::test]
async fn compensate_failed_multipart_initiate_with_existing_version_is_noop() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant, owner);

    let file_id = svc
        .create_file_bare(&ctx, new_file(owner, OwnerKind::User))
        .await
        .expect("create_file_bare");
    svc.presign_version(&ctx, file_id)
        .await
        .expect("presign_version registers a pending version");

    svc.compensate_failed_multipart_initiate(&ctx, file_id)
        .await;

    let file = store
        .get_file(&AccessScope::allow_all(), file_id)
        .await
        .expect("get_file")
        .expect("the file must still exist -- the orphan guard must have declined");
    let versions = store.list_versions(file_id).await.expect("list_versions");
    assert_eq!(
        versions.len(),
        1,
        "the version added by presign_version must remain untouched"
    );
    let _ = file;
}

/// Cross-owner `create_file_bare` requires `ADMIN_POLICY`, mirroring
/// `create_file`'s guard: denied without it, allowed with it.
#[tokio::test]
async fn create_file_bare_cross_owner_requires_admin_policy() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authz = Arc::new(TestAuthorizer::new());
    let authorizer: Arc<dyn Authorizer> = Arc::clone(&authz) as Arc<dyn Authorizer>;
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let user_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);

    let denied = svc
        .create_file_bare(&ctx_a, new_file(user_b, OwnerKind::User))
        .await;
    assert!(
        matches!(denied, Err(DomainError::Forbidden)),
        "cross-owner create_file_bare without ADMIN_POLICY must be denied, got {denied:?}"
    );

    authz.set_admin(true);
    let allowed = svc
        .create_file_bare(&ctx_a, new_file(user_b, OwnerKind::User))
        .await;
    assert!(
        allowed.is_ok(),
        "cross-owner create_file_bare WITH ADMIN_POLICY must succeed, got {allowed:?}"
    );
}

// =============================================================================
// domain/multipart.rs -- small pure branches
// =============================================================================

#[test]
fn multipart_upload_state_completing_as_str_is_stable_wire_spelling() {
    assert_eq!(MultipartUploadState::Completing.as_str(), "completing");
}

#[test]
#[should_panic(expected = "completion lease held elsewhere")]
fn unwrap_completed_panics_while_completion_lease_is_held() {
    let outcome = MultipartCompleteOutcome::Completing {
        retry_after_secs: 5,
    };
    // The call itself is the assertion: this must panic, not return.
    drop(outcome.unwrap_completed());
}

#[test]
fn stored_complete_result_with_unknown_bind_state_fails_to_rehydrate() {
    let stored = StoredCompleteResult {
        version_id: Uuid::now_v7(),
        size: 10,
        content_hash: "00".repeat(32),
        hash_mode: "whole-sha256".to_owned(),
        part_count: 1,
        manifest: None,
        bind_state: "not-a-real-state".to_owned(),
        etag: None,
        current_etag: None,
    };
    assert!(
        stored.into_completed().is_none(),
        "an unrecognized bind_state must fail to rehydrate rather than silently default"
    );
}

// =============================================================================
// domain/service/read_ops.rs + api/rest/dto.rs -- manifests_for_versions /
// VersionDto
// =============================================================================

/// The bare `From<FileVersion>` conversion (used outside the batched
/// `list_versions` path) must never attach a manifest.
#[test]
fn version_dto_from_file_version_omits_manifest() {
    let v = FileVersion {
        file_id: Uuid::now_v7(),
        version_id: Uuid::now_v7(),
        mime_type: "text/plain".to_owned(),
        size: 42,
        hash_algorithm: "sha256".to_owned(),
        hash_value: vec![1, 2, 3],
        hash_mode: "whole-sha256".to_owned(),
        part_count: None,
        status: VersionStatus::Available,
        is_current: true,
        backend_id: "mem".to_owned(),
        backend_path: "/a/b".to_owned(),
        created_at: OffsetDateTime::now_utc(),
    };
    let dto: VersionDto = v.clone().into();
    assert_eq!(dto.version_id, v.version_id);
    assert_eq!(dto.status, v.status.as_str());
    assert!(
        dto.manifest.is_none(),
        "the bare `From` conversion must never attach a manifest"
    );
}

/// `GET /files/{id}/versions` end to end: exercises
/// `FileService::manifests_for_versions` (the batched manifest lookup,
/// `pub(crate)` so only reachable through this handler from outside the
/// crate) and `VersionDto::from_parts` wiring a real version through the
/// handler.
#[tokio::test]
async fn list_versions_endpoint_batches_manifest_lookup_and_serializes_versions() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let subject_ctx = ctx(tenant, owner);

    let ticket = svc
        .create_file(&subject_ctx, new_file(owner, OwnerKind::User), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &subject_ctx,
        ticket.file_id,
        ticket.version_id,
        "application/octet-stream",
        Bytes::from_static(b"hello"),
    )
    .await
    .expect("put_content");
    svc.bind(&subject_ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind");

    let router = Router::new()
        .route(
            &format!("{BASE}/files/{{id}}/versions"),
            get(handlers::list_versions),
        )
        .layer(axum::Extension(subject_ctx.clone()))
        .layer(axum::Extension(Arc::clone(&svc)));

    let resp = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("{BASE}/files/{}/versions", ticket.file_id))
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON body");
    let items = body.as_array().expect("array body");
    assert_eq!(items.len(), 1, "expected exactly the one bound version");
    let version_id_str = ticket.version_id.to_string();
    assert_eq!(items[0]["version_id"], version_id_str.as_str());
    assert!(
        items[0].get("manifest").is_none() || items[0]["manifest"].is_null(),
        "a whole-sha256 version must carry no manifest"
    );
}
