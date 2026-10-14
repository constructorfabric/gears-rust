#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::idempotency::compute_request_hash;
use file_storage::domain::multipart::{
    BindState, MultipartCompleteOutcome, MultipartPlan, MultipartUploadState,
};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{PolicyBody, PolicyScope, SizeLimits};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{DataPlanePort, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{
    BackendCapabilities, BackendRegistry, InMemoryBackend, LocalFsBackend, MultipartCompletionPart,
    StorageBackend,
};
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::{HashMode, Manifest, ManifestEntry};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{ByteRange, CustomMetadataEntry, NewFile, OwnerKind};

mod common;
use common::put_part;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Also returns the raw DSN so idempotency tests can open a second connection and tamper with rows.
async fn build_db_with_dsn() -> (Arc<DBProvider<DbError>>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-mp-test-{}.db", Uuid::now_v7().simple()));
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

async fn build_db() -> Arc<DBProvider<DbError>> {
    build_db_with_dsn().await.0
}

async fn build_service_with_config(
    idempotency_ttl_secs: u64,
) -> (Arc<FileService>, Arc<MultipartService>, DataPlaneService) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    (svc, msvc, dp)
}

async fn build_service() -> (Arc<FileService>, Arc<MultipartService>, DataPlaneService) {
    build_service_with_config(86400).await
}

async fn build_file_service_with_dsn(idempotency_ttl_secs: u64) -> (Arc<FileService>, String) {
    let (db, dsn) = build_db_with_dsn().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store, backends, issuer, authorizer, cfg, None, None,
    ));
    (svc, dsn)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut acc, b| {
        write!(acc, "{b:02x}").expect("writing to a String cannot fail");
        acc
    })
}

async fn count_files_rows(dsn: &str) -> i64 {
    let conn = Database::connect(dsn).await.expect("raw connect");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS c FROM files".to_owned(),
        ))
        .await
        .expect("count query")
        .expect("one row");
    row.try_get::<i64>("", "c").expect("i64 column c")
}

/// Overwrites `request_hash` via raw SQL (stored records are immutable via the API) to
/// simulate a hash for another owner. `Uuid` columns are 16-byte BLOBs, hence `X'...'` literals.
async fn tamper_request_hash(
    dsn: &str,
    tenant_id: Uuid,
    owner_kind: &str,
    owner_id: Uuid,
    key: &str,
    request_hash: &[u8],
) {
    let conn = Database::connect(dsn).await.expect("raw connect");
    let tenant_hex = hex_encode(tenant_id.as_bytes());
    let owner_hex = hex_encode(owner_id.as_bytes());
    let hash_hex = hex_encode(request_hash);
    let sql = format!(
        "UPDATE idempotency_keys SET request_hash = X'{hash_hex}' \
             WHERE tenant_id = X'{tenant_hex}' AND owner_kind = '{owner_kind}' \
             AND owner_id = X'{owner_hex}' AND idempotency_key = '{key}'"
    );
    let res = conn
        .execute_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("tamper request_hash");
    assert_eq!(
        res.rows_affected(),
        1,
        "tamper UPDATE must hit exactly the one row created by the test setup"
    );
}

async fn build_service_with_policy() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<PolicyService>,
    DataPlaneService,
) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
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
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        Arc::clone(&authorizer),
        None,
        Arc::clone(&issuer),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let psvc = Arc::new(PolicyService::new(policy_store, authorizer, 50, 1000));
    (svc, msvc, psvc, dp)
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
        name: "upload.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// Simulates a sidecar on a native-multipart backend: `upload_part` on the backend, then
/// `upsert_multipart_part` to record the part row.
async fn simulate_sidecar_put_part(
    store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    plan: &MultipartPlan,
    backend_path: &str,
    backend_handle: &str,
    part_number: u32,
    data: Bytes,
) {
    let part = plan
        .parts
        .iter()
        .find(|p| p.part_number == part_number)
        .unwrap_or_else(|| panic!("part {part_number} not in plan"));

    assert_eq!(
        data.len() as u64,
        part.size,
        "part {part_number}: simulated sidecar size enforcement — body len {} != plan size {}",
        data.len(),
        part.size,
    );

    let (backend_etag, part_hash) = backend
        .upload_part(backend_path, backend_handle, part_number, part.offset, data)
        .await
        .expect("backend upload_part");

    let size = i64::try_from(part.size).unwrap();
    let now = time::OffsetDateTime::now_utc();
    let part_number_i32 = i32::try_from(part_number).unwrap();

    store
        .upsert_multipart_part(
            plan.upload_id,
            part_number_i32,
            &backend_etag,
            part_hash,
            size,
            now,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn multipart_happy_path_in_memory() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size = 13u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();

    assert_eq!(plan.parts.len(), 1, "13 bytes fits in one part");
    assert!(!plan.upload_id.is_nil());

    let p = &plan.parts[0];
    assert_eq!(p.part_number, 1);
    assert_eq!(p.offset, 0);
    assert_eq!(p.size, declared_size);
    assert!(!p.upload_url.is_empty());

    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    let data = Bytes::from_static(b"Hello, World!");
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        data,
    )
    .await;

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();

    svc.bind(&ctx, ticket.file_id, plan.version_id, None)
        .await
        .unwrap();

    let content = dp
        .read_content(&ctx, ticket.file_id, plan.version_id, None)
        .await
        .unwrap();
    assert_eq!(content, Bytes::from_static(b"Hello, World!"));
}

const JPEG_MAGIC: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
];

/// Declared `image/png` but the parts assemble into a JPEG: version and session stay unchanged,
/// and the assembled object is left for the orphan sweep.
#[tokio::test]
async fn multipart_complete_rejects_content_not_matching_declared_mime() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let declared_size = JPEG_MAGIC.len() as u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "image/png",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(JPEG_MAGIC),
    )
    .await;

    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MimeMismatch { .. }),
        "expected MimeMismatch, got {err:?}"
    );

    let version = multipart_store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Pending);
    assert_eq!(version.mime_type, "image/png");

    let session_after = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must still exist");
    assert_eq!(session_after.state, MultipartUploadState::InProgress);
    assert!(
        !session_after.mime_validated,
        "mime_validated must stay false when validation failed"
    );
}

#[tokio::test]
async fn multipart_complete_persists_validated_mime_and_flag() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let content = Bytes::from_static(b"Hello, World! This is plain text.");
    let declared_size = content.len() as u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "text/plain",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        content,
    )
    .await;

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();

    let version = multipart_store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Available);
    assert_eq!(version.mime_type, "text/plain");

    let session_after = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must still exist");
    assert!(
        session_after.mime_validated,
        "mime_validated must be true after a successful complete"
    );
}

#[tokio::test]
async fn multipart_full_lifecycle_create_to_delete() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    );
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let declared_size = 13u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(b"Hello, World!"),
    )
    .await;
    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();
    svc.bind(&ctx, ticket.file_id, plan.version_id, None)
        .await
        .unwrap();

    svc.get_file(&ctx, ticket.file_id)
        .await
        .expect("file must exist before delete");
    assert!(
        svc.list_versions(&ctx, ticket.file_id, None, None)
            .await
            .unwrap()
            .items
            .iter()
            .any(|v| v.version_id == plan.version_id),
        "the completed multipart version must be present before delete",
    );

    svc.delete_file(&ctx, ticket.file_id, Some("*"))
        .await
        .expect("delete must succeed");

    assert!(
        matches!(
            svc.get_file(&ctx, ticket.file_id).await,
            Err(DomainError::FileNotFound { .. })
        ),
        "file must be FileNotFound after delete",
    );
}

