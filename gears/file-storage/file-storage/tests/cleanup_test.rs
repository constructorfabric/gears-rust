#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::MultipartUploadSession;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{
    AgeRetention, RetentionRuleBody, RetentionScope, StoredRetentionRule,
};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, DataPlanePort, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.cleanup_test.file.type.v1~");

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-cleanup-test-{}.db", Uuid::now_v7().simple()));
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

/// `grace_secs = 0` makes every pending version immediately eligible for the sweep.
async fn build_all(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    Arc<PolicyService>,
    Arc<MultipartService>,
    DataPlaneService,
    Store,
    CleanupEngine,
    Arc<dyn StorageBackend>,
) {
    let db = build_db().await;

    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));

    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let sweep_backends = backends.clone();

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        Arc::clone(&authorizer),
        None,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let psvc = Arc::new(PolicyService::new(policy_store, authorizer));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, psvc, msvc, dp, store, engine, backend)
}

/// Like `build_all` but also returns the `DBProvider`, to backdate `created_at`/`expires_at`
/// (there is no public API for that).
async fn build_all_with_db(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Store,
    CleanupEngine,
    Arc<DBProvider<DbError>>,
) {
    let db = build_db().await;

    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));

    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let sweep_backends = backends.clone();

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        Arc::clone(&authorizer),
        None,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, msvc, store, engine, db)
}

async fn build_all_dual_backend(
    grace_secs: u64,
) -> (Arc<FileService>, DataPlaneService, Store, CleanupEngine) {
    let db = build_db().await;

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_backend)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer = Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let sweep_backends = backends.clone();

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, dp, store, engine)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file() -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "test.txt".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "text/plain".to_owned(),
        custom_metadata: vec![],
    }
}

/// `CleanupStore` wrapper whose `list_versions` fails for one `file_id`; every other method
/// delegates to a real `Store`.
struct FaultyListVersionsStore {
    inner: Store,
    fault_file_id: Uuid,
}

#[async_trait]
impl CleanupStore for FaultyListVersionsStore {
    async fn list_abandoned_pending_versions(
        &self,
        older_than: time::OffsetDateTime,
        now: time::OffsetDateTime,
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
        now: time::OffsetDateTime,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        self.inner.list_expired_multipart_uploads(now).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
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

    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        if file_id == self.fault_file_id {
            Err(DomainError::InternalError)
        } else {
            self.inner.list_versions(file_id).await
        }
    }

    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError> {
        self.inner
            .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
            .await
    }

    async fn has_in_progress_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        self.inner.has_in_progress_multipart_for_file(file_id).await
    }

    async fn delete_file_with_event(
        &self,
        scope: &toolkit_security::AccessScope,
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
        now: time::OffsetDateTime,
    ) -> Result<u64, DomainError> {
        self.inner.delete_expired_idempotency_keys(now).await
    }
}

#[tokio::test]
async fn abandoned_pending_version_is_deleted_by_sweep() {
    let (svc, _psvc, _msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let before = store.list_versions(ticket.file_id).await.unwrap();
    assert_eq!(
        before.len(),
        1,
        "should have 1 pending version before sweep"
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "sweep should have deleted exactly 1 pending version"
    );

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap();
    assert!(
        after.is_none(),
        "pending version row should be deleted after sweep"
    );

    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let reconcile_count = audit
        .iter()
        .filter(|r| r.operation == "orphan_reconcile")
        .count();
    assert!(
        reconcile_count >= 1,
        "expected at least 1 orphan_reconcile audit row"
    );
}

#[tokio::test]
async fn recent_pending_version_is_not_swept_within_grace_window() {
    let (svc, _psvc, _msvc, _dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "recent pending version must not be swept"
    );

    let v = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap();
    assert!(
        v.is_some(),
        "pending version must still exist after grace-protected sweep"
    );
}

/// Sweeping the file's only pending version also deletes the now-orphaned parent `files` row
/// and enqueues `file.deleted`.
#[tokio::test]
async fn sweep_deletes_abandoned_zero_version_file() {
    let (svc, _psvc, _msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "sweep should have deleted the abandoned pending version"
    );
    assert_eq!(
        result.abandoned_files_deleted, 1,
        "sweep should also have deleted the now-orphaned parent file row"
    );

    let version_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_none(),
        "pending version row should be deleted after sweep"
    );

    let file_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        file_after.is_none(),
        "orphaned zero-version file row must be deleted by the sweep"
    );

    let events = store.list_file_events(ticket.file_id).await.unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "file.deleted"),
        "expected a file.deleted event for the orphan-reconciled file"
    );
}

