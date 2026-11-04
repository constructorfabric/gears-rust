#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::{
    MultipartPlan, MultipartUploadSession, MultipartUploadState,
};
use file_storage::domain::policy::{
    AgeRetention, RetentionRuleBody, RetentionScope, StoredRetentionRule,
};
use file_storage::domain::ports::CleanupStore;
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::storage::Store;
use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, NewFile, VersionStatus};

mod common;
use common::{Stack, StackBuilder, bare_file, bound_file, ctx, initiate};

fn new_file() -> NewFile {
    common::new_file_of("test.txt", "text/plain")
}

/// A tenant retention rule that expires every file at once. It is inserted through the store,
/// bypassing `create_retention_rule`'s validation, so only the sweep matcher is exercised.
async fn insert_zero_age_rule(store: &Store, tenant: Uuid) {
    store
        .insert_retention_rule(
            &AccessScope::allow_all(),
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
            .get_file(&AccessScope::allow_all(), file_id)
            .await
    }

    async fn has_active_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        self.inner.has_active_multipart_for_file(file_id).await
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
        now: time::OffsetDateTime,
    ) -> Result<u64, DomainError> {
        self.inner.delete_expired_idempotency_keys(now).await
    }
}

/// Within the grace window a pending version is kept; a file without a matching retention rule is
/// never deleted.
#[tokio::test]
async fn sweep_keeps_recent_pending_versions_and_files_without_retention_rule() {
    let Stack {
        svc,
        dp,
        store,
        engine,
        ..
    } = StackBuilder::new("cleanup-test")
        .grace_secs(86400)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let pending = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let bound = bound_file(&svc, &dp, &ctx, b"should not be deleted").await;

    let result = engine.run_sweep().await;
    assert_eq!(result.abandoned_pending_deleted, 0);
    assert_eq!(result.retention_expired_deleted, 0);
    assert!(
        store
            .get_version(pending.file_id, pending.version_id)
            .await
            .unwrap()
            .is_some()
    );
    let file = svc.get_file(&ctx, bound.file_id).await.unwrap();
    assert_eq!(file.content_id, Some(bound.version_id));
}

/// Sweeping the file's only pending version also deletes the now-orphaned parent `files` row,
/// enqueues `file.deleted` and audits the reconcile (one row for the version, one for the file).
#[tokio::test]
async fn sweep_deletes_abandoned_zero_version_file() {
    let Stack {
        svc, store, engine, ..
    } = StackBuilder::new("cleanup-test")
        .grace_secs(0)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(result.abandoned_pending_deleted, 1);
    assert_eq!(result.abandoned_files_deleted, 1);

    assert!(
        store
            .get_version(t.file_id, t.version_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_file(&AccessScope::allow_all(), t.file_id)
            .await
            .unwrap()
            .is_none()
    );
    let events = store.list_file_events(t.file_id).await.unwrap();
    assert!(events.iter().any(|e| e.event_type == "file.deleted"));
    let audit = store.list_audit(t.file_id).await.unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|r| r.operation == "orphan_reconcile")
            .count(),
        2
    );
}

/// Negative control: a file that still has a bound `Available` version keeps its parent row.
#[tokio::test]
async fn sweep_keeps_file_with_other_versions() {
    let Stack {
        svc,
        dp,
        store,
        engine,
        ..
    } = StackBuilder::new("cleanup-test")
        .grace_secs(0)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let v1 = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let v2 = svc.presign_version(&ctx, v1.file_id).await.unwrap();
    let content = bytes::Bytes::from_static(b"real content");
    dp.put_content(&ctx, v1.file_id, v2.version_id, "text/plain", content)
        .await
        .unwrap();
    svc.bind(&ctx, v1.file_id, v2.version_id, None)
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "the abandoned pending v1 is reclaimed"
    );
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "the file still has a bound version"
    );

    assert!(
        store
            .get_version(v1.file_id, v1.version_id)
            .await
            .unwrap()
            .is_none()
    );
    let file = svc.get_file(&ctx, v1.file_id).await.unwrap();
    assert_eq!(file.content_id, Some(v2.version_id));
    let v2_row = store
        .get_version(v1.file_id, v2.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v2_row.status, VersionStatus::Available);
}