#[tokio::test]
async fn multipart_rejected_on_local_fs() {
    let db = build_db().await;
    let tmp = std::env::temp_dir().join(format!("cf-fs-localfs-{}", Uuid::now_v7().simple()));
    std::fs::create_dir_all(&tmp).unwrap();
    let local: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new("local-fs", &tmp));
    let backends = BackendRegistry::new(vec![local], "local-fs").expect("registry");
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
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            1024,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MultipartNotSupported { .. }),
        "expected MultipartNotSupported, got {err:?}"
    );
}

/// `parts = ceil(size / part_size)`, the last part takes the remainder, sizes sum to the declared
/// size. Uses the minimum valid `preferred_part_size`.
#[tokio::test]
async fn initiate_returns_coherent_parts_plan() {
    let (svc, msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    // Minimum valid `preferred_part_size` (`DEFAULT_MIN_PART_SIZE`) to force multiple parts.
    let part_size = 5 * 1024 * 1024u64; // DEFAULT_MIN_PART_SIZE
    let declared_size = 2 * part_size + 3;
    let preferred_part_size = Some(part_size); // forces plan: [part_size, part_size, 3]
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            preferred_part_size,
            false,
        )
        .await
        .unwrap();

    assert!(!plan.upload_id.is_nil());
    assert!(!plan.parts.is_empty());
    assert_eq!(plan.part_hash_algorithm, "SHA-256");

    let mut total = 0u64;
    let mut prev_offset = 0u64;
    for (i, p) in plan.parts.iter().enumerate() {
        assert_eq!(
            p.part_number as usize,
            i + 1,
            "parts must be 1-based sequential"
        );
        assert_eq!(p.offset, prev_offset, "offset must be contiguous");
        assert!(p.size > 0, "part size must be positive");
        assert!(!p.upload_url.is_empty(), "upload_url must not be empty");
        assert!(
            p.upload_url.contains("sidecar.test"),
            "upload_url must point at sidecar"
        );
        assert!(
            p.upload_url.contains("fs-token"),
            "upload_url must contain fs-token"
        );
        total += p.size;
        prev_offset += p.size;
    }
    assert_eq!(
        total, declared_size,
        "sum of part sizes must equal declared_size"
    );
}

#[tokio::test]
async fn idempotency_same_key_returns_same_file() {
    let (svc, _msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let mut nf = new_file();
    let owner_id = nf.owner_id;
    let key = "idem-key-1".to_owned();

    let t1 = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    nf.owner_id = owner_id; // same owner
    let t2 = svc.create_file(&ctx, nf, Some(key), false).await.unwrap();

    assert_eq!(
        t1.file_id, t2.file_id,
        "idempotent retry must return the same file_id"
    );
    assert_eq!(t1.version_id, t2.version_id);
}

fn max_size_claim(url: &str, verifier: &file_storage::infra::signed_url::Verifier) -> Option<u64> {
    let token_start = url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
    let token = &url[token_start..];
    let now = time::OffsetDateTime::now_utc();
    verifier
        .verify(token, now)
        .expect("token must verify")
        .upload
        .max_size
}

#[tokio::test]
async fn idempotency_replay_reflects_tightened_size_policy() {
    use file_storage::infra::signed_url::Issuer;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let verifier = issuer.verifier();
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
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store,
        backends,
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let psvc = PolicyService::new(policy_store, authorizer, 50, 1000);

    let ctx = ctx(Uuid::now_v7());

    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(1024 * 1024),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let nf = new_file();
    let key = "size-policy-replay-key".to_owned();
    let original = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    assert_eq!(
        max_size_claim(&original.upload_url, &verifier),
        Some(1024 * 1024),
        "the original ticket's token must carry the permissive policy's max_size"
    );

    // Tighten the policy to 10 bytes, then replay with the same key.
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(10),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let replayed = svc.create_file(&ctx, nf, Some(key), false).await.unwrap();
    assert_eq!(replayed.file_id, original.file_id);
    assert_eq!(replayed.version_id, original.version_id);

    assert_eq!(
        max_size_claim(&replayed.upload_url, &verifier),
        Some(10),
        "a replay must re-mint the upload URL against the CURRENT (tightened) policy's \
         max_size, not silently replay the original ticket's now-stale, larger constraint"
    );
}

#[tokio::test]
async fn idempotency_replay_with_diverging_name_returns_conflict() {
    let (svc, dsn) = build_file_service_with_dsn(86400).await;
    let ctx = ctx(Uuid::now_v7());
    let key = "diverging-name-key".to_owned();

    let mut nf = new_file();
    nf.name = "original.bin".to_owned();
    svc.create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    nf.name = "different.bin".to_owned();
    let err = svc
        .create_file(&ctx, nf, Some(key), false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict on a diverging name, got {err:?}"
    );
    assert_eq!(
        count_files_rows(&dsn).await,
        1,
        "a rejected replay must not create a second file"
    );
}

#[tokio::test]
async fn idempotency_replay_with_diverging_metadata_returns_conflict() {
    let (svc, dsn) = build_file_service_with_dsn(86400).await;
    let ctx = ctx(Uuid::now_v7());
    let key = "diverging-metadata-key".to_owned();

    let mut nf = new_file();
    nf.custom_metadata = vec![CustomMetadataEntry {
        key: "tag".to_owned(),
        value: "a".to_owned(),
    }];
    svc.create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    nf.custom_metadata = vec![CustomMetadataEntry {
        key: "tag".to_owned(),
        value: "b".to_owned(),
    }];
    let err = svc
        .create_file(&ctx, nf, Some(key), false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict on diverging metadata, got {err:?}"
    );
    assert_eq!(
        count_files_rows(&dsn).await,
        1,
        "a rejected replay must not create a second file"
    );
}

/// Owner is part of the primary key, so a real owner change never finds the row; the stored hash is
/// tampered to exercise the owner leg of the comparison.
#[tokio::test]
async fn idempotency_replay_with_diverging_owner_returns_conflict() {
    let (svc, dsn) = build_file_service_with_dsn(86400).await;
    let ctx = ctx(Uuid::now_v7());
    let key = "diverging-owner-key".to_owned();

    let nf = new_file();
    svc.create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    let other_owner = Uuid::now_v7();
    let tampered_hash = compute_request_hash(
        nf.owner_kind.as_str(),
        other_owner,
        &nf.name,
        &nf.gts_file_type,
        &nf.mime_type,
        &[],
    );
    tamper_request_hash(
        &dsn,
        ctx.subject_tenant_id(),
        nf.owner_kind.as_str(),
        nf.owner_id,
        &key,
        &tampered_hash,
    )
    .await;

    let err = svc
        .create_file(&ctx, nf, Some(key), false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict when the stored hash reflects a different owner, got {err:?}"
    );
    assert_eq!(
        count_files_rows(&dsn).await,
        1,
        "a rejected replay must not create a second file"
    );
}

#[tokio::test]
async fn idempotency_different_owner_different_file() {
    let (svc, _msvc, _dp) = build_service().await;
    let tenant = Uuid::now_v7();
    let ctx_a = ctx(tenant);
    let ctx_b = ctx(tenant); // same tenant, different subject (different owner_id in NewFile)

    let key = "shared-key".to_owned();

    let mut nf_a = new_file();
    nf_a.owner_id = Uuid::now_v7();
    let mut nf_b = new_file();
    nf_b.owner_id = Uuid::now_v7(); // different owner_id

    let t_a = svc
        .create_file(&ctx_a, nf_a, Some(key.clone()), false)
        .await
        .unwrap();
    let t_b = svc
        .create_file(&ctx_b, nf_b, Some(key), false)
        .await
        .unwrap();

    assert_ne!(
        t_a.file_id, t_b.file_id,
        "different owners must get distinct files even with the same key"
    );
}

#[tokio::test]
async fn idempotency_expiry_creates_new_file() {
    let (svc, _msvc, _dp) = build_service_with_config(1).await;
    let ctx = ctx(Uuid::now_v7());
    let mut nf = new_file();
    let owner_id = nf.owner_id;

    let key = "expiry-key".to_owned();
    let t1 = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    nf.owner_id = owner_id;
    let t2 = svc.create_file(&ctx, nf, Some(key), false).await.unwrap();

    assert_ne!(
        t1.file_id, t2.file_id,
        "after expiry, the same key must create a new file"
    );
}

/// Oversized declared size is rejected at initiate, before any backend state is created.
#[tokio::test]
async fn initiate_multipart_rejected_when_declared_size_exceeds_policy_limit() {
    let (svc, msvc, psvc, _dp) = build_service_with_policy().await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let owner = Uuid::now_v7();

    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(10),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let ticket = svc
        .create_file(
            &ctx,
            NewFile {
                owner_kind: OwnerKind::User,
                owner_id: owner,
                name: "large.bin".to_owned(),
                gts_file_type: GTS.to_owned(),
                mime_type: "application/octet-stream".to_owned(),
                custom_metadata: vec![],
            },
            None,
            false,
        )
        .await
        .unwrap();

    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            11,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicySizeExceeded { .. }),
        "expected PolicySizeExceeded at initiate, got {err:?}"
    );
}

