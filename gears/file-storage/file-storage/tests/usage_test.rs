//! Usage accounting: bytes credited on finalize, debited on delete; a lifecycle nets to zero.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::cleanup::CleanupEngine;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::etag;
use file_storage::domain::multipart::MultipartPlan;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{AgeRetention, RetentionRuleBody, RetentionScope};
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::FileService;
use file_storage::infra::backend::StorageBackend;
use file_storage::infra::content::hash;
use file_storage::infra::external_clients::{UsageDelta, UsageReporter};
use file_storage::infra::storage::Store;
use file_storage_sdk::NewFile;

mod common;
use common::{StackBuilder, ctx};

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
    let s = StackBuilder::new("usage-test")
        .usage_reporter(fake)
        .build()
        .await;
    (s.svc, s.msvc, s.dp, s.store, s.engine, s.backend)
}

fn new_file(owner: Uuid) -> NewFile {
    NewFile {
        gts_file_type: GTS.to_owned(),
        ..common::new_file_for(owner, "usage.bin", "text/plain")
    }
}

/// Drive initiate -> one part -> complete. Returns the part's size.
async fn drive_multipart_upload(
    msvc: &Arc<MultipartService>,
    multipart_store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
    file_id: Uuid,
    data: Bytes,
) -> i64 {
    let size = i64::try_from(data.len()).unwrap();
    let plan: MultipartPlan = msvc
        .initiate_multipart_upload(
            ctx,
            file_id,
            "application/octet-stream",
            data.len() as u64,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(plan.parts.len(), 1, "test payload must fit in one part");

    common::put_part(multipart_store, backend, &plan, file_id, 1, data).await;
    msvc.complete_multipart_upload(ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();
    size
}

/// Finalize credits the version's bytes, through the direct `finalize_upload` path (data plane)
/// and the token-authenticated `finalize_upload_by_token` path (builds `Claims`).
#[tokio::test]
async fn finalize_reports_positive_byte_delta() {
    for by_token in [false, true] {
        let fake = Arc::new(FakeUsageReporter::default());
        let (svc, _msvc, dp, _store, _engine, backend) = build_all(Arc::clone(&fake)).await;
        let tenant = Uuid::now_v7();
        let owner = Uuid::now_v7();
        let ctx = ctx(tenant);

        let ticket = svc
            .create_file(&ctx, new_file(owner), None, false)
            .await
            .unwrap();

        // `create_file` reports `+1 file / 0 bytes`, so finalize is the only byte credit.
        let after_create = wait_for_reports(&fake, 1).await;
        assert_eq!(after_create.len(), 1, "by_token={by_token}");
        assert_eq!(after_create[0].bytes_delta, 0, "create must report 0 bytes");
        assert_eq!(after_create[0].file_count_delta, 1);

        let payload = Bytes::from_static(b"hello usage accounting, this is finalize content");
        if by_token {
            let backend_path = common::backend_path(ticket.file_id, ticket.version_id);
            backend
                .put(&backend_path, payload.clone())
                .await
                .expect("backend put");
            let claims = common::put_claims(ticket.file_id, ticket.version_id, backend_path, false);
            svc.finalize_upload_by_token(
                &claims,
                i64::try_from(payload.len()).unwrap(),
                hash::sha256(&payload),
            )
            .await
            .unwrap();
        } else {
            dp.put_content(
                &ctx,
                ticket.file_id,
                ticket.version_id,
                "text/plain",
                payload.clone(),
            )
            .await
            .unwrap();
        }

        let after_finalize = wait_for_reports(&fake, 2).await;
        assert_eq!(
            after_finalize.len(),
            2,
            "by_token={by_token}: finalize must add exactly one more report"
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
}

#[tokio::test]
async fn multipart_complete_reports_byte_delta() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, msvc, _dp, store, _engine, backend) = build_all(Arc::clone(&fake)).await;
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(owner), None, false)
        .await
        .unwrap();
    wait_for_reports(&fake, 1).await;

    let data = Bytes::from_static(b"multipart assembled payload bytes for usage credit");
    let size = drive_multipart_upload(
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

    let ticket = svc
        .create_file(&ctx, new_file(owner), None, false)
        .await
        .unwrap();
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

    // Deleting the file debits the rest: the whole create -> upload -> delete lifecycle nets to zero.
    let file = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    svc.delete_file(&ctx, ticket.file_id, etag::etag_for(&file).as_deref())
        .await
        .unwrap();
    let deltas = wait_for_reports(&fake, 5).await;
    assert_eq!(deltas.len(), 5, "delete_file must add exactly one debit");
    let bytes_sum: i64 = deltas.iter().map(|d| d.bytes_delta).sum();
    let files_sum: i64 = deltas.iter().map(|d| d.file_count_delta).sum();
    assert_eq!(
        bytes_sum, 0,
        "bytes must net to zero, got deltas {deltas:?}"
    );
    assert_eq!(
        files_sum, 0,
        "files must net to zero, got deltas {deltas:?}"
    );
}

#[tokio::test]
async fn sweep_reports_deltas_for_deleted_files() {
    let fake = Arc::new(FakeUsageReporter::default());
    let (svc, _msvc, dp, store, engine, _backend) = build_all(Arc::clone(&fake)).await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(owner), None, false)
        .await
        .unwrap();
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
