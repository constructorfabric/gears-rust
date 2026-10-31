//! Usage accounting: bytes credited on finalize, debited on delete; a lifecycle nets to zero.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use bytes::Bytes;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::etag;
use file_storage::domain::policy::{AgeRetention, RetentionRuleBody, RetentionScope};
use file_storage::infra::content::hash;

mod common;
use common::{FakeUsageReporter, Stack, StackBuilder, ctx};

/// Finalize credits the version's bytes, through the direct `finalize_upload` path (data plane)
/// and the token-authenticated `finalize_upload_by_token` path (builds `Claims`).
#[tokio::test]
async fn finalize_reports_positive_byte_delta() {
    for by_token in [false, true] {
        let fake = FakeUsageReporter::new();
        let Stack {
            svc, dp, backend, ..
        } = StackBuilder::new("usage-test")
            .usage_reporter(fake.clone())
            .build()
            .await;
        let tenant = Uuid::now_v7();
        let owner = Uuid::now_v7();
        let ctx = ctx(tenant);

        let ticket = svc
            .create_file(
                &ctx,
                common::new_file_for(owner, "usage.bin", "text/plain"),
                None,
                false,
            )
            .await
            .unwrap();

        // `create_file` reports `+1 file / 0 bytes`, so finalize is the only byte credit.
        let after_create = fake.reports(1).await;
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

        let after_finalize = fake.reports(2).await;
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
    let fake = FakeUsageReporter::new();
    let Stack {
        svc,
        msvc,
        multipart_store,
        backend,
        ..
    } = StackBuilder::new("usage-test")
        .usage_reporter(fake.clone())
        .build()
        .await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(
            &ctx,
            common::new_file_for(owner, "usage.bin", "text/plain"),
            None,
            false,
        )
        .await
        .unwrap();
    fake.reports(1).await;

    let data: &'static [u8] = b"multipart assembled payload bytes for usage credit";
    let size = i64::try_from(data.len()).unwrap();
    let plan = common::session_with_part(
        &msvc,
        &multipart_store,
        &backend,
        &ctx,
        ticket.file_id,
        data,
        false,
    )
    .await;
    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();

    let deltas = fake.reports(2).await;
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

/// A file whose first version (`V1_PAYLOAD`) is bound, then a second one bound over it, so `v1` is
/// deletable. Waits for the three reports so far: create, finalize v1, finalize v2 (`bind`
/// reports nothing).
struct TwoVersions {
    fake: Arc<FakeUsageReporter>,
    stack: Stack,
    ctx: SecurityContext,
    tenant: Uuid,
    owner: Uuid,
    file_id: Uuid,
    v1: Uuid,
}

const V1_PAYLOAD: &[u8] = b"version one payload";
const V2_PAYLOAD: &[u8] = b"version two payload, a bit longer";

async fn file_with_two_bound_versions() -> TwoVersions {
    let fake = FakeUsageReporter::new();
    let stack = StackBuilder::new("usage-test")
        .usage_reporter(fake.clone())
        .build()
        .await;
    let (tenant, owner) = (Uuid::now_v7(), Uuid::now_v7());
    let ctx = ctx(tenant);
    let (svc, dp) = (&stack.svc, &stack.dp);

    let ticket = svc
        .create_file(
            &ctx,
            common::new_file_for(owner, "usage.bin", "text/plain"),
            None,
            false,
        )
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(V1_PAYLOAD),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let v2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        v2.file_id,
        v2.version_id,
        "text/plain",
        Bytes::from_static(V2_PAYLOAD),
    )
    .await
    .unwrap();
    let file = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    svc.bind(
        &ctx,
        ticket.file_id,
        v2.version_id,
        etag::etag_for(&file).as_deref(),
    )
    .await
    .unwrap();
    fake.reports(3).await;

    TwoVersions {
        fake,
        stack,
        ctx,
        tenant,
        owner,
        file_id: ticket.file_id,
        v1: ticket.version_id,
    }
}

#[tokio::test]
async fn delete_version_reports_negative_byte_delta() {
    let t = file_with_two_bound_versions().await;

    t.stack
        .svc
        .delete_version(&t.ctx, t.file_id, t.v1)
        .await
        .unwrap();

    let deltas = t.fake.reports(4).await;
    assert_eq!(deltas.len(), 4, "delete_version must add exactly one debit");
    let debit = &deltas[3];
    assert_eq!(debit.bytes_delta, -i64::try_from(V1_PAYLOAD.len()).unwrap());
    assert_eq!(
        debit.file_count_delta, 0,
        "the file itself still exists: only the version's bytes are debited"
    );
    assert_eq!(debit.tenant_id, t.tenant);
    assert_eq!(debit.owner_id, t.owner);
}

/// Deleting the file debits everything left, so create -> uploads -> delete nets to zero.
#[tokio::test]
async fn delete_file_debits_the_rest_so_a_lifecycle_nets_to_zero() {
    let t = file_with_two_bound_versions().await;
    let svc = &t.stack.svc;

    let file = svc.get_file(&t.ctx, t.file_id).await.unwrap();
    svc.delete_file(&t.ctx, t.file_id, etag::etag_for(&file).as_deref())
        .await
        .unwrap();

    let deltas = t.fake.reports(4).await;
    assert_eq!(deltas.len(), 4, "delete_file must add exactly one debit");
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
    let fake = FakeUsageReporter::new();
    let Stack {
        svc,
        dp,
        store,
        engine,
        ..
    } = StackBuilder::new("usage-test")
        .usage_reporter(fake.clone())
        .build()
        .await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(
            &ctx,
            common::new_file_for(owner, "usage.bin", "text/plain"),
            None,
            false,
        )
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

    fake.reports(2).await;

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

    let deltas = fake.reports(3).await;
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
