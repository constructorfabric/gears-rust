//! Idempotency-replay authorization and identity scoping.
//! `ScopedTestAuthorizer` can deny `WRITE` per subject (`create_file` passes `file_id: None`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::Mutex;

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
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Grants everything unless a `subject_id` has been marked write-denied.
#[derive(Default)]
struct ScopedTestAuthorizer {
    deny_write_for_subject: Mutex<Option<Uuid>>,
}

impl ScopedTestAuthorizer {
    fn new() -> Self {
        Self::default()
    }

    /// Mark `subject_id` as `WRITE`-denied.
    ///
    /// # Panics
    /// Panics if the mutex is poisoned.
    fn deny_write_for_subject(&self, subject_id: Uuid) {
        *self.deny_write_for_subject.lock().expect("lock poisoned") = Some(subject_id);
    }
}

#[async_trait]
impl Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if action == actions::WRITE
            && let Some(denied) = *self.deny_write_for_subject.lock().expect("lock poisoned")
            && denied == ctx.subject_id()
        {
            return Err(DomainError::Forbidden);
        }

        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-idempotency-authz-test-{}.db",
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
        name: "upload.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// A revoked caller must not replay a stored ticket (authorize runs before the lookup).
#[tokio::test]
async fn idempotency_replay_requires_authorization() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-authz-1".to_owned();

    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()))
        .await
        .expect("initial create should succeed while authorized");

    h.authz.deny_write_for_subject(subject);
    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key))
        .await;

    assert!(
        matches!(replay, Err(DomainError::Forbidden)),
        "expected Forbidden on replay after WRITE was revoked, got {replay:?}"
    );
    assert_ne!(first.file_id, Uuid::nil());
}

/// The key is scoped to the caller: another caller reusing the same `(owner_kind, owner_id, key)`
/// must be denied, not handed the stored ticket.
#[tokio::test]
async fn idempotency_key_scoped_to_subject() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject_a = Uuid::now_v7();
    let subject_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, subject_a);
    let ctx_b = ctx(tenant, subject_b);

    // Both requests share owner_id and key.
    let owner_id = subject_a;
    let key = "shared-key".to_owned();

    let ticket_a = h
        .file_svc
        .create_file(&ctx_a, new_file(owner_id), Some(key.clone()))
        .await
        .expect("caller A creates and stores the idempotency ticket");

    let result_b = h
        .file_svc
        .create_file(&ctx_b, new_file(owner_id), Some(key.clone()))
        .await;
    match &result_b {
        Ok(ticket_b) => assert_ne!(
            ticket_b.file_id, ticket_a.file_id,
            "caller B must never receive caller A's ticket"
        ),
        Err(DomainError::Forbidden) => {} // also an acceptable denial outcome
        Err(other) => panic!("unexpected error for caller B's replay: {other:?}"),
    }

    let replay_a = h
        .file_svc
        .create_file(&ctx_a, new_file(owner_id), Some(key))
        .await
        .expect("caller A's own replay must still succeed");
    assert_eq!(
        replay_a.file_id, ticket_a.file_id,
        "caller A's replay must return A's original ticket"
    );
}
