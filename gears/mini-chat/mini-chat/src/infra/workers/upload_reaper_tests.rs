use std::sync::Arc;

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{ScanOutcome, UploadReaperDeps, scan_and_reap};
use crate::config::UploadReaperConfig;
use crate::domain::service::test_helpers::{
    InsertTestAttachmentParams, RecordingOutboxEnqueuer, inmem_db, insert_chat_for_user,
    insert_test_attachment, mock_db_provider,
};
use crate::infra::db::entity::attachment::{
    AttachmentStatus, CleanupStatus, Column, Entity, Model as AttachmentModel,
};

async fn seed(
    db: &Arc<crate::domain::service::DbProvider>,
    status: AttachmentStatus,
    provider_file_id: Option<&str>,
    age_secs: i64,
) -> (Uuid, Uuid) {
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat_for_user(db, tenant_id, chat_id, Uuid::new_v4()).await;
    let mut params = InsertTestAttachmentParams::ready_document(tenant_id, chat_id);
    params.status = status;
    params.provider_file_id = provider_file_id.map(str::to_owned);
    let id = insert_test_attachment(db, params).await;
    let conn = db.conn().unwrap();
    Entity::update_many()
        .col_expr(
            Column::UpdatedAt,
            Expr::value(OffsetDateTime::now_utc() - time::Duration::seconds(age_secs)),
        )
        .filter(Column::Id.eq(id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .unwrap();
    (id, chat_id)
}

async fn load(db: &Arc<crate::domain::service::DbProvider>, id: Uuid) -> AttachmentModel {
    let conn = db.conn().unwrap();
    Entity::find()
        .filter(Column::Id.eq(id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&conn)
        .await
        .unwrap()
        .unwrap()
}

fn config() -> UploadReaperConfig {
    UploadReaperConfig {
        enabled: true,
        scan_interval_secs: 60,
        stale_after_secs: 300,
    }
}

#[tokio::test]
async fn stale_uploaded_row_is_failed_and_cleanup_enqueued() {
    let db = mock_db_provider(inmem_db().await);
    let (id, chat_id) = seed(&db, AttachmentStatus::Uploaded, Some("file-stale"), 600).await;
    let outbox = Arc::new(RecordingOutboxEnqueuer::new());
    let deps = UploadReaperDeps {
        db: Arc::clone(&db),
        outbox_enqueuer: Arc::clone(&outbox) as _,
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };

    let result = scan_and_reap(&deps, &config(), &CancellationToken::new()).await;
    assert_eq!(result, ScanOutcome::Done);

    let row = load(&db, id).await;
    assert_eq!(row.status, AttachmentStatus::Failed);
    assert_eq!(row.error_code.as_deref(), Some("upload_abandoned"));
    assert_eq!(row.cleanup_status, Some(CleanupStatus::Pending));
    assert!(row.deleted_at.is_none(), "the row stays visible as failed");

    // A failed row stops counting against the per-chat document limit.
    {
        use crate::domain::repos::AttachmentRepository as _;
        let conn = db.conn().unwrap();
        let docs = crate::infra::db::repo::attachment_repo::AttachmentRepository
            .count_documents(&conn, &AccessScope::allow_all(), chat_id)
            .await
            .unwrap();
        assert_eq!(docs, 0);
    }

    let events = outbox.cleanup_events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].attachment_id, id);
    assert_eq!(events[0].chat_id, chat_id);
    assert_eq!(events[0].provider_file_id.as_deref(), Some("file-stale"));
}

#[tokio::test]
async fn stale_pending_row_without_file_is_failed_without_cleanup() {
    let db = mock_db_provider(inmem_db().await);
    let (id, _) = seed(&db, AttachmentStatus::Pending, None, 600).await;
    let outbox = Arc::new(RecordingOutboxEnqueuer::new());
    let deps = UploadReaperDeps {
        db: Arc::clone(&db),
        outbox_enqueuer: Arc::clone(&outbox) as _,
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };

    assert_eq!(
        scan_and_reap(&deps, &config(), &CancellationToken::new()).await,
        ScanOutcome::Done
    );

    let row = load(&db, id).await;
    assert_eq!(row.status, AttachmentStatus::Failed);
    assert_eq!(row.error_code.as_deref(), Some("upload_abandoned"));
    assert_eq!(row.cleanup_status, None);
    assert!(outbox.cleanup_events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recent_and_finished_rows_are_left_alone() {
    let db = mock_db_provider(inmem_db().await);
    let (recent, _) = seed(&db, AttachmentStatus::Uploaded, Some("file-recent"), 10).await;
    let (ready, _) = seed(&db, AttachmentStatus::Ready, Some("file-ready"), 600).await;
    let outbox = Arc::new(RecordingOutboxEnqueuer::new());
    let deps = UploadReaperDeps {
        db: Arc::clone(&db),
        outbox_enqueuer: Arc::clone(&outbox) as _,
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };

    assert_eq!(
        scan_and_reap(&deps, &config(), &CancellationToken::new()).await,
        ScanOutcome::Done
    );

    assert_eq!(load(&db, recent).await.status, AttachmentStatus::Uploaded);
    assert_eq!(load(&db, ready).await.status, AttachmentStatus::Ready);
    assert!(outbox.cleanup_events.lock().unwrap().is_empty());
}

#[test]
fn config_rejects_stale_window_below_gateway_timeout() {
    let mut cfg = config();
    cfg.validate().unwrap();
    cfg.stale_after_secs = 30;
    assert!(cfg.validate().is_err());
    cfg.stale_after_secs = 300;
    cfg.scan_interval_secs = 0;
    assert!(cfg.validate().is_err());
    cfg.scan_interval_secs = crate::config::background::MAX_SCAN_INTERVAL_SECS + 1;
    assert!(cfg.validate().is_err());
    cfg.scan_interval_secs = crate::config::background::MAX_SCAN_INTERVAL_SECS;
    cfg.validate().unwrap();
}

#[test]
fn config_stale_after_bounds() {
    let with = |secs| UploadReaperConfig {
        stale_after_secs: secs,
        ..config()
    };
    for secs in [59, 86_401] {
        let err = with(secs).validate().unwrap_err();
        assert!(err.contains("stale_after_secs"), "{err}");
    }
    for secs in [60, 86_400] {
        with(secs).validate().unwrap();
    }
}

/// A row refreshed between the scan and the CAS (the background indexing
/// heartbeat) is no longer stale and is not abandoned.
#[tokio::test]
async fn cas_abandon_upload_skips_row_touched_after_scan() {
    use crate::domain::repos::AttachmentRepository as _;
    let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
    let db = mock_db_provider(inmem_db().await);
    let (id, _) = seed(&db, AttachmentStatus::Uploaded, Some("file-touched"), 600).await;
    let tenant_id = load(&db, id).await.tenant_id;
    let conn = db.conn().unwrap();
    let cutoff = OffsetDateTime::now_utc() - time::Duration::seconds(300);

    let stale = repo.find_stale_uploads(&conn, cutoff, 10).await.unwrap();
    assert_eq!(stale.iter().map(|r| r.id).collect::<Vec<_>>(), [id]);

    let touched = repo
        .touch_uploaded(&conn, &AccessScope::for_tenant(tenant_id), id)
        .await
        .unwrap();
    assert_eq!(touched, 1);

    let affected = repo
        .cas_abandon_upload(
            &conn,
            tenant_id,
            id,
            AttachmentStatus::Uploaded,
            cutoff,
            true,
        )
        .await
        .unwrap();
    assert_eq!(affected, 0);
    let row = load(&db, id).await;
    assert_eq!(row.status, AttachmentStatus::Uploaded);
    assert!(row.error_code.is_none(), "{:?}", row.error_code);
    assert_eq!(row.cleanup_status, None);
}

/// The CAS is scoped to the given tenant: another tenant id matches nothing.
#[tokio::test]
async fn cas_abandon_upload_with_wrong_tenant_affects_nothing() {
    let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
    let db = mock_db_provider(inmem_db().await);
    let (id, _) = seed(&db, AttachmentStatus::Uploaded, Some("file-tenant"), 600).await;
    let tenant_id = load(&db, id).await.tenant_id;
    let conn = db.conn().unwrap();
    let cutoff = OffsetDateTime::now_utc() - time::Duration::seconds(300);

    let affected = repo
        .cas_abandon_upload(
            &conn,
            Uuid::new_v4(),
            id,
            AttachmentStatus::Uploaded,
            cutoff,
            true,
        )
        .await
        .unwrap();
    assert_eq!(affected, 0);
    assert_eq!(load(&db, id).await.status, AttachmentStatus::Uploaded);

    // The row's own tenant does match.
    let affected = repo
        .cas_abandon_upload(
            &conn,
            tenant_id,
            id,
            AttachmentStatus::Uploaded,
            cutoff,
            true,
        )
        .await
        .unwrap();
    assert_eq!(affected, 1);
    let row = load(&db, id).await;
    assert_eq!(row.status, AttachmentStatus::Failed);
    assert_eq!(row.error_code.as_deref(), Some("upload_abandoned"));
    assert_eq!(row.cleanup_status, Some(CleanupStatus::Pending));
}

/// A row whose chat was deleted already has `cleanup_status` set by chat
/// deletion, which owns its provider cleanup; the reaper leaves it alone.
#[tokio::test]
async fn row_owned_by_chat_cleanup_is_left_alone() {
    let db = mock_db_provider(inmem_db().await);
    let (id, _) = seed(&db, AttachmentStatus::Uploaded, Some("file-owned"), 600).await;
    let conn = db.conn().unwrap();
    Entity::update_many()
        .col_expr(
            Column::CleanupStatus,
            Expr::value(Some(CleanupStatus::Done)),
        )
        .filter(Column::Id.eq(id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .unwrap();
    let outbox = Arc::new(RecordingOutboxEnqueuer::new());
    let deps = UploadReaperDeps {
        db: Arc::clone(&db),
        outbox_enqueuer: Arc::clone(&outbox) as _,
        metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
    };

    assert_eq!(
        scan_and_reap(&deps, &config(), &CancellationToken::new()).await,
        ScanOutcome::Done
    );

    let row = load(&db, id).await;
    assert_eq!(row.status, AttachmentStatus::Uploaded);
    assert_eq!(row.cleanup_status, Some(CleanupStatus::Done));
    assert!(outbox.cleanup_events.lock().unwrap().is_empty());
}
