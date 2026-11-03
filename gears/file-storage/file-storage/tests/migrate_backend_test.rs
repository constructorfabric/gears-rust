//! `FileService::migrate_backend`: moving a file's content to another backend, its guards, and the
//! race with a concurrent migration.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement};
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation};
use file_storage::domain::authz::Authorizer;
use file_storage::domain::error::DomainError;
use file_storage::infra::backend::{InMemoryBackend, StorageBackend};
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::storage::Store;
use file_storage_sdk::NewFile;

mod common;
use common::{ScopedTestAuthorizer, Stack, StackBuilder, bound_file, ctx};

fn new_file() -> NewFile {
    common::new_file_of("test.txt", "text/plain")
}

/// Migration copies the content to the target backend, repoints the version row and audits it.
#[tokio::test]
async fn migrate_backend_moves_content_and_updates_version_row() {
    let Stack { svc, dp, store, .. } = StackBuilder::dual_backend("migrate-backend").build().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = bound_file(&svc, &dp, &ctx, b"migrate me").await;

    svc.migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap();

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.backend_id, "alt");
    let content = dp
        .read_content(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();
    assert_eq!(
        content,
        Bytes::from_static(b"migrate me"),
        "content must read back from the target backend"
    );

    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let migrate_rows: Vec<_> = audit
        .iter()
        .filter(|r| r.operation == "backend_migrate")
        .collect();
    assert_eq!(migrate_rows.len(), 1);
    assert_eq!(migrate_rows[0].outcome, "success");
}

#[tokio::test]
async fn migrate_backend_to_same_backend_is_noop() {
    let Stack { svc, dp, store, .. } = StackBuilder::dual_backend("migrate-backend").build().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = bound_file(&svc, &dp, &ctx, b"same backend").await;

    svc.migrate_backend(&ctx, ticket.file_id, "mem")
        .await
        .unwrap();

    let audit = store.list_audit(ticket.file_id).await.unwrap();
    assert!(
        audit.iter().all(|r| r.operation != "backend_migrate"),
        "no-op migration must not write an audit row"
    );
}

/// Files with more than one version cannot be migrated.
#[tokio::test]
async fn migrate_backend_rejects_versioned_file() {
    let Stack { svc, dp, .. } = StackBuilder::dual_backend("migrate-backend").build().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = bound_file(&svc, &dp, &ctx, b"v1").await;

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

/// Migrating onto a non-durable target needs the admin scope: a non-admin is `Forbidden` and the
/// version stays on its source backend.
#[tokio::test]
async fn migrate_backend_to_non_durable_target_requires_admin() {
    for admin in [false, true] {
        let authorizer = Arc::new(ScopedTestAuthorizer::admin_gated());
        let Stack { svc, dp, store, .. } = StackBuilder::dual_backend("migrate-backend")
            .authorizer(Arc::clone(&authorizer) as Arc<dyn Authorizer>)
            .build()
            .await;
        authorizer.set_admin(admin);
        let ctx = ctx(Uuid::now_v7());
        let ticket = bound_file(&svc, &dp, &ctx, b"admin-gated migrate").await;

        let result = svc.migrate_backend(&ctx, ticket.file_id, "alt").await;
        let version = store
            .get_version(ticket.file_id, ticket.version_id)
            .await
            .unwrap()
            .unwrap();
        if admin {
            result.unwrap_or_else(|e| panic!("admin must be allowed, got {e:?}"));
            assert_eq!(version.backend_id, "alt");
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, DomainError::Forbidden),
                "expected Forbidden, got {err:?}"
            );
            assert_eq!(version.backend_id, "mem");
        }
    }
}