/// A pending version backing a live (`in_progress` or `completing`) session is never reclaimed,
/// however old, or a long-running upload would lose its version before `complete`.
#[tokio::test]
async fn sweep_skips_pending_version_of_live_multipart_session() {
    for (row, completing, expected_state) in [
        ("in_progress", false, MultipartUploadState::InProgress),
        ("completing", true, MultipartUploadState::Completing),
    ] {
        let OpenSession {
            s,
            ctx,
            file_id,
            plan,
        } = file_with_open_session().await;
        if completing {
            common::plant_lease(&s.multipart_store, plan.upload_id, "completer-a", 300).await;
        }
        backdate_version_created_at(&s.db, plan.version_id).await;

        let result = s.engine.run_sweep().await;
        assert_eq!(result.abandoned_pending_deleted, 0, "{row}");
        assert_eq!(result.abandoned_files_deleted, 0, "{row}");
        assert!(
            s.store
                .get_version(file_id, plan.version_id)
                .await
                .unwrap()
                .is_some(),
            "{row}"
        );
        s.svc
            .get_file(&ctx, file_id)
            .await
            .unwrap_or_else(|e| panic!("{row}: {e:?}"));
        let session = s
            .store
            .get_multipart_upload(plan.upload_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session.state, expected_state,
            "{row}: the live session stays untouched"
        );
    }
}

/// A `list_versions` failure aborts that file's expiry instead of being treated as zero versions;
/// an unrelated matching file is still deleted in the same sweep, with a success audit row.
#[tokio::test]
async fn expire_file_list_versions_error_does_not_delete_file() {
    let stack = StackBuilder::new("cleanup-test").build().await;
    let (svc, dp, store) = (&stack.svc, &stack.dp, &stack.store);
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let faulted = bound_file(svc, dp, &ctx, b"faulted").await;
    let healthy = bound_file(svc, dp, &ctx, b"healthy").await;
    insert_zero_age_rule(store, tenant).await;

    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyListVersionsStore {
        inner: store.clone(),
        fault_file_id: faulted.file_id,
    });
    let result = stack
        .cleanup_engine_over(faulty_store, 86400)
        .run_sweep()
        .await;
    assert_eq!(
        result.retention_expired_deleted, 1,
        "only the healthy file counts"
    );

    let scope = AccessScope::allow_all();
    assert!(
        store
            .get_file(&scope, faulted.file_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_file(&scope, healthy.file_id)
            .await
            .unwrap()
            .is_none()
    );
    let audit = store.list_audit(healthy.file_id).await.unwrap();
    let deleted: Vec<_> = audit
        .iter()
        .filter(|r| r.operation == "retention_delete")
        .collect();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].outcome, "success");
}

// Sweep-vs-complete races are deterministic call orderings.

/// Completed first, then backdated: `complete_multipart_upload` rejects an expired `in_progress`
/// session, so the sweep's session CAS must lose against an already-`completed` row.
#[tokio::test]
async fn sweep_after_complete_wins_does_not_delete_bound_version() {
    let Stack {
        svc,
        msvc,
        store,
        engine,
        backend,
        multipart_store,
        ..
    } = StackBuilder::new("cleanup-test")
        .grace_secs(0)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = common::session_with_part(
        &msvc,
        &multipart_store,
        &backend,
        &ctx,
        file_id,
        b"Hello, World!",
        false,
    )
    .await;
    msvc.complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();
    svc.bind(&ctx, file_id, plan.version_id, None)
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
        result.expired_multipart_aborted, 0,
        "the session CAS loses to `completed`"
    );

    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.status, VersionStatus::Available);
    let file = svc.get_file(&ctx, file_id).await.unwrap();
    assert_eq!(file.content_id, Some(plan.version_id));
}

/// Reverse order: the sweep aborts the expired session, deletes its version and the now-orphaned
/// parent file in one pass; a later `complete` finds no file.
#[tokio::test]
async fn sweep_before_complete_wins_cleans_up_expired_session() {
    let Stack {
        svc,
        msvc,
        store,
        engine,
        ..
    } = StackBuilder::new("cleanup-test")
        .grace_secs(0)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = initiate(&msvc, &ctx, file_id, 13).await;
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
        "the sweep wins the session CAS"
    );
    assert_eq!(
        result.abandoned_files_deleted, 1,
        "the zero-version file goes in the same pass"
    );
    assert!(
        store
            .get_version(file_id, plan.version_id)
            .await
            .unwrap()
            .is_none()
    );

    let err = svc.get_file(&ctx, file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "got {err:?}"
    );
    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "late complete: got {err:?}"
    );
}