#[tokio::test]
async fn initiate_multipart_allowed_when_declared_size_within_policy_limit() {
    let (svc, msvc, psvc, _dp) = build_service_with_policy().await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let owner = Uuid::now_v7();

    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(100),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let ticket = svc
        .create_file(
            &ctx,
            NewFile {
                owner_kind: OwnerKind::User,
                owner_id: owner,
                name: "small.bin".to_owned(),
                gts_file_type: GTS.to_owned(),
                mime_type: "application/octet-stream".to_owned(),
                custom_metadata: vec![],
            },
            None,
            false,
        )
        .await
        .unwrap();

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            50,
            None,
            false,
        )
        .await
        .unwrap();
    assert!(!plan.upload_id.is_nil());
}

/// Must be rejected up front, before `compute_plan` could overflow or over-allocate.
#[tokio::test]
async fn initiate_multipart_rejects_absurd_preferred_part_size() {
    let (svc, msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            1024,
            Some(u64::MAX),
            false,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation for an absurd preferred_part_size, got {err:?}"
    );
}

#[tokio::test]
async fn initiate_plan_urls_carry_valid_multipart_tokens() {
    use file_storage::infra::signed_url::Op;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let verifier = issuer.verifier();

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
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        Arc::clone(&issuer),
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let part_size = 5 * 1024 * 1024u64; // DEFAULT_MIN_PART_SIZE
    let declared_size = 2 * part_size + 3;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            Some(part_size),
            false,
        )
        .await
        .unwrap();

    let now = time::OffsetDateTime::now_utc();
    for p in &plan.parts {
        let url = &p.upload_url;
        let token_start = url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
        let token = &url[token_start..];

        let claims = verifier.verify(token, now).expect("token must verify");

        assert_eq!(
            claims.op,
            Op::MultipartPart,
            "op must be MultipartPart for part {}",
            p.part_number
        );
        assert_eq!(claims.file_id, ticket.file_id);
        assert_eq!(claims.version_id, plan.version_id);
        assert_eq!(claims.multipart.upload_id, plan.upload_id);
        assert_eq!(claims.multipart.part_number, p.part_number);
        assert_eq!(claims.multipart.offset, p.offset);
        assert_eq!(
            claims.multipart.size, p.size,
            "size claim must match plan for part {}",
            p.part_number
        );
    }
}

/// Mirrors the private `build_backend_registry` with the default config: `local-fs` is not
/// `multipart_native`, so initiate is rejected. Flip once the default backend supports multipart.
#[tokio::test]
async fn multipart_initiate_against_real_default_topology_is_rejected_until_backend_supports_it() {
    use file_storage::config::FileStorageConfig;

    let db = build_db().await;
    let cfg = FileStorageConfig::default();
    assert!(
        !cfg.enable_in_memory_backend,
        "this test locks in the REAL default topology (local-fs only); if this \
         default flips, the doc caveat in multipart-coordinator.md and this test \
         both need updating"
    );

    let mut backend_list: Vec<Arc<dyn StorageBackend>> =
        vec![Arc::new(LocalFsBackend::new("local-fs", &cfg.storage_root))];
    if cfg.enable_in_memory_backend {
        backend_list.push(Arc::new(InMemoryBackend::new("memory")));
    }
    let backends = BackendRegistry::new(backend_list, "local-fs").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let svc_cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        svc_cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            1024,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MultipartNotSupported { .. }),
        "expected MultipartNotSupported against the real default topology, got {err:?}"
    );
}

/// Parts are reported through the real `report_multipart_part` handler. DB state is asserted
/// via the entity, not `list_multipart_parts` (the method under test). Declaring just over 2x
/// the minimum part size forces 3 parts [min, min, 3]; bytes are written only for MIME sniffing.
#[tokio::test]
async fn multipart_complete_uses_reported_parts_not_empty_list() {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;
    use tower::ServiceExt;

    use file_storage::api::rest::handlers;
    use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
    use file_storage::infra::signed_url::Verifier;
    use file_storage::infra::storage::entity::multipart_upload_part;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let verifier: Arc<Verifier> = Arc::new(issuer.verifier());
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
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store.clone()) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        Arc::clone(&issuer),
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size = 2 * DEFAULT_MIN_PART_SIZE + 3;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        plan.parts.len(),
        3,
        "declared_size = 2*min + 3 must plan exactly 3 parts"
    );

    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(
        "test-internal-secret".to_owned(),
        time::Duration::ZERO,
    ));

    let router = Router::new()
        .route(
            "/api/file-storage/v1/files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report",
            post(handlers::report_multipart_part),
        )
        .layer(axum::Extension(Arc::clone(&verifier)))
        .layer(axum::Extension(finalize_auth))
        .layer(axum::Extension(Arc::clone(&msvc)));

    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    let mut expected_total: i64 = 0;
    for part in &plan.parts {
        let token_start =
            part.upload_url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
        let token = &part.upload_url[token_start..];

        let size = i64::try_from(part.size).unwrap();
        expected_total += size;

        backend
            .upload_part(
                &backend_path,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                Bytes::from(vec![b'x'; usize::try_from(part.size).unwrap()]),
            )
            .await
            .expect("backend upload_part");

        let body = serde_json::json!({
            "backend_etag": format!("etag-{}", part.part_number),
            "hash_hex": hex::encode([u8::try_from(part.part_number % 256).unwrap(); 32]),
            "size": size,
        });

        let uri = format!(
            "/api/file-storage/v1/files/{}/versions/{}/multipart/{}/parts/{}/report",
            ticket.file_id, plan.version_id, plan.upload_id, part.part_number
        );
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-fs-token", token)
            .header("x-fs-internal-token", "test-internal-secret")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = router.clone().oneshot(req).await.expect("router dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "report_multipart_part must succeed for part {}",
            part.part_number
        );
    }

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();

    let conn = db.conn().expect("conn");
    let rows = multipart_upload_part::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query multipart_upload_parts directly");
    assert_eq!(
        rows.len(),
        plan.parts.len(),
        "multipart_upload_parts must have exactly one row per reported part"
    );
    let db_total: i64 = rows.iter().map(|r| r.size).sum();
    assert_eq!(db_total, expected_total);

    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(
        version.size, db_total,
        "completed version size must equal the sum of reported part sizes"
    );
}

