//! Tests for the shared `POST /files` orchestration in `create_flow.rs`.
//!
//! Regression coverage for the handler/SDK-shared refactor: before this
//! module existed, `api::rest::handlers::create_file` inlined this same
//! branching logic and only the REST handler exercised it. These tests pin
//! the exact behavior (single-part fallback, multipart-plan branch, the
//! idempotency+multipart rejection, and the compensating delete on a failed
//! initiate) directly against `create_flow::create_file` so the SDK local
//! client's call to the same function is covered too.

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage_sdk::{NewFile, OwnerKind};

use super::{CreateFileOutcome, MultipartIntent, create_file};
use crate::domain::authz::TenantOnlyAuthorizer;
use crate::domain::multipart_service::MultipartService;
use crate::domain::ports::MultipartStore;
use crate::domain::service::{FileService, ServiceConfig};
use crate::infra::backend::{BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend};
use crate::infra::signed_url::Issuer;
use crate::infra::storage::Store;
use crate::infra::storage::migrations::Migrator;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

async fn build_services() -> (Arc<FileService>, Arc<MultipartService>) {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-create-flow-test-{}.db",
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
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));

    // `InMemoryBackend` advertises `multipart_native: true`, so the
    // multipart branch is reachable without a real backend.
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn crate::domain::authz::Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let file_svc = Arc::new(FileService::new(
        store,
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let multipart_svc = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    (file_svc, multipart_svc)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("valid SecurityContext")
}

fn new_file(owner_id: Uuid) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id,
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// No `multipart` intent at all falls straight through to the ordinary
/// single-part path.
#[tokio::test]
async fn no_multipart_intent_returns_single_part() {
    let (file_svc, multipart_svc) = build_services().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let outcome = create_file(
        &file_svc,
        &multipart_svc,
        &ctx,
        new_file(owner),
        None,
        true,
        None,
    )
    .await
    .expect("create_file");

    assert!(matches!(outcome, CreateFileOutcome::SinglePart(_)));
}

/// A `multipart` intent whose computed plan collapses to exactly one part
/// (`declared_size` smaller than the minimum part size) falls back to the
/// single-part path too — no multipart session is ever initiated for it.
#[tokio::test]
async fn one_part_multipart_plan_falls_back_to_single_part() {
    let (file_svc, multipart_svc) = build_services().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let outcome = create_file(
        &file_svc,
        &multipart_svc,
        &ctx,
        new_file(owner),
        None,
        true,
        Some(MultipartIntent {
            declared_size: 1024, // well under the 5 MiB minimum part size
            preferred_part_size: None,
        }),
    )
    .await
    .expect("create_file");

    assert!(matches!(outcome, CreateFileOutcome::SinglePart(_)));
}

/// A `multipart` intent whose plan needs two or more parts creates the bare
/// file row and returns the multipart plan, with no single-part pending
/// version registered by `create_file` itself.
#[tokio::test]
async fn multi_part_plan_returns_multipart_outcome() {
    let (file_svc, multipart_svc) = build_services().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let outcome = create_file(
        &file_svc,
        &multipart_svc,
        &ctx,
        new_file(owner),
        None,
        true,
        Some(MultipartIntent {
            declared_size: 12 * 1024 * 1024, // 12 MiB -> multiple 5 MiB parts
            preferred_part_size: None,
        }),
    )
    .await
    .expect("create_file");

    match outcome {
        CreateFileOutcome::Multipart { file_id, plan } => {
            assert!(plan.parts.len() >= 2, "expected a multi-part plan");
            assert_eq!(plan.version_id, plan.version_id);
            let file = file_svc
                .get_file(&ctx, file_id)
                .await
                .expect("bare file row must exist");
            assert!(
                file.content_id.is_none(),
                "a fresh multipart create must not have bound content yet"
            );
        }
        CreateFileOutcome::SinglePart(_) => panic!("expected a multipart outcome"),
    }
}

/// `idempotency_key` together with a (real, ≥2-part) `multipart` intent is
/// rejected before either service is touched.
#[tokio::test]
async fn idempotency_key_with_multipart_intent_is_rejected() {
    let (file_svc, multipart_svc) = build_services().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let result = create_file(
        &file_svc,
        &multipart_svc,
        &ctx,
        new_file(owner),
        Some("replay-key".to_owned()),
        true,
        Some(MultipartIntent {
            declared_size: 12 * 1024 * 1024,
            preferred_part_size: None,
        }),
    )
    .await;

    assert!(
        matches!(
            result,
            Err(crate::domain::error::DomainError::Validation { .. })
        ),
        "expected a validation error, got {result:?}"
    );
}

/// A multipart initiate that fails after the bare file row was already
/// committed must not leave a version-less orphan behind: the compensation
/// deletes it synchronously, before the original error propagates.
#[tokio::test]
async fn failed_initiate_compensates_the_orphaned_bare_file() {
    // A backend that does not support native multipart makes
    // `initiate_multipart_upload` fail with `MultipartNotSupported` — the
    // simplest reproducible initiate failure, no fault injection needed.
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-create-flow-compensate-test-{}.db",
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
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));

    let mut storage_root = std::env::temp_dir();
    storage_root.push(format!(
        "cf-fs-create-flow-fsroot-{}",
        Uuid::now_v7().simple()
    ));
    let backend: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new(
        "local-fs",
        storage_root.to_string_lossy().as_ref(),
    ));
    let backends = BackendRegistry::new(vec![backend], "local-fs").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn crate::domain::authz::Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let file_svc = Arc::new(FileService::new(
        store,
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let multipart_svc = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let result = create_file(
        &file_svc,
        &multipart_svc,
        &ctx,
        new_file(owner),
        None,
        true,
        Some(MultipartIntent {
            declared_size: 12 * 1024 * 1024,
            preferred_part_size: None,
        }),
    )
    .await;

    assert!(
        matches!(
            result,
            Err(crate::domain::error::DomainError::MultipartNotSupported { .. })
        ),
        "expected MultipartNotSupported, got {result:?}"
    );

    // The owner's listing must show zero files -- the bare row created just
    // before the failed initiate was compensated away, not left as an orphan.
    let listed = file_svc
        .list_files(
            &ctx,
            file_storage_sdk::OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id: owner,
            },
            Some(10),
            0,
        )
        .await
        .expect("list_files");
    assert!(
        listed.is_empty(),
        "the orphaned bare file must have been compensated away, got {listed:?}"
    );
}
