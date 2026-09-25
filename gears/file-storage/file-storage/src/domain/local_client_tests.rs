//! Smoke tests for `FileStorageLocalClient`. Exhaustive per-method coverage
//! (success + characteristic failure, REST-equivalence, and the
//! service-owner scenario) lives in `tests/sdk_client_test.rs`; these
//! in-crate tests only pin that the client is object-safe and that its
//! thinnest wiring (construction, a single round trip, and an unknown-id
//! error) behaves as expected.

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage_sdk::{FileFetch, FileStorageClientV1, NewFile, OwnerKind};

use super::FileStorageLocalClient;
use crate::domain::authz::TenantOnlyAuthorizer;
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy_service::PolicyService;
use crate::domain::ports::{MultipartStore, PolicyStore};
use crate::domain::service::{FileService, ServiceConfig};
use crate::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use crate::infra::signed_url::Issuer;
use crate::infra::storage::Store;
use crate::infra::storage::migrations::Migrator;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

async fn build_client() -> FileStorageLocalClient {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-local-client-test-{}.db",
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
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let service = Arc::new(FileService::new(
        store,
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let multipart_service = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let policy_service = Arc::new(PolicyService::new(policy_store, authorizer));
    FileStorageLocalClient::new(service, multipart_service, policy_service)
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

#[tokio::test]
async fn usable_through_sdk_trait_object() {
    // This is how `ClientHub` stores it: behind `dyn FileStorageClientV1`.
    let client: Box<dyn FileStorageClientV1> = Box::new(build_client().await);
    let ctx = ctx(Uuid::now_v7());
    let storages = client
        .list_storages(&ctx)
        .await
        .expect("list_storages through the trait object");
    assert_eq!(storages.len(), 1);
    assert_eq!(storages[0].id, "mem");
}

#[tokio::test]
async fn create_file_then_get_file_round_trips() {
    let client = build_client().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let outcome = client
        .create_file(&ctx, new_file(owner), None, false, None)
        .await
        .expect("create_file");
    let ticket = match outcome {
        file_storage_sdk::CreateFileOutcome::SinglePart(t) => t,
        file_storage_sdk::CreateFileOutcome::Multipart { .. } => {
            panic!("expected a single-part outcome")
        }
    };

    let fetch = client
        .get_file(&ctx, ticket.file_id, None)
        .await
        .expect("get_file");
    match fetch {
        FileFetch::Modified { record, .. } => {
            assert_eq!(record.file.file_id, ticket.file_id);
            assert_eq!(record.file.owner_id, owner);
        }
        FileFetch::NotModified => panic!("expected Modified on a fresh file"),
    }
}

#[tokio::test]
async fn get_file_for_unknown_id_is_not_found() {
    let client = build_client().await;
    let ctx = ctx(Uuid::now_v7());

    let result = client.get_file(&ctx, Uuid::now_v7(), None).await;
    assert!(result.is_err(), "expected an error for an unknown file id");
}