/// The callback is token-authenticated, so the reported `size` must match `claims.multipart.size`;
/// no part row may be persisted for a forged size.
#[tokio::test]
async fn report_part_rejects_forged_size() {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;
    use tower::ServiceExt;

    use file_storage::api::rest::handlers;
    use file_storage::infra::signed_url::Verifier;
    use file_storage::infra::storage::entity::multipart_upload_part;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let verifier: Arc<Verifier> = Arc::new(issuer.verifier());
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
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store.clone()) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        Arc::clone(&issuer),
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size: u64 = 100;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        plan.parts.len(),
        1,
        "small declared_size must plan one part"
    );
    let part = &plan.parts[0];
    let planned_size = i64::try_from(part.size).unwrap();

    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(
        "test-internal-secret".to_owned(),
        time::Duration::ZERO,
    ));

    let router = Router::new()
        .route(
            "/api/file-storage/v1/files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report",
            post(handlers::report_multipart_part),
        )
        .layer(axum::Extension(Arc::clone(&verifier)))
        .layer(axum::Extension(finalize_auth))
        .layer(axum::Extension(Arc::clone(&msvc)));

    let token_start =
        part.upload_url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
    let token = &part.upload_url[token_start..];

    let forged_size = planned_size + 1;
    let body = serde_json::json!({
        "backend_etag": "forged-etag",
        "hash_hex": hex::encode([7u8; 32]),
        "size": forged_size,
    });
    let uri = format!(
        "/api/file-storage/v1/files/{}/versions/{}/multipart/{}/parts/{}/report",
        ticket.file_id, plan.version_id, plan.upload_id, part.part_number
    );
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-fs-token", token)
        .header("x-fs-internal-token", "test-internal-secret")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = router.clone().oneshot(req).await.expect("router dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "a forged part size must be rejected"
    );

    let conn = db.conn().expect("conn");
    let rows = multipart_upload_part::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query multipart_upload_parts directly");
    assert!(
        rows.is_empty(),
        "a rejected forged-size report must not persist any part row"
    );
}

/// Table-driven: a `local-fs` registry rejects initiate, a `memory` registry accepts it.
#[tokio::test]
async fn multipart_initiate_rejected_when_backend_not_multipart_native() {
    struct Case {
        name: &'static str,
        backend: fn() -> Arc<dyn StorageBackend>,
        backend_id: &'static str,
        expect_multipart_supported: bool,
    }

    let cases = [
        Case {
            name: "local-fs-only registry",
            backend: || {
                let tmp =
                    std::env::temp_dir().join(format!("cf-fs-mpn-{}", Uuid::now_v7().simple()));
                std::fs::create_dir_all(&tmp).expect("create tmp dir");
                Arc::new(LocalFsBackend::new("local-fs", tmp)) as Arc<dyn StorageBackend>
            },
            backend_id: "local-fs",
            expect_multipart_supported: false,
        },
        Case {
            name: "memory-only registry",
            backend: || Arc::new(InMemoryBackend::new("memory")) as Arc<dyn StorageBackend>,
            backend_id: "memory",
            expect_multipart_supported: true,
        },
    ];

    for case in cases {
        let db = build_db().await;
        let backend = (case.backend)();
        let backends = BackendRegistry::new(vec![backend], case.backend_id).expect("registry");
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
        let svc = Arc::new(FileService::new(
            store.clone(),
            backends.clone(),
            Arc::clone(&issuer),
            Arc::clone(&authorizer),
            cfg,
            None,
            None,
        ));
        let msvc = Arc::new(MultipartService::new(
            Arc::new(store) as Arc<dyn MultipartStore>,
            backends,
            authorizer,
            None,
            issuer,
            "http://sidecar.test".to_owned(),
            3600,
        ));

        let ctx = ctx(Uuid::now_v7());
        let ticket = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();

        let result = msvc
            .initiate_multipart_upload(
                &ctx,
                ticket.file_id,
                "application/octet-stream",
                1024,
                None,
                false,
            )
            .await;

        if case.expect_multipart_supported {
            assert!(
                result.is_ok(),
                "case '{}': expected multipart to be accepted, got {:?}",
                case.name,
                result.err()
            );
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, DomainError::MultipartNotSupported { .. }),
                "case '{}': expected MultipartNotSupported, got {err:?}",
                case.name
            );
        }
    }
}

/// Counts `complete_multipart` calls to prove rejections short-circuit before the backend.
struct CompleteCallCountingBackend {
    inner: Arc<dyn StorageBackend>,
    calls: Arc<AtomicUsize>,
}

impl CompleteCallCountingBackend {
    fn new(inner: Arc<dyn StorageBackend>) -> (Arc<Self>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(Self {
            inner,
            calls: Arc::clone(&calls),
        });
        (backend, calls)
    }
}

#[async_trait]
impl StorageBackend for CompleteCallCountingBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
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
        self.inner.put(path, bytes).await
    }
    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.inner.get(path).await
    }
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<futures::stream::BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_stream(path).await
    }
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
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
    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        self.inner.initiate_multipart(path).await
    }
    async fn upload_part(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        part_offset: u64,
        data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        self.inner
            .upload_part(path, upload_handle, part_number, part_offset, data)
            .await
    }
    async fn complete_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .complete_multipart(path, upload_handle, parts)
            .await
    }
    async fn abort_multipart(&self, path: &str, upload_handle: &str) -> Result<(), DomainError> {
        self.inner.abort_multipart(path, upload_handle).await
    }
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        self.inner.list_paths().await
    }
}

#[tokio::test]
async fn complete_returns_version_size_and_composite_hash() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let content = Bytes::from_static(b"Hello, World!");
    let declared_size = content.len() as u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        content.clone(),
    )
    .await;

    let completed = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    let expected_hash = hash::sha256(&content);

    assert_eq!(completed.version_id, plan.version_id);
    assert_eq!(completed.size, i64::try_from(declared_size).unwrap());
    assert_eq!(completed.hash_algorithm, "SHA-256");
    assert_eq!(completed.content_hash, expected_hash);
    assert_eq!(completed.hash_mode, HashMode::WholeSha256);
    assert_eq!(completed.part_count, 1);
    assert_eq!(completed.manifest, None);

    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(version.hash_mode, HashMode::WholeSha256.as_str());
    assert_eq!(version.part_count, None);
    assert_eq!(version.hash_value, expected_hash);
    assert_eq!(
        store.get_version_manifest(plan.version_id).await.unwrap(),
        None,
        "a one-part completion must not persist a manifest row"
    );
}