/// Negative control: a file that still has a bound `Available` version keeps its parent row.
#[tokio::test]
async fn sweep_keeps_file_with_other_versions() {
    let (svc, _psvc, _msvc, dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let v1 = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let v2 = svc.presign_version(&ctx, v1.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        v1.file_id,
        v2.version_id,
        "text/plain",
        Bytes::from_static(b"real content"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, v1.file_id, v2.version_id, None)
        .await
        .unwrap();

    let before = store.list_versions(v1.file_id).await.unwrap();
    assert_eq!(before.len(), 2, "file should have 2 versions before sweep");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "sweep should reclaim the one abandoned pending version (v1)"
    );
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "the file must NOT be deleted -- it still has a real, bound version"
    );

    let v1_after = store.get_version(v1.file_id, v1.version_id).await.unwrap();
    assert!(
        v1_after.is_none(),
        "the abandoned pending version must still be reclaimed"
    );

    let file_after = svc.get_file(&ctx, v1.file_id).await.unwrap();
    assert_eq!(file_after.content_id, Some(v2.version_id));
    let v2_after = store
        .get_version(v1.file_id, v2.version_id)
        .await
        .unwrap()
        .expect("bound version must survive the sweep");
    assert_eq!(v2_after.status, VersionStatus::Available);
}

/// Expired sessions are inserted directly with a past `expires_at`; no public API can expire one.
#[tokio::test]
async fn expired_multipart_session_is_aborted_by_sweep() {
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None)
        .await
        .unwrap();

    let not_expired = store
        .list_expired_multipart_uploads(time::OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert!(
        not_expired.is_empty(),
        "session with future expires_at must not appear in expired list"
    );

    let upload_id2 = Uuid::now_v7();
    let file_id2 = ticket.file_id;
    let version_id2 = Uuid::now_v7();
    let past_time = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let now_t = time::OffsetDateTime::now_utc();

    store
        .insert_pending_version(
            file_id2,
            version_id2,
            "text/plain",
            "mem",
            &format!("/{file_id2}/{version_id2}"),
            now_t,
        )
        .await
        .unwrap();

    store
        .create_multipart_upload(
            upload_id2,
            file_id2,
            version_id2,
            "fake-backend-handle",
            "text/plain",
            0u64,      // declared_size (not relevant for sweep test)
            0u64,      // part_size (not relevant for sweep test)
            past_time, // expires in the past
            now_t,
        )
        .await
        .unwrap();

    let expired = store
        .list_expired_multipart_uploads(time::OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert!(
        expired.iter().any(|s| s.upload_id == upload_id2),
        "backdated session must appear in expired list"
    );

    let result = engine.run_sweep().await;
    assert!(
        result.expired_multipart_aborted >= 1,
        "sweep must report at least 1 aborted multipart session"
    );

    let original_session = store
        .get_multipart_upload(session.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        original_session.state,
        file_storage::domain::multipart::MultipartUploadState::InProgress,
        "non-expired session must still be in_progress"
    );

    let aborted_session = store
        .get_multipart_upload(upload_id2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        aborted_session.state,
        file_storage::domain::multipart::MultipartUploadState::Aborted,
        "backdated session must be aborted after sweep"
    );
}

/// A pending version backing a live `in_progress` session is never reclaimed, however old,
/// or a long-running upload would lose its version before `complete`.
#[tokio::test]
async fn sweep_skips_pending_version_of_active_multipart_session() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };

    // grace = 1 hour: only the backdated multipart-session version is a sweep candidate.
    let (svc, msvc, store, engine, db) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None)
        .await
        .unwrap();

    // Backdate the backing version past the grace cutoff; `expires_at` stays in the future.
    let conn = db.conn().expect("conn");
    let backdated = time::OffsetDateTime::now_utc() - time::Duration::hours(2);
    FileVersionEntity::update_many()
        .col_expr(FileVersionColumn::CreatedAt, Expr::value(backdated))
        .filter(FileVersionColumn::VersionId.eq(plan.version_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "a pending version backing a live multipart session must not be reclaimed"
    );

    let version_after = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_some(),
        "the multipart session's backing version must survive the sweep"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must still exist");
    assert_eq!(
        session_after.state,
        file_storage::domain::multipart::MultipartUploadState::InProgress,
        "the live session must survive the sweep untouched"
    );
}

