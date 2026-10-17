//! Scaffolding shared by the upload-flow integration tests: temp-file SQLite, a
//! `FileService` + `MultipartService` stack over one store/backend, and small request/part helpers.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use axum::response::Response;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer};
use file_storage::domain::multipart::MultipartPlan;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

pub const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB (a bare `sqlite::memory:` gives each pooled connection its own DB),
/// fully migrated. Returns the DSN too, for tests that tamper through a second connection.
pub async fn build_db_with_dsn(tag: &str) -> (Arc<DBProvider<DbError>>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-{tag}-{}.db", Uuid::now_v7().simple()));
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
    (Arc::new(DBProvider::new(db)), dsn)
}

pub async fn build_db(tag: &str) -> Arc<DBProvider<DbError>> {
    build_db_with_dsn(tag).await.0
}

pub fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

pub fn new_file(name: &str) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: name.to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

pub fn base_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    }
}

pub async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("valid JSON body")
}

/// A `FileService` and a `MultipartService` over one fresh store, in-memory backend and issuer.
pub struct Stack {
    pub svc: Arc<FileService>,
    pub msvc: Arc<MultipartService>,
    pub multipart_store: Arc<dyn MultipartStore>,
    pub backend: Arc<dyn StorageBackend>,
    pub store: Store,
    pub dsn: String,
}

/// Build a [`Stack`] on the caller's `issuer` (so tests can mint tokens `svc.verifier()` accepts).
/// `complete_lease_secs` overrides the multipart complete lease when set.
pub async fn build_stack(
    tag: &str,
    issuer: Arc<Issuer>,
    complete_lease_secs: Option<i64>,
) -> Stack {
    build_stack_with(tag, issuer, 3600, |msvc| match complete_lease_secs {
        Some(secs) => msvc.with_complete_lease_secs(secs),
        None => msvc,
    })
    .await
}

/// [`build_stack`] with a custom per-part URL TTL (for both services) and a hook to tweak the
/// `MultipartService` (session TTL, lease, ...) before it is shared.
pub async fn build_stack_with(
    tag: &str,
    issuer: Arc<Issuer>,
    url_ttl_secs: i64,
    tweak: impl FnOnce(MultipartService) -> MultipartService,
) -> Stack {
    let (db, dsn) = build_db_with_dsn(tag).await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(db);
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        ServiceConfig {
            default_url_ttl_secs: url_ttl_secs,
            ..base_config()
        },
        None,
        None,
    ));
    let msvc = tweak(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        url_ttl_secs,
    ));
    Stack {
        svc,
        msvc: Arc::new(msvc),
        multipart_store,
        backend,
        store,
        dsn,
    }
}

/// Upload one part of `plan` the way the sidecar would: bytes to the backend, then the part row.
pub async fn put_part(
    store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    plan: &MultipartPlan,
    file_id: Uuid,
    part_number: u32,
    data: Bytes,
) {
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .expect("query session")
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    let part = plan
        .parts
        .iter()
        .find(|p| p.part_number == part_number)
        .unwrap_or_else(|| panic!("part {part_number} not in plan"));
    assert_eq!(
        data.len() as u64,
        part.size,
        "part {part_number}: test data must match the plan"
    );

    let (backend_etag, part_hash) = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            part_number,
            part.offset,
            data,
        )
        .await
        .expect("backend upload_part");

    store
        .upsert_multipart_part(
            plan.upload_id,
            i32::try_from(part_number).unwrap(),
            &backend_etag,
            part_hash,
            i64::try_from(part.size).unwrap(),
            time::OffsetDateTime::now_utc(),
        )
        .await
        .expect("upsert part");
}