/// Bind A, then B (A's ETag is now stale); completing a third session with A's ETag must fail
/// before any session/version mutation.
#[tokio::test]
async fn complete_with_stale_if_match_is_rejected() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan_a = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            false,
        )
        .await
        .unwrap();
    let session_a = multipart_store
        .get_multipart_upload(plan_a.upload_id)
        .await
        .unwrap()
        .expect("session a must exist");
    let backend_path_a = format!("/{}/{}", ticket.file_id, plan_a.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan_a,
        &backend_path_a,
        &session_a.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan_a.upload_id, None)
        .await
        .unwrap();
    let bound_a = svc
        .bind(&ctx, ticket.file_id, plan_a.version_id, None)
        .await
        .unwrap();
    let etag_after_bind_a =
        file_storage::domain::etag::etag_for(&bound_a).expect("etag after first bind");

    let plan_b = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            false,
        )
        .await
        .unwrap();
    let session_b = multipart_store
        .get_multipart_upload(plan_b.upload_id)
        .await
        .unwrap()
        .expect("session b must exist");
    let backend_path_b = format!("/{}/{}", ticket.file_id, plan_b.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan_b,
        &backend_path_b,
        &session_b.backend_upload_handle,
        1,
        Bytes::from_static(b"BBBBB"),
    )
    .await;
    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan_b.upload_id, None)
        .await
        .unwrap();
    svc.bind(
        &ctx,
        ticket.file_id,
        plan_b.version_id,
        Some(&etag_after_bind_a),
    )
    .await
    .unwrap();

    let plan_c = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            false,
        )
        .await
        .unwrap();
    let session_c = multipart_store
        .get_multipart_upload(plan_c.upload_id)
        .await
        .unwrap()
        .expect("session c must exist");
    let backend_path_c = format!("/{}/{}", ticket.file_id, plan_c.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan_c,
        &backend_path_c,
        &session_c.backend_upload_handle,
        1,
        Bytes::from_static(b"CCCCC"),
    )
    .await;

    let err = msvc
        .complete_multipart_upload(
            &ctx,
            ticket.file_id,
            plan_c.upload_id,
            Some(&etag_after_bind_a),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed for a stale If-Match, got {err:?}"
    );

    let session_c_after = multipart_store
        .get_multipart_upload(plan_c.upload_id)
        .await
        .unwrap()
        .expect("session c must still exist");
    assert_eq!(
        session_c_after.state,
        MultipartUploadState::InProgress,
        "a rejected If-Match must not touch the session's state"
    );
}

/// `If-Match: *` skips the comparison even when the file has bound content.
#[tokio::test]
async fn complete_wildcard_if_match_succeeds() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan_a = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            false,
        )
        .await
        .unwrap();
    let session_a = multipart_store
        .get_multipart_upload(plan_a.upload_id)
        .await
        .unwrap()
        .expect("session a must exist");
    let backend_path_a = format!("/{}/{}", ticket.file_id, plan_a.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan_a,
        &backend_path_a,
        &session_a.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan_a.upload_id, None)
        .await
        .unwrap();
    svc.bind(&ctx, ticket.file_id, plan_a.version_id, None)
        .await
        .unwrap();

    let plan_b = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            false,
        )
        .await
        .unwrap();
    let session_b = multipart_store
        .get_multipart_upload(plan_b.upload_id)
        .await
        .unwrap()
        .expect("session b must exist");
    let backend_path_b = format!("/{}/{}", ticket.file_id, plan_b.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan_b,
        &backend_path_b,
        &session_b.backend_upload_handle,
        1,
        Bytes::from_static(b"BBBBB"),
    )
    .await;

    let completed = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan_b.upload_id, Some("*"))
        .await
        .expect("If-Match: * must bypass the precondition check")
        .unwrap_completed();
    assert_eq!(completed.version_id, plan_b.version_id);
}

/// Only parts 1 and 3 reported: `MultipartPartsMissing` lists the gap, the backend's
/// `complete_multipart` is never reached, and the session stays `in_progress`.
#[tokio::test]
async fn complete_with_missing_parts_lists_them() {
    use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;

    let db = build_db().await;
    let inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let (counting, calls) = CompleteCallCountingBackend::new(inner);
    let backend: Arc<dyn StorageBackend> = counting;
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size = 2 * DEFAULT_MIN_PART_SIZE + 3;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        plan.parts.len(),
        3,
        "declared_size must plan exactly 3 parts"
    );

    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    for part in plan.parts.iter().filter(|p| p.part_number != 2) {
        let data = vec![b'x'; usize::try_from(part.size).unwrap()];
        simulate_sidecar_put_part(
            &multipart_store,
            &backend,
            &plan,
            &backend_path,
            &session.backend_upload_handle,
            part.part_number,
            Bytes::from(data),
        )
        .await;
    }

    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    match err {
        DomainError::MultipartPartsMissing { upload_id, missing } => {
            assert_eq!(upload_id, plan.upload_id);
            assert_eq!(missing, vec![2], "exactly part 2 must be reported missing");
        }
        other => panic!("expected MultipartPartsMissing, got {other:?}"),
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a missing-parts rejection must never reach the backend's complete_multipart"
    );

    let session_after = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must still exist");
    assert_eq!(session_after.state, MultipartUploadState::InProgress);
}

/// 3-part plan, only part 1 reported: `received == [1]`, `missing == [2, 3]` with fresh URLs.
#[tokio::test]
async fn introspect_reports_received_and_missing_parts() {
    use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;

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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        Arc::clone(&issuer),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size = 2 * DEFAULT_MIN_PART_SIZE + 3;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        plan.parts.len(),
        3,
        "declared_size must plan exactly 3 parts"
    );

    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    let part1 = plan.parts.iter().find(|p| p.part_number == 1).unwrap();
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from(vec![b'x'; usize::try_from(part1.size).unwrap()]),
    )
    .await;

    let status = msvc
        .introspect_multipart_upload(&ctx, ticket.file_id, plan.upload_id)
        .await
        .unwrap();

    assert_eq!(status.upload_id, plan.upload_id);
    assert_eq!(status.version_id, plan.version_id);
    assert_eq!(status.state, MultipartUploadState::InProgress);
    assert_eq!(status.declared_size, declared_size);
    assert_eq!(status.part_size, plan.part_size);

    assert_eq!(status.received.len(), 1, "exactly part 1 was reported");
    assert_eq!(status.received[0].part_number, 1);
    assert_eq!(status.received[0].size, i64::try_from(part1.size).unwrap());

    assert_eq!(status.missing.len(), 2, "parts 2 and 3 are still missing");
    let plan_by_number: std::collections::HashMap<u32, _> =
        plan.parts.iter().map(|p| (p.part_number, p)).collect();
    for missing in &status.missing {
        assert!(
            missing.part_number == 2 || missing.part_number == 3,
            "unexpected missing part {}",
            missing.part_number
        );
        let planned = plan_by_number
            .get(&missing.part_number)
            .expect("missing part must be in the original plan");
        assert_eq!(missing.offset, planned.offset, "offset must match the plan");
        assert_eq!(missing.size, planned.size, "size must match the plan");
        assert!(
            missing.upload_url.is_some(),
            "part {} must have a fresh resume upload_url",
            missing.part_number
        );
    }
}

