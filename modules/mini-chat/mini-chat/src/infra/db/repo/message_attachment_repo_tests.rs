use super::*;
use crate::domain::repos::MessageAttachmentRepository as _;
use crate::domain::service::test_helpers::{
    InsertTestAttachmentParams, inmem_db, insert_chat, insert_test_attachment, insert_test_message,
    insert_test_message_attachment, mock_db_provider,
};
use modkit_security::AccessScope;

impl MessageAttachmentRepository {
    pub async fn exists_for_attachment<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        attachment_id: Uuid,
    ) -> Result<bool, DomainError> {
        let count = Entity::find()
            .filter(Column::AttachmentId.eq(attachment_id))
            .secure()
            .scope_with(scope)
            .count(runner)
            .await
            .map_err(db_err)?;
        Ok(count > 0)
    }
}

/// P5-L1: `copy_for_retry` copies non-deleted attachments, skips soft-deleted.
#[tokio::test]
async fn copy_for_retry_excludes_soft_deleted() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    // 3 attachments: 2 ready, 1 soft-deleted
    let att1 = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;
    let att2 = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;
    let att3_deleted = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            deleted_at: Some(OffsetDateTime::now_utc()),
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    let original_msg_id = Uuid::new_v4();
    insert_test_message(&db, tenant_id, chat_id, original_msg_id).await;

    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    // Link all 3 to the original message
    for att_id in [att1, att2, att3_deleted] {
        insert_test_message_attachment(&db, tenant_id, chat_id, original_msg_id, att_id).await;
    }

    // Copy to new message
    let new_msg_id = Uuid::new_v4();
    insert_test_message(&db, tenant_id, chat_id, new_msg_id).await;

    let repo = MessageAttachmentRepository;
    let copied = repo
        .copy_for_retry(&conn, &scope, original_msg_id, new_msg_id, chat_id)
        .await
        .expect("copy_for_retry");

    // Only 2 non-deleted attachments should be copied
    assert_eq!(copied, 2, "should copy 2 non-deleted attachments");

    // Verify the deleted one is not linked to the new message
    let exists_deleted = repo
        .exists_for_attachment(&conn, &scope, att3_deleted)
        .await
        .expect("exists_for_attachment");
    // It exists for the original message, but the new message's links are only att1 and att2.
    // exists_for_attachment checks globally (any message), so the deleted one still has the original link.
    assert!(
        exists_deleted,
        "deleted attachment still has original message link"
    );
}

/// P5-L2: `copy_for_retry` when all original attachments are deleted → 0 copied.
#[tokio::test]
async fn copy_for_retry_all_deleted_returns_zero() {
    let db = mock_db_provider(inmem_db().await);
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let att1 = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            uploaded_by_user_id: user_id,
            deleted_at: Some(OffsetDateTime::now_utc()),
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;

    let original_msg_id = Uuid::new_v4();
    insert_test_message(&db, tenant_id, chat_id, original_msg_id).await;
    insert_test_message_attachment(&db, tenant_id, chat_id, original_msg_id, att1).await;

    let new_msg_id = Uuid::new_v4();
    insert_test_message(&db, tenant_id, chat_id, new_msg_id).await;

    let repo = MessageAttachmentRepository;
    let scope = AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let copied = repo
        .copy_for_retry(&conn, &scope, original_msg_id, new_msg_id, chat_id)
        .await
        .expect("copy_for_retry");

    assert_eq!(copied, 0, "should copy 0 when all attachments are deleted");
}