/// Companion: once `expires_at` has also passed, the sweep aborts the session and reclaims it.
#[tokio::test]
async fn sweep_reclaims_version_after_session_expires() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };
    use file_storage::infra::storage::entity::multipart_upload::{
        Column as MultipartUploadColumn, Entity as MultipartUploadEntity,
    };

    let (svc, msvc, store, engine, db) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None)
        .await
        .unwrap();

    let conn = db.conn().expect("conn");
    let now = time::OffsetDateTime::now_utc();

    let backdated_created = now - time::Duration::hours(2);
    FileVersionEntity::update_many()
        .col_expr(FileVersionColumn::CreatedAt, Expr::value(backdated_created))
        .filter(FileVersionColumn::VersionId.eq(plan.version_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");

    let backdated_expiry = now - time::Duration::seconds(10);
    MultipartUploadEntity::update_many()
        .col_expr(
            MultipartUploadColumn::ExpiresAt,
            Expr::value(backdated_expiry),
        )
        .filter(MultipartUploadColumn::UploadId.eq(plan.upload_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate session expires_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 1,
        "the now-expired session must be aborted"
    );
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "the version must be reclaimed once its session is no longer live"
    );

    let version_after = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_none(),
        "the pending version must be gone once the multipart session is no longer live"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("the session row itself is aborted, not deleted");
    assert_eq!(
        session_after.state,
        file_storage::domain::multipart::MultipartUploadState::Aborted,
        "the session must be aborted once its expiry has passed"
    );
}

/// The zero-age rule is inserted through the store, bypassing `create_retention_rule`'s validation,
/// so only the sweep matcher is exercised.
#[tokio::test]
async fn retention_expired_file_is_deleted_by_sweep() {
    let (svc, _psvc, _msvc, dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"retention test"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    store
        .insert_retention_rule(
            &toolkit_security::AccessScope::allow_all(),
            tenant,
            &RetentionScope::Tenant,
            None,
            &RetentionRuleBody {
                age: Some(AgeRetention { max_age_days: 0 }),
                inactivity: None,
                metadata: None,
            },
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let before = store.list_all_files_for_sweep(None, 1000).await.unwrap();
    assert!(
        before.iter().any(|f| f.file_id == ticket.file_id),
        "file must be present before sweep"
    );

    let result = engine.run_sweep().await;
    assert!(
        result.retention_expired_deleted >= 1,
        "sweep must delete at least 1 retention-expired file"
    );

    let after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        after.is_none(),
        "file must be deleted after retention sweep"
    );

    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let ret_del: Vec<_> = audit
        .iter()
        .filter(|r| r.operation == "retention_delete")
        .collect();
    assert!(
        !ret_del.is_empty(),
        "expected at least 1 retention_delete audit row"
    );
    assert_eq!(ret_del[0].outcome, "success");
}

/// `create_retention_rule` rejects `max_age_days = 0`, so such a rule can never reach the sweep.
#[tokio::test]
async fn sweep_does_not_run_zero_age_rule() {
    let (svc, psvc, _msvc, dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let result = psvc
        .create_retention_rule(
            &ctx,
            RetentionScope::Tenant,
            None,
            RetentionRuleBody {
                age: Some(AgeRetention { max_age_days: 0 }),
                inactivity: None,
                metadata: None,
            },
        )
        .await;
    assert!(
        matches!(
            result,
            Err(file_storage::domain::error::DomainError::Validation { .. })
        ),
        "expected Validation, got {result:?}"
    );

    let rules = store
        .list_retention_rules(&toolkit_security::AccessScope::allow_all(), tenant)
        .await
        .unwrap();
    assert!(
        rules.is_empty(),
        "no retention rule row should exist after a rejected create"
    );

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"must survive"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.retention_expired_deleted, 0,
        "no rule exists, so nothing should be retention-deleted"
    );

    let after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        after.is_some(),
        "file must survive the sweep since the dangerous rule was never created"
    );
}

/// A `list_versions` failure aborts that file's expiry instead of being treated as zero versions;
/// an unrelated matching file is still deleted in the same sweep.
#[tokio::test]
async fn expire_file_list_versions_error_does_not_delete_file() {
    let (svc, _psvc, _msvc, dp, store, _engine, backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let faulted = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        faulted.file_id,
        faulted.version_id,
        "text/plain",
        Bytes::from_static(b"faulted"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, faulted.file_id, faulted.version_id, None)
        .await
        .unwrap();

    let healthy = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        healthy.file_id,
        healthy.version_id,
        "text/plain",
        Bytes::from_static(b"healthy"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, healthy.file_id, healthy.version_id, None)
        .await
        .unwrap();

    store
        .insert_retention_rule(
            &toolkit_security::AccessScope::allow_all(),
            tenant,
            &RetentionScope::Tenant,
            None,
            &RetentionRuleBody {
                age: Some(AgeRetention { max_age_days: 0 }),
                inactivity: None,
                metadata: None,
            },
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyListVersionsStore {
        inner: store.clone(),
        fault_file_id: faulted.file_id,
    });
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 86400,
        },
    );

    let result = engine.run_sweep().await;

    assert_eq!(
        result.retention_expired_deleted, 1,
        "only the unrelated healthy file should count as retention-expired-deleted"
    );

    let faulted_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), faulted.file_id)
        .await
        .unwrap();
    assert!(
        faulted_after.is_some(),
        "file with a faulted list_versions call must survive the sweep"
    );

    let healthy_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), healthy.file_id)
        .await
        .unwrap();
    assert!(
        healthy_after.is_none(),
        "unrelated matching file must still be deleted by the same sweep"
    );
}

