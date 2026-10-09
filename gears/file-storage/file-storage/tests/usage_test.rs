//! Usage accounting: bytes credited on finalize, debited on delete; a lifecycle nets to zero.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::etag;
use file_storage::domain::multipart::MultipartPlan;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{AgeRetention, RetentionRuleBody, RetentionScope};
use file_storage::domain::ports::{CleanupStore, DataPlanePort, MultipartStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::content::hash;
use file_storage::infra::external_clients::{UsageDelta, UsageReporter};
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.usage_test.file.type.v1~");

/// Capturing fake `UsageReporter`; records deltas in call order.
#[derive(Default)]
struct FakeUsageReporter {
    deltas: tokio::sync::Mutex<Vec<UsageDelta>>,
}

impl FakeUsageReporter {
    async fn snapshot(&self) -> Vec<UsageDelta> {
        self.deltas.lock().await.clone()
    }
}

#[async_trait]
impl UsageReporter for FakeUsageReporter {
    async fn report(&self, delta: UsageDelta) {
        self.deltas.lock().await.push(delta);
    }
}

/// `report_usage` is fire-and-forget, so poll briefly (bounded ~1s) for a captured delta.
async fn wait_for_reports(fake: &FakeUsageReporter, at_least: usize) -> Vec<UsageDelta> {
    for _ in 0..200 {
        let snap = fake.snapshot().await;
        if snap.len() >= at_least {
            return snap;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    fake.snapshot().await
}

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-usage-test-{}.db", Uuid::now_v7().simple()));
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

/// Wires `FileService`, `MultipartService`, `DataPlaneService`, `Store` and `CleanupEngine`
/// to one fake reporter.
async fn build_all(
    fake: Arc<FakeUsageReporter>,
) -> (
    Arc<FileService>,
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());

    let reporter: Arc<dyn UsageReporter> = fake;

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        Some(Arc::clone(&reporter)),
    ));
    let msvc = Arc::new(
        MultipartService::new(
            multipart_store,
            backends.clone(),
            Arc::clone(&authorizer),
            None,
            issuer,
            "http://sidecar.test".to_owned(),
            3600,
        )
        .with_usage_reporter(Some(Arc::clone(&reporter))),
    );
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let engine = CleanupEngine::new(
        sweep_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 86400,
        },
    )
    .with_usage_reporter(Some(reporter));

    (svc, msvc, dp, store, engine, backend)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file(owner: Uuid) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: owner,
        name: "usage.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "text/plain".to_owned(),
        custom_metadata: vec![],
    }
}

/// Drive initiate -> one part -> complete, writing the part via the native backend API and
/// persisting it with `upsert_multipart_part`. Returns `(upload_id, version_id, size)`.
async fn drive_multipart_upload(
    msvc: &MultipartService,
    multipart_store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
    file_id: Uuid,
    data: Bytes,
) -> (Uuid, Uuid, i64) {
    let declared_size = data.len() as u64;
    let plan: MultipartPlan = msvc
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
    assert_eq!(plan.parts.len(), 1, "test payload must fit in one part");

    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{file_id}/{}", plan.version_id);

    let (backend_etag, part_hash) = backend
        .upload_part(&backend_path, &session.backend_upload_handle, 1, 0, data)
        .await
        .expect("backend upload_part");

    multipart_store
        .upsert_multipart_part(
            plan.upload_id,
            1,
            &backend_etag,
            part_hash,
            i64::try_from(declared_size).unwrap(),
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    msvc.complete_multipart_upload(ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();

    (
        plan.upload_id,
        plan.version_id,
        i64::try_from(declared_size).unwrap(),
    )
}

#[tokio::test]
async fn finalize_reports_positive_byte_delta() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, _msvc, dp, _store, _engine, _backend) = build_all(Arc::clone(&fake)).await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(owner), None).await.unwrap();

    // `create_file` reports `+1 file / 0 bytes`, so finalize is the only byte credit.
    let after_create = wait_for_reports(&fake, 1).await;
    assert_eq!(after_create.len(), 1);
    assert_eq!(after_create[0].bytes_delta, 0, "create must report 0 bytes");
    assert_eq!(after_create[0].file_count_delta, 1);

    let payload = Bytes::from_static(b"hello usage accounting, this is finalize content");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        payload.clone(),
    )
    .await
    .unwrap();

    let after_finalize = wait_for_reports(&fake, 2).await;
    assert_eq!(
        after_finalize.len(),
        2,
        "finalize must add exactly one more report"
    );
    let credit = &after_finalize[1];
    assert_eq!(credit.bytes_delta, i64::try_from(payload.len()).unwrap());
    assert_eq!(
        credit.file_count_delta, 0,
        "file count was already credited at create time"
    );
    assert_eq!(credit.tenant_id, tenant);
    assert_eq!(credit.owner_id, owner);
}