/// A foreign `upload_id` is masked as not found, indistinguishable from a missing one.
#[tokio::test]
async fn introspect_foreign_upload_id_is_not_found() {
    let (svc, msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket_a = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let ticket_b = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan_a = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket_a.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();

    let err = msvc
        .introspect_multipart_upload(&ctx, ticket_b.file_id, plan_a.upload_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MultipartUploadNotFound { .. }),
        "expected MultipartUploadNotFound for a foreign upload_id, got {err:?}"
    );
}

/// Expired but still `in_progress` (no sweep ran): full accounting, but no resume URLs.
#[tokio::test]
async fn introspect_expired_session_returns_state_without_urls() {
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();

    // Backdate `expires_at`: session stays `in_progress` in the DB but is no longer resumable.
    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let status = msvc
        .introspect_multipart_upload(&ctx, ticket.file_id, plan.upload_id)
        .await
        .unwrap();

    assert_eq!(status.state, MultipartUploadState::InProgress);
    assert_eq!(
        status.missing.len(),
        1,
        "the single-part plan has exactly one missing part"
    );
    for missing in &status.missing {
        assert!(
            missing.upload_url.is_none(),
            "an expired session must not mint a resume URL for part {}",
            missing.part_number
        );
    }
}

/// Resume URL token `exp` is capped at the session's `expires_at`, not a fresh TTL.
#[tokio::test]
async fn introspect_resume_urls_expire_with_session() {
    use file_storage::infra::signed_url::Op;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let verifier = issuer.verifier();
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        Arc::clone(&issuer),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");

    let status = msvc
        .introspect_multipart_upload(&ctx, ticket.file_id, plan.upload_id)
        .await
        .unwrap();

    assert_eq!(status.missing.len(), 1);
    let missing = &status.missing[0];
    let upload_url = missing
        .upload_url
        .as_deref()
        .expect("a live session must mint a resume URL");

    let token_start = upload_url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
    let token = &upload_url[token_start..];
    let now = time::OffsetDateTime::now_utc();
    let claims = verifier
        .verify(token, now)
        .expect("resume token must verify");

    assert_eq!(claims.op, Op::MultipartPart);
    assert_eq!(claims.file_id, ticket.file_id);
    assert_eq!(claims.version_id, plan.version_id);
    assert_eq!(claims.multipart.upload_id, plan.upload_id);
    assert_eq!(claims.multipart.part_number, missing.part_number);
    assert_eq!(claims.multipart.offset, missing.offset);
    assert_eq!(claims.multipart.size, missing.size);
    assert!(
        claims.exp <= session.expires_at.unix_timestamp(),
        "resume token exp ({}) must not exceed the session's own expires_at ({})",
        claims.exp,
        session.expires_at.unix_timestamp()
    );
}

/// `build_service` that also exposes the `Store`, to read session/version rows back.
async fn build_service_with_store() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    DataPlaneService,
    Store,
) {
    let s = common::build_stack(
        "mp-test",
        Arc::new(Issuer::generate(3600).expect("issuer")),
        None,
    )
    .await;
    let dp = DataPlaneService::new(Arc::clone(&s.svc) as Arc<dyn DataPlanePort>);
    (s.svc, s.msvc, dp, s.store)
}

#[tokio::test]
async fn initiate_multipart_upload_persists_backend_id_and_path_on_the_session() {
    let (svc, msvc, _dp, store) = build_service_with_store().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            1024,
            None,
            false,
        )
        .await
        .unwrap();

    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("pending version must exist");

    assert_eq!(
        session.backend_id, version.backend_id,
        "session.backend_id must match the pending version's own backend_id"
    );
    assert_eq!(
        session.backend_path, version.backend_path,
        "session.backend_path must match the pending version's own backend_path"
    );
}

/// (a) An absurd `declared_size` (`u64::MAX`) must be rejected quickly with a `400`-class
/// (`DomainError::Validation`) error -- never drive a giant allocation or hang the request.
#[tokio::test]
async fn initiate_multipart_rejects_absurd_declared_size_quickly() {
    let (svc, msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            u64::MAX,
            None,
            false,
        )
        .await
        .unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation for an absurd declared_size, got {err:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "rejecting an absurd declared_size must be fast, not attempt a huge allocation; took \
         {elapsed:?}"
    );
}

#[tokio::test]
async fn initiate_session_expiry_uses_dedicated_session_ttl_not_url_ttl() {
    let issuer = Arc::new(Issuer::generate(100_000).expect("issuer"));
    // url_ttl_secs = 60s (per-part URLs), session_ttl_secs = 3600s (60x longer) -- mirrors gear.rs
    // wiring `default_url_ttl_secs` vs the dedicated `multipart_session_ttl_secs`.
    let common::Stack {
        svc,
        msvc,
        multipart_store,
        ..
    } = common::build_stack_with("mp-test", Arc::clone(&issuer), 60, |m| {
        m.with_session_ttl_secs(3600)
    })
    .await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let before = time::OffsetDateTime::now_utc();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();
    let after = time::OffsetDateTime::now_utc();

    // The plan's own `expires_at` (per-part URL expiry) must reflect the short url_ttl_secs, not
    // the session TTL.
    assert!(
        plan.expires_at <= after + time::Duration::seconds(60 + 5),
        "plan.expires_at must use the short url_ttl_secs, got {} (now ~ {after})",
        plan.expires_at
    );

    // The persisted session row must use the much longer session_ttl_secs.
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    assert!(
        session.expires_at >= before + time::Duration::seconds(3600 - 5),
        "session.expires_at must be ~ now + session_ttl_secs (3600s), got {} (initiated ~ \
         {before})",
        session.expires_at
    );
    assert!(
        session.expires_at > plan.expires_at,
        "the session must outlive its own first batch of per-part URLs: session {} vs plan {}",
        session.expires_at,
        plan.expires_at
    );

    // The session outlives the URL TTL (asserted above): session.expires_at is ~3600s out while
    // url_ttl_secs is only 60s.
    assert!(
        session.expires_at > time::OffsetDateTime::now_utc() + time::Duration::seconds(60),
        "sanity check: the session must outlive the url_ttl_secs window for this test to be \
         meaningful"
    );

    let introspect_started = time::OffsetDateTime::now_utc();
    let status = msvc
        .introspect_multipart_upload(&ctx, ticket.file_id, plan.upload_id)
        .await
        .unwrap();
    let missing = status
        .missing
        .first()
        .expect("single-part upload has exactly one missing part");
    let upload_url = missing
        .upload_url
        .as_deref()
        .expect("a live session must mint a resume URL");
    let token_start = upload_url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
    let token = &upload_url[token_start..];
    let verifier = issuer.verifier();
    let claims = verifier
        .verify(token, time::OffsetDateTime::now_utc())
        .expect("resume token must verify");
    assert!(
        claims.exp <= (introspect_started + time::Duration::seconds(60 + 5)).unix_timestamp(),
        "resume token exp ({}) must be capped at now + url_ttl_secs (60s), not minted with the \
         session's long-lived expires_at",
        claims.exp
    );
    assert!(
        claims.exp < session.expires_at.unix_timestamp(),
        "resume token exp ({}) must be strictly less than the session's own expires_at ({}) -- \
         proving the URL TTL cap actually bites when it is much shorter than the session TTL",
        claims.exp,
        session.expires_at.unix_timestamp()
    );
}