#[tokio::test]
async fn file_without_matching_retention_rule_is_not_deleted() {
    let (svc, _psvc, _msvc, dp, _store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"should not be deleted"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.retention_expired_deleted, 0,
        "file without a matching rule must not be deleted"
    );

    let file = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert_eq!(file.file_id, ticket.file_id);
}

#[tokio::test]
async fn migrate_backend_moves_content_and_updates_version_row() {
    let (svc, dp, store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"migrate me"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let v_before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v_before.backend_id, "mem");

    svc.migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap();

    let v_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        v_after.backend_id, "alt",
        "version must now point to alt backend"
    );

    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let migrate_rows: Vec<_> = audit
        .iter()
        .filter(|r| r.operation == "backend_migrate")
        .collect();
    assert!(
        !migrate_rows.is_empty(),
        "expected at least 1 backend_migrate audit row"
    );
    assert_eq!(migrate_rows[0].outcome, "success");
}

#[tokio::test]
async fn migrate_backend_to_same_backend_is_noop() {
    let (svc, dp, store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"same backend"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    svc.migrate_backend(&ctx, ticket.file_id, "mem")
        .await
        .unwrap();

    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let migrate_count = audit
        .iter()
        .filter(|r| r.operation == "backend_migrate")
        .count();
    assert_eq!(
        migrate_count, 0,
        "no-op migration must not write an audit row"
    );
}

/// Files with more than one version cannot be migrated.
#[tokio::test]
async fn migrate_backend_rejects_versioned_file() {
    use file_storage::domain::error::DomainError;

    let (svc, dp, _store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let t2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        t2.version_id,
        "text/plain",
        Bytes::from_static(b"v2"),
    )
    .await
    .unwrap();

    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::VersionedFileMigrationNotSupported { .. }),
        "expected VersionedFileMigrationNotSupported, got {err:?}"
    );
}

/// Grants everything except `ADMIN_POLICY`, which requires `set_admin(true)`.
#[derive(Default)]
struct ScopedTestAuthorizer {
    is_admin: std::sync::atomic::AtomicBool,
}

impl ScopedTestAuthorizer {
    fn set_admin(&self, admin: bool) {
        self.is_admin
            .store(admin, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl file_storage::domain::authz::Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<toolkit_security::AccessScope, DomainError> {
        if action == file_storage::domain::authz::actions::ADMIN_POLICY
            && !self.is_admin.load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(DomainError::Forbidden);
        }
        Ok(toolkit_security::AccessScope::for_tenant(
            ctx.subject_tenant_id(),
        ))
    }
}

async fn build_all_dual_backend_scoped(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    DataPlaneService,
    Store,
    Arc<ScopedTestAuthorizer>,
) {
    let db = build_db().await;

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(vec![mem_backend, alt_backend], "mem").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer = Arc::new(ScopedTestAuthorizer::default());
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends,
        issuer,
        Arc::clone(&authorizer) as Arc<dyn file_storage::domain::authz::Authorizer>,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    // `grace_secs` is unused here; kept for signature symmetry with the other `build_all*` helpers.
    let _ = grace_secs;
    (svc, dp, store, authorizer)
}

#[tokio::test]
async fn migrate_backend_rejects_non_durable_target_for_non_admin() {
    let (svc, dp, store, authorizer) = build_all_dual_backend_scoped(86400).await;
    authorizer.set_admin(false);
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"non-admin migrate attempt"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Forbidden),
        "expected Forbidden, got {err:?}"
    );

    let v_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        v_after.backend_id, "mem",
        "version must stay on the source backend after a rejected migration"
    );
}

#[tokio::test]
async fn migrate_backend_allows_non_durable_target_for_admin_scope() {
    let (svc, dp, store, authorizer) = build_all_dual_backend_scoped(86400).await;
    authorizer.set_admin(true);
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"admin migrate attempt"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    svc.migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap();

    let v_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        v_after.backend_id, "alt",
        "admin-scoped caller must be able to migrate onto a non-durable target"
    );
}

