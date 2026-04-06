use std::sync::Arc;

use modkit_db::DBProvider;
use modkit_db::odata::LimitCfg;
use modkit_db::secure::secure_insert;
use modkit_security::AccessScope;
use sea_orm::Set;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::repos::{InsertUserMessageParams, MessageRepository as _};
use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};
use crate::infra::db::entity::attachment::{
    ActiveModel as AttAm, AttachmentKind, AttachmentStatus, Entity as AttEntity,
};
use crate::infra::db::entity::message_attachment::{ActiveModel as MaAm, Entity as MaEntity};

use super::MessageRepository;

type Db = Arc<DBProvider<modkit_db::DbError>>;

// ── Helpers ──

fn scope() -> AccessScope {
    AccessScope::allow_all()
}

fn limit_cfg() -> LimitCfg {
    LimitCfg {
        default: 20,
        max: 100,
    }
}

async fn test_db() -> Db {
    mock_db_provider(inmem_db().await)
}

/// Insert a parent chat row (required by FK constraints).
async fn insert_chat(db: &Db, tenant_id: Uuid, chat_id: Uuid) {
    use crate::infra::db::entity::chat::{ActiveModel, Entity as ChatEntity};

    let now = OffsetDateTime::now_utc();
    let am = ActiveModel {
        id: Set(chat_id),
        tenant_id: Set(tenant_id),
        user_id: Set(Uuid::new_v4()),
        model: Set("gpt-5.2".to_owned()),
        title: Set(Some("test".to_owned())),
        is_temporary: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    let conn = db.conn().unwrap();
    secure_insert::<ChatEntity>(am, &scope(), &conn)
        .await
        .expect("insert chat");
}

/// Insert a user message row. Returns the message ID.
async fn insert_user_message(db: &Db, tenant_id: Uuid, chat_id: Uuid) -> Uuid {
    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let msg_id = Uuid::now_v7();
    repo.insert_user_message(
        &conn,
        &scope(),
        InsertUserMessageParams {
            id: msg_id,
            tenant_id,
            chat_id,
            request_id: Uuid::new_v4(),
            content: "hello".to_owned(),
        },
    )
    .await
    .expect("insert user message");
    msg_id
}

/// Insert an attachment row. Returns the attachment ID.
async fn insert_attachment(
    db: &Db,
    tenant_id: Uuid,
    chat_id: Uuid,
    kind: AttachmentKind,
    filename: &str,
    status: AttachmentStatus,
    img_thumbnail: Option<(Vec<u8>, i32, i32)>,
) -> Uuid {
    let now = OffsetDateTime::now_utc();
    let att_id = Uuid::now_v7();
    let (thumb_bytes, thumb_w, thumb_h) = match img_thumbnail {
        Some((bytes, w, h)) => (Some(bytes), Some(w), Some(h)),
        None => (None, None, None),
    };
    let am = AttAm {
        id: Set(att_id),
        tenant_id: Set(tenant_id),
        chat_id: Set(chat_id),
        uploaded_by_user_id: Set(Uuid::new_v4()),
        filename: Set(filename.to_owned()),
        content_type: Set("application/octet-stream".to_owned()),
        size_bytes: Set(1024),
        storage_backend: Set("azure".to_owned()),
        provider_file_id: Set(None),
        status: Set(status),
        error_code: Set(None),
        attachment_kind: Set(kind),
        for_file_search: Set(true),
        for_code_interpreter: Set(false),
        doc_summary: Set(None),
        img_thumbnail: Set(thumb_bytes),
        img_thumbnail_width: Set(thumb_w),
        img_thumbnail_height: Set(thumb_h),
        summary_model: Set(None),
        summary_updated_at: Set(None),
        cleanup_status: Set(None),
        cleanup_attempts: Set(0),
        last_cleanup_error: Set(None),
        cleanup_updated_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    let conn = db.conn().unwrap();
    secure_insert::<AttEntity>(am, &scope(), &conn)
        .await
        .expect("insert attachment");
    att_id
}

/// Insert a soft-deleted attachment row. Returns the attachment ID.
async fn insert_deleted_attachment(db: &Db, tenant_id: Uuid, chat_id: Uuid) -> Uuid {
    let now = OffsetDateTime::now_utc();
    let att_id = Uuid::now_v7();
    let am = AttAm {
        id: Set(att_id),
        tenant_id: Set(tenant_id),
        chat_id: Set(chat_id),
        uploaded_by_user_id: Set(Uuid::new_v4()),
        filename: Set("deleted.pdf".to_owned()),
        content_type: Set("application/pdf".to_owned()),
        size_bytes: Set(512),
        storage_backend: Set("azure".to_owned()),
        provider_file_id: Set(None),
        status: Set(AttachmentStatus::Ready),
        error_code: Set(None),
        attachment_kind: Set(AttachmentKind::Document),
        for_file_search: Set(true),
        for_code_interpreter: Set(false),
        doc_summary: Set(None),
        img_thumbnail: Set(None),
        img_thumbnail_width: Set(None),
        img_thumbnail_height: Set(None),
        summary_model: Set(None),
        summary_updated_at: Set(None),
        cleanup_status: Set(None),
        cleanup_attempts: Set(0),
        last_cleanup_error: Set(None),
        cleanup_updated_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(Some(now)),
    };
    let conn = db.conn().unwrap();
    secure_insert::<AttEntity>(am, &scope(), &conn)
        .await
        .expect("insert deleted attachment");
    att_id
}

/// Link a message to an attachment via `message_attachments`.
async fn link_message_attachment(
    db: &Db,
    tenant_id: Uuid,
    chat_id: Uuid,
    message_id: Uuid,
    attachment_id: Uuid,
) {
    let now = OffsetDateTime::now_utc();
    let am = MaAm {
        tenant_id: Set(tenant_id),
        chat_id: Set(chat_id),
        message_id: Set(message_id),
        attachment_id: Set(attachment_id),
        created_at: Set(now),
    };
    let conn = db.conn().unwrap();
    secure_insert::<MaEntity>(am, &scope(), &conn)
        .await
        .expect("link message attachment");
}

// ════════════════════════════════════════════════════════════════════
// batch_attachment_summaries tests
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn batch_empty_message_ids_returns_empty() {
    let db = test_db().await;
    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(Uuid::new_v4());

    let map = repo
        .batch_attachment_summaries(&conn, &scope, Uuid::new_v4(), &[])
        .await
        .expect("batch empty");

    assert!(map.is_empty());
}

#[tokio::test]
async fn batch_no_attachments_returns_empty_map() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let msg_id = insert_user_message(&db, tenant_id, chat_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let map = repo
        .batch_attachment_summaries(&conn, &scope, chat_id, &[msg_id])
        .await
        .expect("batch no attachments");

    assert!(map.is_empty(), "no links -> empty map");
}

#[tokio::test]
async fn batch_single_message_single_attachment() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let msg_id = insert_user_message(&db, tenant_id, chat_id).await;
    let att_id = insert_attachment(
        &db,
        tenant_id,
        chat_id,
        AttachmentKind::Document,
        "report.pdf",
        AttachmentStatus::Ready,
        None,
    )
    .await;
    link_message_attachment(&db, tenant_id, chat_id, msg_id, att_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let map = repo
        .batch_attachment_summaries(&conn, &scope, chat_id, &[msg_id])
        .await
        .expect("batch");

    assert_eq!(map.len(), 1);
    let summaries = &map[&msg_id];
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].attachment_id, att_id);
    assert_eq!(summaries[0].kind, "document");
    assert_eq!(summaries[0].filename, "report.pdf");
    assert_eq!(summaries[0].status, "ready");
    assert!(summaries[0].img_thumbnail.is_none());
}