/// A concurrent `migrate_backend` commits between our destination `put` and our CAS. Racing to a
/// different target, our CAS loses (`Conflict`) and our destination blob is cleaned up. Racing to
/// the same target, the deterministic path makes our blob the winner's live data: we return `Ok`
/// and must not delete it.
#[tokio::test]
async fn migrate_backend_race_with_another_migration() {
    for racer_target in ["alt2", "alt1"] {
        let same_target = racer_target == "alt1";
        let (db, dsn) = common::build_db_with_dsn("migrate-backend").await;
        let store = Store::new(Arc::clone(&db));
        let tenant = Uuid::now_v7();
        let content = Bytes::from_static(b"race content");

        let mem: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
        let alt1: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt1"));
        let alt2: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt2"));
        let racer_backend = if same_target { &alt1 } else { &alt2 };

        // The hook needs the file's ids, which exist only after the stack is built.
        let ids_cell: RaceIds = Arc::new(std::sync::Mutex::new(None));
        let hook: OnPutHook = {
            let (ids_cell, store, racer_backend, content) = (
                Arc::clone(&ids_cell),
                store.clone(),
                Arc::clone(racer_backend),
                content.clone(),
            );
            Box::new(move || {
                Box::pin(async move {
                    let (file_id, version_id, from_id, from_path) = ids_cell
                        .lock()
                        .expect("ids_cell mutex")
                        .clone()
                        .expect("ids must be set before migrate_backend runs");
                    let dest_path = format!("/{file_id}/{version_id}");
                    racer_backend
                        .put(&dest_path, content)
                        .await
                        .expect("racer's own blob write");
                    let audit = AuditEntry::success(
                        tenant,
                        "system",
                        Uuid::nil(),
                        Some(file_id),
                        AuditOperation::BackendMigrate,
                        serde_json::json!({ "racer": racer_backend.id() }),
                    );
                    let won = store
                        .rebind_version_backend(
                            file_id,
                            version_id,
                            &from_id,
                            &from_path,
                            racer_backend.id(),
                            &dest_path,
                            audit,
                        )
                        .await
                        .expect("racer's CAS call");
                    assert!(won, "the injected racer's CAS must win");
                })
            })
        };
        let racing_alt1: Arc<dyn StorageBackend> = Arc::new(RacingBackend {
            inner: Arc::clone(&alt1),
            on_put: std::sync::Mutex::new(Some(hook)),
        });

        let Stack { dp, svc, .. } = StackBuilder::new("migrate-backend")
            .backends(vec![mem, racing_alt1, Arc::clone(&alt2)], "mem")
            .db(db, &dsn)
            .build()
            .await;
        let ctx = ctx(tenant);
        let ticket = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();
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
        *ids_cell.lock().expect("ids_cell mutex") = Some((
            ticket.file_id,
            ticket.version_id,
            before.backend_id,
            before.backend_path,
        ));

        let result = svc.migrate_backend(&ctx, ticket.file_id, "alt1").await;
        let dest_path = common::backend_path(ticket.file_id, ticket.version_id);
        if same_target {
            result.expect("a same-target race must be a no-op, not an error");
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, DomainError::Conflict { .. }),
                "expected Conflict from the losing CAS, got {err:?}"
            );
            assert!(
                !alt1.exists(&dest_path).await.unwrap(),
                "the loser's target blob must be cleaned up"
            );
        }

        let after = store
            .get_version(ticket.file_id, ticket.version_id)
            .await
            .unwrap()
            .expect("version must still exist");
        assert_eq!(after.backend_id, racer_target, "{racer_target}");
        assert_eq!(after.backend_path, dest_path, "{racer_target}");
        assert_eq!(
            racer_backend.get(&dest_path).await.unwrap(),
            content,
            "{racer_target}: the winner's blob must survive untouched"
        );
    }
}

/// `(file_id, version_id, backend_id, backend_path)` of the file being migrated.
type RaceIds = Arc<std::sync::Mutex<Option<(Uuid, Uuid, String, String)>>>;

type OnPutHook = Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send>;

/// Runs a one-shot hook before `put`, modelling a racing `migrate_backend` that commits between
/// this call's destination `put` and its CAS, deterministically and in-process.
struct RacingBackend {
    inner: Arc<dyn StorageBackend>,
    on_put: std::sync::Mutex<Option<OnPutHook>>,
}

#[async_trait]
impl StorageBackend for RacingBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self) -> file_storage::infra::backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
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

/// Verification of a migrated multipart-composite version needs only the object bytes and the
/// manifest row, not the part rows.
#[tokio::test]
async fn migrate_backend_verifies_multipart_composite_without_parts_rows() {
    let (src, dst) = common::mem_and_alt_backends();
    let s = StackBuilder::new("migrate-backend")
        .backends(vec![Arc::clone(&src), Arc::clone(&dst)], "mem")
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, plan, _full) = common::composite_session(&s, &ctx).await;
    s.msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();

    let conn = Database::connect(&s.dsn).await.expect("raw connect");
    let deleted = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DELETE FROM multipart_upload_parts".to_owned(),
        ))
        .await
        .expect("delete parts");
    assert!(deleted.rows_affected() >= 1, "the part rows existed");
    // `migrate_backend` needs exactly one version: drop the pending one `create_file` left.
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM file_versions WHERE status = 'pending'".to_owned(),
    ))
    .await
    .expect("delete leftover pending version");

    s.svc
        .migrate_backend(&ctx, file_id, "alt")
        .await
        .expect("migrate must verify from object bytes + manifest row alone");

    let version = s
        .store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.backend_id, "alt");
    let manifest = s
        .store
        .get_version_manifest(plan.version_id)
        .await
        .unwrap()
        .unwrap();
    let moved = dst.get(&version.backend_path).await.unwrap();
    Store::verify_content_hash(
        &moved,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&manifest),
    )
    .expect("the destination copy must verify against the manifest");
}