/// Window where `finalize_version` already made the version `Available` but the session CAS has
/// not run: the cleanup's status-guarded delete must match zero rows.
#[tokio::test]
async fn cleanup_skips_version_already_finalized() {
    let Stack {
        svc,
        msvc,
        store,
        engine,
        ..
    } = StackBuilder::new("cleanup-test")
        .grace_secs(0)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = initiate(&msvc, &ctx, file_id, 5).await;
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();

    let audit = AuditEntry::success(
        Uuid::nil(),
        "system",
        Uuid::nil(),
        Some(file_id),
        AuditOperation::FinalizeVersion,
        serde_json::json!({}),
    );
    let outcome = store
        .finalize_version(
            file_id,
            plan.version_id,
            5,
            vec![0u8; 32],
            HashMode::WholeSha256,
            None,
            None,
            None,
            audit,
            None,
        )
        .await
        .unwrap();
    assert!(outcome.updated);

    engine.cleanup_expired_session_version(&session).await;

    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.status, VersionStatus::Available);
}

/// Rows are seeded directly; `idempotency_keys.file_id` has an FK, so they point at real files.
/// `orphan_grace_secs` stays 86400: zero would sweep these FK-only files and cascade the rows.
#[tokio::test]
async fn run_sweep_deletes_expired_idempotency_rows() {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;

    use file_storage::infra::storage::entity::idempotency_key;
    use file_storage::infra::storage::repo::IdempotencyRepo;
    use file_storage::infra::storage::store::IdempotencyInsert;

    let Stack {
        db, engine, svc, ..
    } = StackBuilder::new("cleanup-test").build().await;
    let tenant_id = Uuid::now_v7();
    let ctx = ctx(tenant_id);
    let expired_file = bare_file(&svc, &ctx).await;
    let live_file = bare_file(&svc, &ctx).await;
    let conn = db.conn().expect("conn");
    let repo = IdempotencyRepo::new();
    let now = time::OffsetDateTime::now_utc();

    for (key, file_id, expires_at) in [
        ("expired-key", expired_file, now - time::Duration::hours(1)),
        ("live-key", live_file, now + time::Duration::days(1)),
    ] {
        let row = IdempotencyInsert {
            tenant_id,
            owner_kind: "user".to_owned(),
            owner_id: Uuid::now_v7(),
            key: key.to_owned(),
            subject_id: Uuid::now_v7(),
            response_status: 201,
            response_body: "{}".to_owned(),
            response_etag: "etag".to_owned(),
            request_hash: key.as_bytes().to_vec(),
            expires_at,
        };
        repo.insert(&conn, &row, file_id, now - time::Duration::hours(2))
            .await
            .expect(key);
    }

    let result = engine.run_sweep().await;
    assert_eq!(result.idempotency_keys_deleted, 1);

    let rows = idempotency_key::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query idempotency_keys directly");
    assert_eq!(rows.len(), 1, "only the live row should remain");
    assert_eq!(rows[0].idempotency_key, "live-key");
}