#[tokio::test]
async fn batch_multiple_messages_multiple_attachments() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let msg1 = insert_user_message(&db, tenant_id, chat_id).await;
    let msg2 = insert_user_message(&db, tenant_id, chat_id).await;

    let att_a = insert_attachment(
        &db,
        tenant_id,
        chat_id,
        AttachmentKind::Document,
        "a.pdf",
        AttachmentStatus::Ready,
        None,
    )
    .await;
    let att_b = insert_attachment(
        &db,
        tenant_id,
        chat_id,
        AttachmentKind::Image,
        "b.png",
        AttachmentStatus::Ready,
        None,
    )
    .await;
    let att_c = insert_attachment(
        &db,
        tenant_id,
        chat_id,
        AttachmentKind::Document,
        "c.txt",
        AttachmentStatus::Uploaded,
        None,
    )
    .await;

    // msg1 → att_a, att_b
    link_message_attachment(&db, tenant_id, chat_id, msg1, att_a).await;
    link_message_attachment(&db, tenant_id, chat_id, msg1, att_b).await;
    // msg2 → att_c
    link_message_attachment(&db, tenant_id, chat_id, msg2, att_c).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let map = repo
        .batch_attachment_summaries(&conn, &scope, chat_id, &[msg1, msg2])
        .await
        .expect("batch multi");

    assert_eq!(map.len(), 2);
    assert_eq!(map[&msg1].len(), 2);
    assert_eq!(map[&msg2].len(), 1);
    assert_eq!(map[&msg2][0].attachment_id, att_c);
    assert_eq!(map[&msg2][0].status, "uploaded");
}

