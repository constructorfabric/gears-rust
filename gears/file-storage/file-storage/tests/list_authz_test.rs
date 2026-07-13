//! Cross-user file-enumeration authorization tests (P2 remediation 0.9).
//!
//! `TenantOnlyAuthorizer` is not sufficient here — it ignores `action`
//! entirely, so it can't distinguish an `ADMIN_POLICY` grant from an ordinary
//! `READ`. This file duplicates the `ScopedTestAuthorizer` test double first
//! introduced in `tests/policy_authz_test.rs` (0.7) rather than extracting a
//! shared helper module: each `tests/*.rs` file compiles as its own
//! integration-test crate, so cross-file reuse would require either a shared
//! `tests/common/mod.rs` module (touching the already-landed 0.7 file to wire
//! it up) or a `[lib]`-style test harness restructuring — both more invasive
//! than the few lines duplicated below. `policy_authz_test.rs`'s doc comment
//! on `ScopedTestAuthorizer` explicitly anticipates this: "intentionally
//! self-contained ... so later steps (0.9/0.10/0.11) can reuse it verbatim."

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

// ── ScopedTestAuthorizer (duplicated from tests/policy_authz_test.rs) ───────

/// Grants `READ`/`WRITE`/`DELETE` unconditionally (subject to `deny_write_for`),
/// but only grants `ADMIN_POLICY` while `is_admin` is set.
#[derive(Default)]
struct ScopedTestAuthorizer {
    is_admin: AtomicBool,
    deny_write_for: Mutex<Option<Uuid>>,
}

impl ScopedTestAuthorizer {
    fn new() -> Self {
        Self::default()
    }

    /// Toggle whether `ADMIN_POLICY` is granted.
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

// ── test harness ─────────────────────────────────────────────────────────────

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-list-authz-test-{}.db",
        Uuid::now_v7().simple()
    ));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
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

// ── list_files ───────────────────────────────────────────────────────────────

/// `GET /files?owner_kind=user&owner_id=<victim>` from a non-owner, non-admin
/// caller must be denied and must not leak the victim's file listing.
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

/// Positive control: listing one's own files is always allowed.
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

/// An `ADMIN_POLICY`-authorized caller may list another user's files.
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