// Sweep-vs-complete races are tested as deterministic call orderings, never with sleeps or
// real concurrency.
/// Drives a single-part upload to a bound `Available` version; returns `(upload_id, version_id)`.
async fn complete_one_part_multipart_upload(
    msvc: &MultipartService,
    svc: &FileService,
    store: &Store,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
    file_id: Uuid,
    data: &'static [u8],
) -> (Uuid, Uuid) {
    let declared_size = data.len() as u64;
    let plan = msvc
        .initiate_multipart_upload(
            ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            None,
            None,
        )
        .await
        .unwrap();
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    let part = plan.parts.first().expect("single-part plan");

    let (backend_etag, part_hash) = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            part.part_number,
            part.offset,
            Bytes::from_static(data),
        )
        .await
        .expect("backend upload_part");
    store
        .upsert_multipart_part(
            plan.upload_id,
            i32::try_from(part.part_number).unwrap(),
            &backend_etag,
            part_hash,
            i64::try_from(part.size).unwrap(),
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    msvc.complete_multipart_upload(ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();
    svc.bind(ctx, file_id, plan.version_id, None).await.unwrap();

    (plan.upload_id, plan.version_id)
}

/// Completed first, then backdated: `complete_multipart_upload` rejects an expired `in_progress`
/// session, so the sweep's session CAS must lose against an already-`completed` row.
#[tokio::test]
async fn sweep_after_complete_wins_does_not_delete_bound_version() {
    let (svc, _psvc, msvc, _dp, store, engine, backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let (upload_id, version_id) = complete_one_part_multipart_upload(
        &msvc,
        &svc,
        &store,
        &backend,
        &ctx,
        ticket.file_id,
        b"Hello, World!",
    )
    .await;

    let before = store
        .get_version(ticket.file_id, version_id)
        .await
        .unwrap()
        .expect("version must exist after complete");
    assert_eq!(before.status, VersionStatus::Available);
    let file_before = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert_eq!(file_before.content_id, Some(version_id));

    store
        .set_multipart_expires_at_for_test(
            upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 0,
        "the sweep's session CAS must lose against the already-`completed` row"
    );

    let after = store
        .get_version(ticket.file_id, version_id)
        .await
        .unwrap()
        .expect("bound version must not be deleted by the sweep");
    assert_eq!(after.status, VersionStatus::Available);

    let file_after = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert_eq!(file_after.content_id, Some(version_id));
}

/// Reverse order: the sweep aborts the expired session and deletes the version first;
/// a later `complete` is rejected.
#[tokio::test]
async fn sweep_before_complete_wins_cleans_up_expired_session() {
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            None,
        )
        .await
        .unwrap();

    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 1,
        "sweep must win the session CAS and abort the expired session"
    );

    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version.is_none(),
        "pending version must be deleted once the sweep wins the session CAS"
    );

    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            file_storage::domain::error::DomainError::MultipartUploadNotInProgress { .. }
        ),
        "expected MultipartUploadNotInProgress after the sweep aborted the session, got {err:?}"
    );
}

/// Expired but still `in_progress` (no sweep ran): `complete_multipart_upload` itself must reject.
#[tokio::test]
async fn complete_after_session_expired_is_rejected() {
    let (svc, _psvc, msvc, _dp, store, _engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            None,
        )
        .await
        .unwrap();

    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            file_storage::domain::error::DomainError::MultipartUploadNotInProgress { .. }
        ),
        "expected MultipartUploadNotInProgress for an expired-but-still-in_progress \
         session, got {err:?}"
    );
}

/// Window where `finalize_version` already made the version `Available` but the session CAS has
/// not run: the sweep's status-guarded delete must match zero rows.
#[tokio::test]
async fn sweep_mid_flight_after_finalize_but_before_session_cas_does_not_delete_available_version()
{
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            None,
        )
        .await
        .unwrap();
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    assert_eq!(
        session.state,
        file_storage::domain::multipart::MultipartUploadState::InProgress,
        "session must still be in_progress at the moment cleanup is invoked"
    );

    let finalize_audit = file_storage::domain::audit::AuditEntry {
        tenant_id: Uuid::nil(),
        actor_kind: "system".to_owned(),
        actor_id: Uuid::nil(),
        file_id: Some(ticket.file_id),
        operation: file_storage::domain::audit::AuditOperation::FinalizeVersion,
        outcome: file_storage::domain::audit::AuditOutcome::Success,
        detail: serde_json::json!({ "test": "mid-flight simulation" }),
        occurred_at: time::OffsetDateTime::now_utc(),
    };
    let finalized = store
        .finalize_version(
            ticket.file_id,
            plan.version_id,
            5,
            vec![0u8; 32],
            file_storage::infra::content::hash_mode::HashMode::WholeSha256,
            None,
            None,
            None,
            finalize_audit,
        )
        .await
        .unwrap();
    assert!(
        finalized,
        "finalize_version must flip the pending version to available"
    );

    engine.cleanup_expired_session_version(&session).await;

    let after = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must not be deleted by the mid-flight cleanup");
    assert_eq!(after.status, VersionStatus::Available);
}