#[tokio::test]
async fn abort_multipart_upload_uses_the_sessions_own_backend_when_version_is_already_gone() {
    let db = build_db().await;
    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_backend)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        ServiceConfig {
            default_url_ttl_secs: 3600,
            sidecar_base_url: "http://sidecar.test".to_owned(),
            default_page_size: 50,
            max_page_size: 1000,
            idempotency_ttl_secs: 86400,
        },
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store.clone()) as Arc<dyn MultipartStore>,
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let backend_handle = alt_backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate on the alt backend");

    let now = time::OffsetDateTime::now_utc();
    let upload_id = Uuid::now_v7();
    store
        .create_multipart_upload(
            upload_id,
            file_id,
            version_id,
            &backend_handle,
            "alt",
            &backend_path,
            "application/octet-stream",
            0,
            0,
            false,
            now + time::Duration::hours(1),
            now,
        )
        .await
        .expect("insert session row");

    msvc.abort_multipart_upload(&ctx, file_id, upload_id)
        .await
        .expect("abort_multipart_upload");

    // Prove the abort landed on "alt", not "mem": a still-live handle would accept another
    // `upload_part` call; an aborted one reports "handle not found".
    let after_abort = alt_backend
        .upload_part(
            &backend_path,
            &backend_handle,
            1,
            0,
            Bytes::from_static(b"x"),
        )
        .await;
    assert!(
        after_abort.is_err(),
        "the multipart handle on the session's OWN backend (\"alt\") must have been \
         aborted, but it is still live: {after_abort:?}"
    );
}

#[tokio::test]
async fn initiate_multipart_upload_rejects_overflowing_session_ttl_instead_of_panicking() {
    let common::Stack { svc, msvc, .. } = common::build_stack_with(
        "mp-test",
        Arc::new(Issuer::generate(3600).expect("issuer")),
        3600,
        |m| m.with_session_ttl_secs(i64::MAX),
    )
    .await;

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .expect("create_file");

    let result = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await;

    assert!(
        result.is_err(),
        "an overflowing session_ttl_secs must be rejected with a DomainError, not panic: \
         {result:?}"
    );
}

#[tokio::test]
async fn multipart_complete_retry_is_idempotent() {
    let (svc, msvc, multipart_store, backend, _store, ctx) = build_redesign_env().await;

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let declared_size = 13u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    put_part(
        &multipart_store,
        &backend,
        &plan,
        ticket.file_id,
        1,
        Bytes::from_static(b"Hello, World!"),
    )
    .await;

    let first = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    let replay = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .expect("re-complete of a completed session must be idempotent")
        .unwrap_completed();
    assert_eq!(replay.version_id, first.version_id);
    assert_eq!(replay.size, first.size);
    assert_eq!(replay.content_hash, first.content_hash);
    assert_eq!(replay.hash_mode, first.hash_mode);
    assert_eq!(replay.manifest, first.manifest);
    assert_eq!(replay.bind_state, first.bind_state);
    assert_eq!(replay.etag, first.etag);
}

#[tokio::test]
async fn finalize_multipart_version_rejects_after_cleanup_aborts_completing_session() {
    use file_storage::domain::audit::{AuditEntry, AuditOperation};
    use file_storage::domain::ports::MultipartFinishSnapshot;
    use file_storage::infra::content::hash_mode::HashMode;
    use file_storage_sdk::VersionStatus;
    use toolkit_security::AccessScope;

    let (svc, msvc, _dp, store) = build_service_with_store().await;
    let tenant_id = Uuid::now_v7();
    let ctx = ctx(tenant_id);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            10 * 1024 * 1024,
            Some(5 * 1024 * 1024),
            false,
        )
        .await
        .unwrap();

    let now = time::OffsetDateTime::now_utc();
    let acquired = store
        .acquire_multipart_complete_lease(
            plan.upload_id,
            "stale-completer",
            now - time::Duration::seconds(1),
            now,
        )
        .await
        .unwrap();
    assert!(
        acquired,
        "a fresh in_progress session must accept the lease"
    );

    let abort_audit = AuditEntry::success(
        tenant_id,
        "system",
        Uuid::nil(),
        Some(ticket.file_id),
        AuditOperation::MultipartAbort,
        serde_json::json!({"reason": "expired_multipart_session_cleanup"}),
    );
    let aborted = store
        .abort_multipart_upload(plan.upload_id, abort_audit)
        .await
        .unwrap();
    assert!(
        aborted,
        "cleanup's abort must win the CAS while the lease is expired"
    );

    let finalize_audit = AuditEntry::success(
        tenant_id,
        "user",
        ctx.subject_id(),
        Some(ticket.file_id),
        AuditOperation::FinalizeVersion,
        serde_json::json!({"version_id": plan.version_id, "upload_id": plan.upload_id}),
    );
    let session_audit = AuditEntry::success(
        tenant_id,
        "user",
        ctx.subject_id(),
        Some(ticket.file_id),
        AuditOperation::MultipartComplete,
        serde_json::json!({"upload_id": plan.upload_id}),
    );
    let err = store
        .finalize_multipart_version(
            ticket.file_id,
            None,
            Some("application/octet-stream".to_owned()),
            finalize_audit,
            None,
            MultipartFinishSnapshot {
                upload_id: plan.upload_id,
                version_id: plan.version_id,
                size: 10 * 1024 * 1024,
                content_hash: vec![0u8; 32],
                hash_mode: HashMode::WholeSha256,
                part_count: None,
                session_audit,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict once the session is aborted, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(
        version.status,
        VersionStatus::Pending,
        "the version must stay pending -- the finalize must have rolled back entirely"
    );

    let file = store
        .require_file(&AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        file.content_id.is_none(),
        "content_id must remain unbound -- the rejected finalize must not have auto-bound"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session row must still exist");
    assert_eq!(
        session_after.state,
        MultipartUploadState::Aborted,
        "the session must remain aborted -- the rejected finalize must not have resurrected it"
    );
}

#[allow(clippy::type_complexity)]
async fn build_redesign_env() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<dyn MultipartStore>,
    Arc<dyn StorageBackend>,
    Store,
    SecurityContext,
) {
    let s = common::build_stack(
        "mp-test",
        Arc::new(Issuer::generate(3600).expect("issuer")),
        None,
    )
    .await;
    (
        s.svc,
        s.msvc,
        s.multipart_store,
        s.backend,
        s.store,
        ctx(Uuid::now_v7()),
    )
}

#[tokio::test]
async fn auto_bind_complete_binds_and_returns_etag() {
    let (svc, msvc, multipart_store, backend, store, ctx) = build_redesign_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, file_id, "application/octet-stream", 13, None, true)
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    assert!(
        session.auto_bind,
        "merged-create session must record auto_bind"
    );
    put_part(
        &multipart_store,
        &backend,
        &plan,
        file_id,
        1,
        Bytes::from_static(b"Hello, World!"),
    )
    .await;

    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(completed.bind_state, BindState::Bound);
    assert!(
        completed.etag.is_some(),
        "bound complete must carry the new ETag"
    );
    assert_eq!(completed.current_etag, None);

    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
        .await
        .unwrap()
        .expect("file");
    assert_eq!(
        file.content_id,
        Some(completed.version_id),
        "complete must have bound the version \u{2014} no separate bind call"
    );
}