/// Same credit via the token-authenticated `finalize_upload_by_token` path (builds `Claims`).
#[tokio::test]
async fn finalize_by_token_reports_positive_byte_delta() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, _msvc, _dp, _store, _engine, backend) = build_all(Arc::clone(&fake)).await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(owner), None).await.unwrap();
    wait_for_reports(&fake, 1).await;

    let payload = Bytes::from_static(b"token-path payload bytes");
    let backend_path = format!("/{}/{}", ticket.file_id, ticket.version_id);
    backend
        .put(&backend_path, payload.clone())
        .await
        .expect("backend put");
    let size = i64::try_from(payload.len()).unwrap();
    let digest = hash::sha256(&payload);

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
    };
    svc.finalize_upload_by_token(&claims, size, digest)
        .await
        .unwrap();

    let deltas = wait_for_reports(&fake, 2).await;
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[1].bytes_delta, size);
    assert_eq!(deltas[1].file_count_delta, 0);
    assert_eq!(deltas[1].tenant_id, tenant);
    assert_eq!(deltas[1].owner_id, owner);
}

#[tokio::test]
async fn multipart_complete_reports_byte_delta() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, msvc, _dp, store, _engine, backend) = build_all(Arc::clone(&fake)).await;
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(owner), None).await.unwrap();
    wait_for_reports(&fake, 1).await;

    let data = Bytes::from_static(b"multipart assembled payload bytes for usage credit");
    let (_upload_id, _version_id, size) = drive_multipart_upload(
        &msvc,
        &multipart_store,
        &backend,
        &ctx,
        ticket.file_id,
        data,
    )
    .await;

    let deltas = wait_for_reports(&fake, 2).await;
    assert_eq!(
        deltas.len(),
        2,
        "multipart complete must add exactly one more report"
    );
    let credit = &deltas[1];
    assert_eq!(credit.bytes_delta, size);
    assert_eq!(
        credit.file_count_delta, 0,
        "the file itself was already credited +1 at create_file time"
    );
    assert_eq!(credit.tenant_id, tenant);
    assert_eq!(credit.owner_id, owner);
}

#[tokio::test]
async fn delete_version_reports_negative_byte_delta() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, _msvc, dp, _store, _engine, _backend) = build_all(Arc::clone(&fake)).await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(owner), None).await.unwrap();
    let v1_payload = Bytes::from_static(b"version one payload");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        v1_payload.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Second version bound as current, so v1 is deletable.
    let v2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        v2.file_id,
        v2.version_id,
        "text/plain",
        Bytes::from_static(b"version two payload, a bit longer"),
    )
    .await
    .unwrap();
    let file_before_rebind = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    let if_match = etag::etag_for(&file_before_rebind);
    svc.bind(&ctx, ticket.file_id, v2.version_id, if_match.as_deref())
        .await
        .unwrap();

    // Reports: create, finalize v1, finalize v2 = 3 (`bind` reports nothing).
    wait_for_reports(&fake, 3).await;

    svc.delete_version(&ctx, ticket.file_id, ticket.version_id)
        .await
        .unwrap();

    let deltas = wait_for_reports(&fake, 4).await;
    assert_eq!(deltas.len(), 4, "delete_version must add exactly one debit");
    let debit = &deltas[3];
    assert_eq!(debit.bytes_delta, -i64::try_from(v1_payload.len()).unwrap());
    assert_eq!(
        debit.file_count_delta, 0,
        "the file itself still exists -- only the version's bytes are debited"
    );
    assert_eq!(debit.tenant_id, tenant);
    assert_eq!(debit.owner_id, owner);
}

#[tokio::test]
async fn sweep_reports_deltas_for_deleted_files() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, _msvc, dp, store, engine, _backend) = build_all(Arc::clone(&fake)).await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(owner), None).await.unwrap();
    let payload = Bytes::from_static(b"retention-swept payload");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        payload.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    wait_for_reports(&fake, 2).await;

    // Tenant-wide age rule with max_age_days = 0 expires immediately.
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

    let result = engine.run_sweep().await;
    assert!(
        result.retention_expired_deleted >= 1,
        "sweep must delete the retention-expired file"
    );

    let deltas = wait_for_reports(&fake, 3).await;
    assert_eq!(
        deltas.len(),
        3,
        "the sweep-driven delete must add exactly one debit"
    );
    let debit = &deltas[2];
    assert_eq!(debit.bytes_delta, -i64::try_from(payload.len()).unwrap());
    assert_eq!(debit.file_count_delta, -1);
    assert_eq!(debit.tenant_id, tenant);
    assert_eq!(debit.owner_id, owner);
}

#[tokio::test]
async fn usage_deltas_sum_to_zero_over_create_upload_delete() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, _msvc, dp, _store, _engine, _backend) = build_all(Arc::clone(&fake)).await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(Uuid::now_v7()), None)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"a full create -> upload -> delete lifecycle"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let file = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    let if_match = etag::etag_for(&file);
    svc.delete_file(&ctx, ticket.file_id, if_match.as_deref())
        .await
        .unwrap();

    let deltas = wait_for_reports(&fake, 3).await;
    assert_eq!(deltas.len(), 3);

    let bytes_sum: i64 = deltas.iter().map(|d| d.bytes_delta).sum();
    let files_sum: i64 = deltas.iter().map(|d| d.file_count_delta).sum();
    assert_eq!(
        bytes_sum, 0,
        "a full create->upload->delete cycle must net to zero bytes, got deltas {deltas:?}"
    );
    assert_eq!(
        files_sum, 0,
        "a full create->upload->delete cycle must net to zero files, got deltas {deltas:?}"
    );
}