/// Rows are seeded directly; `idempotency_keys.file_id` has an FK, so they point at real files.
#[tokio::test]
async fn run_sweep_deletes_expired_idempotency_rows() {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::entity::idempotency_key;
    use file_storage::infra::storage::repo::IdempotencyRepo;
    use file_storage::infra::storage::store::IdempotencyInsert;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let store = Store::new(Arc::clone(&db));
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    // `orphan_grace_secs` 86400, not 0: zero would sweep these FK-only files, cascading the rows.
    let engine = CleanupEngine::new(
        sweep_store,
        backends.clone(),
        CleanupConfig {
            orphan_grace_secs: 86400,
        },
    );

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        ServiceConfig {
            default_url_ttl_secs: 3600,
            sidecar_base_url: "http://sidecar.test".to_owned(),
            default_page_size: 50,
            max_page_size: 1000,
            idempotency_ttl_secs: 86400,
        },
        None,
        None,
    );
    let tenant_id = Uuid::now_v7();
    let ctx = ctx(tenant_id);
    let expired_ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let live_ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let conn = db.conn().expect("conn");
    let repo = IdempotencyRepo::new();
    let subject_id = Uuid::now_v7();
    let now = time::OffsetDateTime::now_utc();

    repo.insert(
        &conn,
        &IdempotencyInsert {
            tenant_id,
            owner_kind: "user".to_owned(),
            owner_id: Uuid::now_v7(),
            key: "expired-key".to_owned(),
            subject_id,
            response_status: 201,
            response_body: "{}".to_owned(),
            response_etag: "etag-expired".to_owned(),
            request_hash: b"expired-hash".to_vec(),
            expires_at: now - time::Duration::hours(1),
        },
        expired_ticket.file_id,
        now - time::Duration::hours(2),
    )
    .await
    .expect("insert expired row");

    let live_owner_id = Uuid::now_v7();
    let live_file_id = live_ticket.file_id;
    repo.insert(
        &conn,
        &IdempotencyInsert {
            tenant_id,
            owner_kind: "user".to_owned(),
            owner_id: live_owner_id,
            key: "live-key".to_owned(),
            subject_id,
            response_status: 201,
            response_body: "{\"ok\":true}".to_owned(),
            response_etag: "etag-live".to_owned(),
            request_hash: b"live-hash".to_vec(),
            expires_at: now + time::Duration::days(1),
        },
        live_file_id,
        now,
    )
    .await
    .expect("insert live row");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.idempotency_keys_deleted, 1,
        "sweep should have deleted exactly the one expired idempotency row"
    );

    let rows = idempotency_key::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query idempotency_keys directly");
    assert_eq!(rows.len(), 1, "only the live row should remain");
    let remaining = &rows[0];
    assert_eq!(remaining.idempotency_key, "live-key");
    assert_eq!(remaining.owner_id, live_owner_id);
    assert_eq!(remaining.file_id, live_file_id);
    assert_eq!(remaining.subject_id, subject_id);
    assert_eq!(remaining.response_status, 201);
    assert_eq!(remaining.response_body, "{\"ok\":true}");
    assert_eq!(remaining.response_etag, "etag-live");
}

/// Unpublished outbox rows must survive regardless of age (`published_at` stays NULL until a relay
/// exists). Seeded directly: there is no API to backdate `occurred_at`.
#[tokio::test]
async fn run_sweep_does_not_touch_unpublished_outbox_rows() {
    use sea_orm::{EntityTrait, Set};
    use toolkit_db::secure::{SecureEntityExt, secure_insert};
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::entity::{audit_outbox, events_outbox};

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let store = Store::new(Arc::clone(&db));
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let engine = CleanupEngine::new(
        sweep_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 0,
        },
    );

    let conn = db.conn().expect("conn");
    let ancient = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();

    let audit_event_id = Uuid::now_v7();
    let audit_am = audit_outbox::ActiveModel {
        event_id: Set(audit_event_id),
        tenant_id: Set(tenant_id),
        actor_kind: Set("system".to_owned()),
        actor_id: Set(Uuid::nil()),
        file_id: Set(Some(file_id)),
        operation: Set("orphan_reconcile".to_owned()),
        outcome: Set("success".to_owned()),
        detail: Set(serde_json::json!({ "seed": "ancient" })),
        occurred_at: Set(ancient),
        published_at: Set(None),
    };
    secure_insert::<audit_outbox::Entity>(audit_am, &AccessScope::allow_all(), &conn)
        .await
        .expect("insert ancient audit_outbox row");

    let event_event_id = Uuid::now_v7();
    let events_am = events_outbox::ActiveModel {
        event_id: Set(event_event_id),
        tenant_id: Set(tenant_id),
        owner_id: Set(Uuid::now_v7()),
        file_id: Set(file_id),
        event_type: Set("file.deleted".to_owned()),
        payload: Set(serde_json::json!({ "seed": "ancient" })),
        occurred_at: Set(ancient),
        published_at: Set(None),
    };
    secure_insert::<events_outbox::Entity>(events_am, &AccessScope::allow_all(), &conn)
        .await
        .expect("insert ancient events_outbox row");

    engine.run_sweep().await;

    let audit_rows = audit_outbox::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query audit_outbox directly");
    assert!(
        audit_rows.iter().any(|r| r.event_id == audit_event_id),
        "unpublished audit_outbox row must survive the sweep regardless of age"
    );

    let event_rows = events_outbox::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query events_outbox directly");
    assert!(
        event_rows.iter().any(|r| r.event_id == event_event_id),
        "unpublished events_outbox row must survive the sweep regardless of age"
    );
}