/// A one-part (5 byte) session with its part already uploaded, on a fresh stack.
async fn ready_single_part_session(
    auto_bind: bool,
) -> (
    Arc<MultipartService>,
    Arc<dyn MultipartStore>,
    Store,
    SecurityContext,
    Uuid,
    MultipartPlan,
) {
    let (svc, msvc, multipart_store, backend, store, ctx) = build_redesign_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            5,
            None,
            auto_bind,
        )
        .await
        .unwrap();
    put_part(
        &multipart_store,
        &backend,
        &plan,
        file_id,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    (msvc, multipart_store, store, ctx, file_id, plan)
}

/// Plants a completion lease held by `owner` that expires `secs_from_now` seconds from now
/// (negative = already expired: a completer that died mid-assembly).
async fn plant_lease(
    multipart_store: &Arc<dyn MultipartStore>,
    upload_id: Uuid,
    owner: &str,
    secs_from_now: i64,
) {
    let now = time::OffsetDateTime::now_utc();
    let acquired = multipart_store
        .acquire_multipart_complete_lease(
            upload_id,
            owner,
            now + time::Duration::seconds(secs_from_now),
            now,
        )
        .await
        .unwrap();
    assert!(acquired);
}

async fn bound_content_id(store: &Store, file_id: Uuid) -> Option<Uuid> {
    store
        .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
        .await
        .unwrap()
        .expect("file")
        .content_id
}

#[tokio::test]
async fn manual_session_complete_does_not_bind() {
    let (msvc, _ms, store, ctx, file_id, plan) = ready_single_part_session(false).await;
    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(completed.bind_state, BindState::Manual);
    assert_eq!(completed.etag, None);
    assert_eq!(
        bound_content_id(&store, file_id).await,
        None,
        "manual complete must not bind"
    );
}

/// A `complete` racing another caller's LIVE completion lease answers `Completing` (HTTP 202 at the
/// REST layer) — poll by re-issuing.
#[tokio::test]
async fn complete_while_lease_held_returns_completing() {
    let (msvc, ms, _store, ctx, file_id, plan) = ready_single_part_session(true).await;
    plant_lease(&ms, plan.upload_id, "other-completer", 120).await;

    match msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
    {
        MultipartCompleteOutcome::Completing { retry_after_secs } => {
            assert!(retry_after_secs > 0);
        }
        MultipartCompleteOutcome::Completed(_) => {
            panic!("must answer Completing while another lease is live")
        }
    }
}

/// Takeover: the previous completer died mid-assembly (state stuck in `completing`, lease expired).
#[tokio::test]
async fn complete_takes_over_expired_lease_and_finishes() {
    let (msvc, ms, store, ctx, file_id, plan) = ready_single_part_session(true).await;
    plant_lease(&ms, plan.upload_id, "dead-completer", -5).await;

    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("takeover after an expired lease must succeed")
        .unwrap_completed();
    assert_eq!(completed.bind_state, BindState::Bound);
    assert_eq!(
        bound_content_id(&store, file_id).await,
        Some(completed.version_id)
    );
    let version = store
        .get_version(file_id, completed.version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Available);
}

/// Same setup as `complete_takes_over_expired_lease_and_finishes`, but the session itself expired.
#[tokio::test]
async fn complete_on_expired_completing_session_returns_expired_not_completing() {
    let (msvc, ms, store, ctx, file_id, plan) = ready_single_part_session(true).await;
    plant_lease(&ms, plan.upload_id, "dead-completer", -5).await;
    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::seconds(1),
        )
        .await
        .unwrap();

    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect_err("an expired completing session must never be retried into Completing");
    match err {
        DomainError::MultipartUploadNotInProgress { state, .. } => {
            assert_eq!(
                state, "expired",
                "must report the session as expired, not its raw state"
            );
        }
        other => panic!("expected MultipartUploadNotInProgress(\"expired\"), got {other:?}"),
    }
}

#[tokio::test]
async fn resume_missing_part_then_complete() {
    let (svc, msvc, multipart_store, backend, store, ctx) = build_redesign_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();
    // 6 MiB + 5 bytes at a 5 MiB min part size → exactly 2 parts...
    let part = 5 * 1024 * 1024u64;
    let declared = part + 5;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared,
            Some(part),
            true,
        )
        .await
        .unwrap();
    assert_eq!(plan.parts.len(), 2, "plan must have 2 parts");
    let body: Vec<u8> = (0..declared)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();

    // Only part 1 lands.
    put_part(
        &multipart_store,
        &backend,
        &plan,
        file_id,
        1,
        Bytes::copy_from_slice(&body[..usize::try_from(part).unwrap()]),
    )
    .await;

    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::MultipartPartsMissing { .. }));

    let status = msvc
        .introspect_multipart_upload(&ctx, file_id, plan.upload_id)
        .await
        .unwrap();
    assert_eq!(status.received.len(), 1);
    assert_eq!(status.missing.len(), 1);
    assert_eq!(status.missing[0].part_number, 2);
    assert!(
        status.missing[0].upload_url.is_some(),
        "resume URL expected"
    );

    put_part(
        &multipart_store,
        &backend,
        &plan,
        file_id,
        2,
        Bytes::copy_from_slice(&body[usize::try_from(part).unwrap()..]),
    )
    .await;
    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(completed.bind_state, BindState::Bound);
    assert_eq!(completed.size, i64::try_from(declared).unwrap());
    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
        .await
        .unwrap()
        .expect("file");
    assert_eq!(file.content_id, Some(completed.version_id));
}

/// Two or more parts finalize as `multipart-composite-sha256`: root over the part manifest, which
/// is persisted and returned (a one-part plan is plain `whole-sha256`, see
/// `complete_returns_version_size_and_composite_hash`).
#[tokio::test]
async fn complete_two_parts_returns_composite_root_and_manifest() {
    let (svc, msvc, multipart_store, backend, store, ctx) = build_redesign_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();
    let part = 5 * 1024 * 1024u64;
    let declared = part + 5;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared,
            Some(part),
            false,
        )
        .await
        .unwrap();
    assert_eq!(plan.parts.len(), 2);
    let body: Vec<u8> = (0..declared)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let split = usize::try_from(part).unwrap();
    for (n, chunk) in [(1u32, &body[..split]), (2, &body[split..])] {
        put_part(
            &multipart_store,
            &backend,
            &plan,
            file_id,
            n,
            Bytes::copy_from_slice(chunk),
        )
        .await;
    }

    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    let expected = Manifest::new(vec![
        ManifestEntry {
            offset: 0,
            digest: hash::digest_to_array(hash::sha256(&body[..split])),
        },
        ManifestEntry {
            offset: part,
            digest: hash::digest_to_array(hash::sha256(&body[split..])),
        },
    ])
    .unwrap();
    assert_eq!(completed.hash_mode, HashMode::MultipartCompositeSha256);
    assert_eq!(completed.part_count, 2);
    assert_eq!(completed.content_hash, expected.root().to_vec());
    assert_eq!(completed.manifest, Some(expected.to_wire_string()));
    assert_eq!(
        store.get_version_manifest(plan.version_id).await.unwrap(),
        Some(expected.to_wire_string()),
        "the manifest row is persisted with the version"
    );
}