/// Unpublished outbox rows must survive regardless of age (`published_at` stays NULL until a relay
/// exists). Seeded directly: there is no API to backdate `occurred_at`.
#[tokio::test]
async fn run_sweep_does_not_touch_unpublished_outbox_rows() {
    use sea_orm::{EntityTrait, Set};
    use toolkit_db::secure::{SecureEntityExt, secure_insert};

    use file_storage::infra::storage::entity::{audit_outbox, events_outbox};

    let Stack { db, engine, .. } = StackBuilder::new("cleanup-test")
        .grace_secs(0)
        .build()
        .await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let ancient = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
    let (tenant_id, file_id) = (Uuid::now_v7(), Uuid::now_v7());
    let (audit_id, event_id) = (Uuid::now_v7(), Uuid::now_v7());

    let audit = audit_outbox::ActiveModel {
        event_id: Set(audit_id),
        tenant_id: Set(tenant_id),
        actor_kind: Set("system".to_owned()),
        actor_id: Set(Uuid::nil()),
        file_id: Set(Some(file_id)),
        operation: Set("orphan_reconcile".to_owned()),
        outcome: Set("success".to_owned()),
        detail: Set(serde_json::json!({})),
        occurred_at: Set(ancient),
        published_at: Set(None),
    };
    secure_insert::<audit_outbox::Entity>(audit, &scope, &conn)
        .await
        .expect("seed audit");
    let event = events_outbox::ActiveModel {
        event_id: Set(event_id),
        tenant_id: Set(tenant_id),
        owner_id: Set(Uuid::now_v7()),
        file_id: Set(file_id),
        event_type: Set("file.deleted".to_owned()),
        payload: Set(serde_json::json!({})),
        occurred_at: Set(ancient),
        published_at: Set(None),
    };
    secure_insert::<events_outbox::Entity>(event, &scope, &conn)
        .await
        .expect("seed event");

    engine.run_sweep().await;

    let audit_rows = audit_outbox::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    assert!(
        audit_rows.iter().any(|r| r.event_id == audit_id),
        "audit_outbox row must survive"
    );
    let event_rows = events_outbox::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    assert!(
        event_rows.iter().any(|r| r.event_id == event_id),
        "events_outbox row must survive"
    );
}

/// A file with one open (`in_progress`, 1 KiB) multipart session on a stack with a one-hour
/// grace window.
struct OpenSession {
    s: Stack,
    ctx: SecurityContext,
    file_id: Uuid,
    plan: MultipartPlan,
}

async fn file_with_open_session() -> OpenSession {
    let s = StackBuilder::new("cleanup-test")
        .grace_secs(3600)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&s.svc, &ctx).await;
    let plan = initiate(&s.msvc, &ctx, file_id, 1024).await;
    OpenSession {
        s,
        ctx,
        file_id,
        plan,
    }
}

/// Moves a version's `created_at` two hours back, past the grace window (no public API for that).
async fn backdate_version_created_at(db: &Arc<DBProvider<DbError>>, version_id: Uuid) {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };

    let conn = db.conn().expect("conn");
    let backdated = time::OffsetDateTime::now_utc() - time::Duration::hours(2);
    FileVersionEntity::update_many()
        .col_expr(FileVersionColumn::CreatedAt, Expr::value(backdated))
        .filter(FileVersionColumn::VersionId.eq(version_id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");
}

/// An expired `completing` session (dead completer's lease) is aborted by the sweep, lease cleared.
#[tokio::test]
async fn sweep_aborts_expired_completing_session() {
    let OpenSession {
        s:
            Stack {
                store,
                engine,
                multipart_store,
                ..
            },
        plan,
        ..
    } = file_with_open_session().await;
    common::plant_lease(&multipart_store, plan.upload_id, "stale-completer", -3600).await;
    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(result.expired_multipart_aborted, 1);

    let after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, MultipartUploadState::Aborted);
    assert!(
        after.lease_until.is_none(),
        "the abort clears the lease with the state change"
    );
}

#[tokio::test]
async fn cleanup_aborts_on_the_sessions_own_backend_when_version_is_already_gone() {
    let (mem_backend, alt_backend) = common::mem_and_alt_backends();
    let Stack { engine, svc, .. } = StackBuilder::new("cleanup-test")
        .backends(vec![mem_backend, Arc::clone(&alt_backend)], "mem")
        .grace_secs(3600)
        .build()
        .await;

    let ctx = ctx(Uuid::now_v7());
    let file_id = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap()
        .file_id;
    let upload = common::start_raw_upload(&alt_backend, file_id).await;

    let now = time::OffsetDateTime::now_utc();
    let session = MultipartUploadSession {
        upload_id: Uuid::now_v7(),
        file_id,
        version_id: upload.version_id,
        backend_upload_handle: upload.handle.clone(),
        state: MultipartUploadState::InProgress,
        declared_mime: "application/octet-stream".to_owned(),
        mime_validated: false,
        declared_size: 0,
        part_size: 0,
        auto_bind: false,
        lease_until: None,
        complete_result: None,
        backend_id: Some("alt".to_owned()),
        backend_path: upload.backend_path.clone(),
        created_at: now,
        expires_at: now - time::Duration::hours(1),
    };

    engine.cleanup_expired_session_version(&session).await;

    common::assert_raw_upload_aborted(&alt_backend, &upload).await;
}