/// Two racers share the same starting `(backend_id, backend_path)`; the CAS lets only one win.
#[tokio::test]
async fn concurrent_migrate_backend_second_racer_is_rejected() {
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::repo::VersionRepo;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(store.clone(), backends, issuer, authorizer, cfg, None, None);

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("pending version row must exist");
    assert_eq!(before.backend_id, "mem");

    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let repo = VersionRepo::new();

    let expected_backend_id = before.backend_id.clone();
    let expected_backend_path = before.backend_path.clone();

    let first = repo
        .rebind_backend(
            &conn,
            &scope,
            ticket.file_id,
            ticket.version_id,
            &expected_backend_id,
            &expected_backend_path,
            "alt",
            "/alt/racer-a",
        )
        .await
        .expect("first racer's CAS call");
    assert!(first, "first racer's CAS must win");

    let second = repo
        .rebind_backend(
            &conn,
            &scope,
            ticket.file_id,
            ticket.version_id,
            &expected_backend_id,
            &expected_backend_path,
            "other",
            "/other/racer-b",
        )
        .await
        .expect("second racer's CAS call");
    assert!(
        !second,
        "second racer's CAS must lose: the pointer already changed"
    );

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(
        after.backend_id, "alt",
        "version must reflect only the FIRST racer's target"
    );
    assert_eq!(after.backend_path, "/alt/racer-a");
}

type RaceIds = Arc<std::sync::Mutex<Option<(Uuid, Uuid, String, String)>>>;

/// Runs a one-shot hook before `put`, modelling a racing `migrate_backend` that commits between
/// this call's destination `put` and its CAS, deterministically and in-process.
struct RacingBackend {
    inner: Arc<dyn StorageBackend>,
    #[allow(clippy::type_complexity)]
    on_put: std::sync::Mutex<
        Option<Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send>>,
    >,
}

#[async_trait]
impl StorageBackend for RacingBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self) -> file_storage::infra::backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        let hook = self.on_put.lock().expect("on_put mutex").take();
        if let Some(hook) = hook {
            hook().await;
        }
        self.inner.put(path, bytes).await
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.inner.get(path).await
    }

    async fn get_range(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
    ) -> Result<Bytes, DomainError> {
        self.inner.get_range(path, range).await
    }

    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        self.inner.size(path).await
    }

    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }

    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
    }
}

