//! Cross-user file-enumeration authorization tests.
//! Duplicates `ScopedTestAuthorizer` (each `tests/*.rs` is its own crate).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::authz::{Authorizer, actions};
use file_storage::domain::error::DomainError;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerFilter, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Grants `ADMIN_POLICY` only while `is_admin` is set; other actions are allowed.
#[derive(Default)]
struct ScopedTestAuthorizer {
    is_admin: AtomicBool,
    deny_write_for: Mutex<Option<Uuid>>,
}

impl ScopedTestAuthorizer {
    fn new() -> Self {
        Self::default()
    }

    fn set_admin(&self, admin: bool) {
        self.is_admin.store(admin, Ordering::SeqCst);
    }
}

#[async_trait]
impl Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if action == actions::ADMIN_POLICY {
            return if self.is_admin.load(Ordering::SeqCst) {
                Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
            } else {
                Err(DomainError::Forbidden)
            };
        }

        if action == actions::WRITE
            && let Some(denied) = *self.deny_write_for.lock().expect("lock poisoned")
            && Some(denied) == file_id
        {
            return Err(DomainError::Forbidden);
        }

        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-list-authz-test-{}.db",
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

struct Harness {
    file_svc: Arc<FileService>,
    authz: Arc<ScopedTestAuthorizer>,
}

async fn build_harness() -> Harness {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authz = Arc::new(ScopedTestAuthorizer::new());
    let authorizer: Arc<dyn Authorizer> = Arc::clone(&authz) as Arc<dyn Authorizer>;
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let file_svc = Arc::new(FileService::new(
        store, backends, issuer, authorizer, cfg, None, None,
    ));
    Harness { file_svc, authz }
}

fn ctx(tenant: Uuid, subject: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file(owner_id: Uuid) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id,
        name: "victim.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

fn owner_filter(owner_id: Uuid) -> OwnerFilter {
    OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id,
    }
}

/// A non-owner, non-admin listing another user's files must be denied without leaking them.
#[tokio::test]
async fn list_files_foreign_owner_without_admin_is_denied() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let user_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);
    let ctx_b = ctx(tenant, user_b);

    h.file_svc
        .create_file(&ctx_a, new_file(user_a), None)
        .await
        .expect("user A creates own file");

    let result = h
        .file_svc
        .list_files(&ctx_b, owner_filter(user_a), Some(10), 0)
        .await;
    assert!(
        matches!(result, Err(DomainError::Forbidden)),
        "expected Forbidden, got {result:?}"
    );
}

#[tokio::test]
async fn list_files_self_owner_is_allowed() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(user_a), None)
        .await
        .expect("user A creates own file");

    let found = h
        .file_svc
        .list_files(&ctx_a, owner_filter(user_a), Some(10), 0)
        .await
        .expect("self-owner list should succeed");
    assert!(found.iter().any(|f| f.file_id == ticket.file_id));
}

#[tokio::test]
async fn list_files_foreign_owner_with_admin_scope_is_allowed() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let admin = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);
    let ctx_admin = ctx(tenant, admin);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(user_a), None)
        .await
        .expect("user A creates own file");
    h.authz.set_admin(true);

    let found = h
        .file_svc
        .list_files(&ctx_admin, owner_filter(user_a), Some(10), 0)
        .await
        .expect("admin should be able to list foreign owner's files");
    assert!(found.iter().any(|f| f.file_id == ticket.file_id));
}