#[tokio::test]
async fn batch_shared_attachment_across_messages() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let msg1 = insert_user_message(&db, tenant_id, chat_id).await;
    let msg2 = insert_user_message(&db, tenant_id, chat_id).await;
    let att_id = insert_attachment(
        &db,
        tenant_id,
        chat_id,
        AttachmentKind::Document,
        "shared.pdf",
        AttachmentStatus::Ready,
        None,
    )
    .await;

    // Both messages reference the same attachment (M:N)
    link_message_attachment(&db, tenant_id, chat_id, msg1, att_id).await;
    link_message_attachment(&db, tenant_id, chat_id, msg2, att_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let map = repo
        .batch_attachment_summaries(&conn, &scope, chat_id, &[msg1, msg2])
        .await
        .expect("batch shared");

    assert_eq!(map.len(), 2);
    assert_eq!(map[&msg1].len(), 1);
    assert_eq!(map[&msg2].len(), 1);
    assert_eq!(map[&msg1][0].attachment_id, att_id);
    assert_eq!(map[&msg2][0].attachment_id, att_id);
}

#[tokio::test]
async fn batch_with_img_thumbnail() {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let msg_id = insert_user_message(&db, tenant_id, chat_id).await;
    let thumb_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0]; // fake image bytes
    let att_id = insert_attachment(
        &db,
        tenant_id,
        chat_id,
        AttachmentKind::Image,
        "photo.webp",
        AttachmentStatus::Ready,
        Some((thumb_bytes.clone(), 120, 80)),
    )
    .await;
    link_message_attachment(&db, tenant_id, chat_id, msg_id, att_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let map = repo
        .batch_attachment_summaries(&conn, &scope, chat_id, &[msg_id])
        .await
        .expect("batch thumbnail");

    let summaries = &map[&msg_id];
    assert_eq!(summaries.len(), 1);
    let thumb = summaries[0]
        .img_thumbnail
        .as_ref()
        .expect("thumbnail present");
    assert_eq!(thumb.content_type, "image/webp");
    assert_eq!(thumb.width, 120);
    assert_eq!(thumb.height, 80);
    assert_eq!(thumb.data_base64, BASE64.encode(&thumb_bytes));
}

#[tokio::test]
async fn batch_skips_soft_deleted_attachments() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let msg_id = insert_user_message(&db, tenant_id, chat_id).await;

    // Insert a pre-deleted attachment (deleted_at set at insert time)
    let att_id = insert_deleted_attachment(&db, tenant_id, chat_id).await;
    link_message_attachment(&db, tenant_id, chat_id, msg_id, att_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let map = repo
        .batch_attachment_summaries(&conn, &scope, chat_id, &[msg_id])
        .await
        .expect("batch deleted");

    assert!(map.is_empty(), "soft-deleted attachments must be excluded");
}

// ════════════════════════════════════════════════════════════════════
// Tenant scope isolation
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn batch_cross_tenant_returns_empty() {
    let db = test_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_a, chat_id).await;

    let msg_id = insert_user_message(&db, tenant_a, chat_id).await;
    let att_id = insert_attachment(
        &db,
        tenant_a,
        chat_id,
        AttachmentKind::Document,
        "secret.pdf",
        AttachmentStatus::Ready,
        None,
    )
    .await;
    link_message_attachment(&db, tenant_a, chat_id, msg_id, att_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();

    // Query with tenant_b scope — should return nothing
    let scope_b = AccessScope::for_tenant(tenant_b);
    let map = repo
        .batch_attachment_summaries(&conn, &scope_b, chat_id, &[msg_id])
        .await
        .expect("batch cross-tenant");

    assert!(
        map.is_empty(),
        "cross-tenant query must not return attachments"
    );

    // Query with tenant_a scope — should work
    let scope_a = AccessScope::for_tenant(tenant_a);
    let map = repo
        .batch_attachment_summaries(&conn, &scope_a, chat_id, &[msg_id])
        .await
        .expect("batch own tenant");

    assert_eq!(map.len(), 1, "own tenant query must return attachments");
}