/// Racer migrates to a different target mid-flight: our CAS loses and our destination blob is
/// cleaned up, since it is never the live pointer.
#[tokio::test]
async fn migrate_backend_loser_target_blob_cleaned_up() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt1_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt1"));
    let alt2_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt2"));

    let tenant = Uuid::now_v7();
    let content = Bytes::from_static(b"loser cleanup content");

    let ids_cell: RaceIds = Arc::new(std::sync::Mutex::new(None));

    let hook_ids_cell = Arc::clone(&ids_cell);
    let hook_store = store.clone();
    let hook_alt2 = Arc::clone(&alt2_backend);
    let hook_bytes = content.clone();
    let hook: Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send> =
        Box::new(move || {
            Box::pin(async move {
                let (file_id, version_id, orig_backend_id, orig_backend_path) = hook_ids_cell
                    .lock()
                    .expect("ids_cell mutex")
                    .clone()
                    .expect("ids must be set before migrate_backend runs");
                let dest_path = format!("/{file_id}/{version_id}");
                hook_alt2
                    .put(&dest_path, hook_bytes.clone())
                    .await
                    .expect("racer's own blob write");
                let audit = AuditEntry::success(
                    tenant,
                    "system",
                    Uuid::nil(),
                    Some(file_id),
                    AuditOperation::BackendMigrate,
                    serde_json::json!({ "racer": "concurrent-winner-different-target" }),
                );
                let won = hook_store
                    .rebind_version_backend(
                        file_id,
                        version_id,
                        &orig_backend_id,
                        &orig_backend_path,
                        "alt2",
                        &dest_path,
                        audit,
                    )
                    .await
                    .expect("racer's CAS call");
                assert!(
                    won,
                    "the injected racer's CAS must win: nothing else has touched the row yet"
                );
            })
        });

    let racing_alt1: Arc<dyn StorageBackend> = Arc::new(RacingBackend {
        inner: alt1_inner.clone(),
        on_put: std::sync::Mutex::new(Some(hook)),
    });

    let backends = BackendRegistry::new(
        vec![
            Arc::clone(&mem_backend),
            Arc::clone(&racing_alt1),
            Arc::clone(&alt2_backend),
        ],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    let ctx = ctx(tenant);
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must exist before migration");
    assert_eq!(before.backend_id, "mem");
    *ids_cell.lock().expect("ids_cell mutex") = Some((
        ticket.file_id,
        ticket.version_id,
        before.backend_id.clone(),
        before.backend_path.clone(),
    ));

    let expected_dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);

    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt1")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict from the losing CAS, got {err:?}"
    );

    assert!(
        !alt1_inner.exists(&expected_dest_path).await.unwrap(),
        "loser's target blob must be cleaned up after the CAS loses"
    );

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(after.backend_id, "alt2");
    assert_eq!(after.backend_path, expected_dest_path);
    let winner_bytes = alt2_backend.get(&expected_dest_path).await.unwrap();
    assert_eq!(winner_bytes, content, "winner's blob must be untouched");
}

/// Racer migrates to the SAME target: the path is deterministic, so our destination blob is the
/// winner's live data. The loser must return `Ok` without deleting it.
#[tokio::test]
async fn migrate_backend_same_target_race_preserves_winner_blob() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt1_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt1"));

    let tenant = Uuid::now_v7();
    let content = Bytes::from_static(b"same target race content");

    let ids_cell: RaceIds = Arc::new(std::sync::Mutex::new(None));

    let hook_ids_cell = Arc::clone(&ids_cell);
    let hook_store = store.clone();
    let hook_alt1 = Arc::clone(&alt1_inner);
    let hook_bytes = content.clone();
    let hook: Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send> =
        Box::new(move || {
            Box::pin(async move {
                let (file_id, version_id, orig_backend_id, orig_backend_path) = hook_ids_cell
                    .lock()
                    .expect("ids_cell mutex")
                    .clone()
                    .expect("ids must be set before migrate_backend runs");
                let dest_path = format!("/{file_id}/{version_id}");
                hook_alt1
                    .put(&dest_path, hook_bytes.clone())
                    .await
                    .expect("racer's own blob write");
                let audit = AuditEntry::success(
                    tenant,
                    "system",
                    Uuid::nil(),
                    Some(file_id),
                    AuditOperation::BackendMigrate,
                    serde_json::json!({ "racer": "concurrent-winner-same-target" }),
                );
                let won = hook_store
                    .rebind_version_backend(
                        file_id,
                        version_id,
                        &orig_backend_id,
                        &orig_backend_path,
                        "alt1",
                        &dest_path,
                        audit,
                    )
                    .await
                    .expect("racer's CAS call");
                assert!(
                    won,
                    "the injected racer's CAS must win: nothing else has touched the row yet"
                );
            })
        });

    let racing_alt1: Arc<dyn StorageBackend> = Arc::new(RacingBackend {
        inner: alt1_inner.clone(),
        on_put: std::sync::Mutex::new(Some(hook)),
    });

    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&racing_alt1)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    let ctx = ctx(tenant);
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must exist before migration");
    assert_eq!(before.backend_id, "mem");
    *ids_cell.lock().expect("ids_cell mutex") = Some((
        ticket.file_id,
        ticket.version_id,
        before.backend_id.clone(),
        before.backend_path.clone(),
    ));

    let expected_dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);

    svc.migrate_backend(&ctx, ticket.file_id, "alt1")
        .await
        .expect("same-target race must be a no-op, not an error");

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(after.backend_id, "alt1");
    assert_eq!(after.backend_path, expected_dest_path);

    assert!(
        alt1_inner.exists(&expected_dest_path).await.unwrap(),
        "winner's destination blob must NOT be deleted by the loser's cleanup"
    );
    let stored_bytes = alt1_inner.get(&expected_dest_path).await.unwrap();
    assert_eq!(
        stored_bytes, content,
        "surviving blob must match the winner's bytes"
    );
}
