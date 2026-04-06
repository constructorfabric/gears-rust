use super::*;

fn make_msg() -> OutboxMessage {
    OutboxMessage {
        partition_id: 1,
        seq: 1,
        payload: b"{}".to_vec(),
        payload_type: "application/json".to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 0i16,
    }
}

fn make_cleanup_payload(provider_file_id: Option<&str>) -> OutboxMessage {
    let event = serde_json::json!({
        "event_type": "attachment_deleted",
        "tenant_id": "00000000-0000-0000-0000-000000000001",
        "chat_id": "00000000-0000-0000-0000-000000000002",
        "attachment_id": "00000000-0000-0000-0000-000000000003",
        "provider_file_id": provider_file_id,
        "vector_store_id": null,
        "storage_backend": "openai",
        "attachment_kind": "document",
        "deleted_at": "2026-01-01T00:00:00Z"
    });
    OutboxMessage {
        partition_id: 1,
        seq: 1,
        payload: serde_json::to_vec(&event).unwrap(),
        payload_type: "application/json".to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 0i16,
    }
}

#[tokio::test]
async fn attachment_handler_rejects_invalid_payload() {
    use crate::domain::service::test_helpers::inmem_db;

    let db = inmem_db().await;
    let db_provider = crate::domain::service::test_helpers::mock_db_provider(db);
    let handler = AttachmentCleanupHandler::new(
        Arc::new(crate::domain::service::test_helpers::NoopFileStorage),
        db_provider,
        crate::infra::db::repo::chat_repo::ChatRepository::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }),
        5,
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    );

    let msg = make_msg(); // payload is "{}" — missing required fields
    let result = handler.handle(&msg).await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "invalid payload should be rejected"
    );
}

#[tokio::test]
async fn attachment_handler_succeeds_no_provider_file() {
    use crate::domain::service::test_helpers::inmem_db;

    let db = inmem_db().await;
    let db_provider = crate::domain::service::test_helpers::mock_db_provider(db);
    let handler = AttachmentCleanupHandler::new(
        Arc::new(crate::domain::service::test_helpers::NoopFileStorage),
        db_provider,
        crate::infra::db::repo::chat_repo::ChatRepository::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }),
        5,
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    );

    let msg = make_cleanup_payload(None);
    let result = handler.handle(&msg).await;
    // mark_done will fail (attachment doesn't exist in DB) → Retry
    // but the important thing is it doesn't Reject for missing provider_file_id
    assert!(
        matches!(result, MessageResult::Ok | MessageResult::Retry),
        "no provider file should not reject"
    );
}

#[tokio::test]
async fn deserialize_cleanup_payload() {
    let msg = make_cleanup_payload(Some("file-abc123"));
    let payload: AttachmentCleanupPayload =
        serde_json::from_slice(&msg.payload).expect("deserialization should succeed");
    assert_eq!(
        payload.attachment_id.to_string(),
        "00000000-0000-0000-0000-000000000003"
    );
    assert_eq!(payload.provider_file_id.as_deref(), Some("file-abc123"));
    assert_eq!(payload.storage_backend, "openai");
}

// ── Chat cleanup handler tests ──────────────────────────────────

fn make_chat_cleanup_payload(chat_id: uuid::Uuid) -> OutboxMessage {
    let event = serde_json::json!({
        "reason": "chat_soft_delete",
        "tenant_id": uuid::Uuid::new_v4().to_string(),
        "chat_id": chat_id.to_string(),
        "system_request_id": uuid::Uuid::new_v4().to_string(),
        "chat_deleted_at": "2026-01-01T00:00:00+00:00",
    });
    OutboxMessage {
        partition_id: 1,
        seq: 1,
        payload: serde_json::to_vec(&event).unwrap(),
        payload_type: "application/json".to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 0i16,
    }
}

fn build_chat_handler(db_provider: Arc<DbProvider>) -> ChatCleanupHandler {
    use crate::domain::service::test_helpers::{NoopFileStorage, NoopVectorStoreProvider};
    ChatCleanupHandler::new(
        Arc::new(NoopFileStorage),
        Arc::new(NoopVectorStoreProvider),
        db_provider,
        crate::infra::db::repo::chat_repo::ChatRepository::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }),
        5,
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    )
}

#[tokio::test]
async fn chat_cleanup_rejects_invalid_payload() {
    use crate::domain::service::test_helpers::inmem_db;

    let db = inmem_db().await;
    let handler = build_chat_handler(crate::domain::service::test_helpers::mock_db_provider(db));

    let msg = make_msg(); // "{}" — missing fields
    let result = handler.handle(&msg).await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "invalid payload should be rejected"
    );
}

#[tokio::test]
async fn chat_cleanup_rejects_active_chat() {
    use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};

    let db = inmem_db().await;
    let handler = build_chat_handler(mock_db_provider(db));

    // Non-existent chat → is_deleted_system returns false
    let msg = make_chat_cleanup_payload(uuid::Uuid::new_v4());
    let result = handler.handle(&msg).await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "active/non-existent chat should be rejected"
    );
}

#[tokio::test]
async fn chat_cleanup_succeeds_empty_chat() {
    use crate::domain::repos::ChatRepository as _;
    use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};

    let db = inmem_db().await;
    let db_provider = mock_db_provider(db.clone());

    // Create and soft-delete a chat
    let chat_repo =
        crate::infra::db::repo::chat_repo::ChatRepository::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        });
    let tenant_id = uuid::Uuid::new_v4();
    let user_id = uuid::Uuid::new_v4();
    let chat_id = uuid::Uuid::new_v4();
    let scope = modkit_security::AccessScope::allow_all();
    let conn = db_provider.conn().unwrap();

    let chat = crate::domain::models::Chat {
        id: chat_id,
        tenant_id,
        user_id,
        model: "test-model".to_owned(),
        title: Some("test".to_owned()),
        is_temporary: false,
        created_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    };
    chat_repo.create(&conn, &scope, chat).await.unwrap();
    chat_repo.soft_delete(&conn, &scope, chat_id).await.unwrap();

    let handler = build_chat_handler(db_provider);
    let msg = make_chat_cleanup_payload(chat_id);
    let result = handler.handle(&msg).await;
    assert!(
        matches!(result, MessageResult::Ok),
        "empty soft-deleted chat should succeed, got: {result:?}"
    );
}

#[tokio::test]
async fn deserialize_chat_cleanup_payload() {
    let chat_id = uuid::Uuid::new_v4();
    let msg = make_chat_cleanup_payload(chat_id);
    let payload: ChatCleanupPayload =
        serde_json::from_slice(&msg.payload).expect("deserialization should succeed");
    assert_eq!(payload.chat_id, chat_id);
    assert_eq!(
        payload.reason,
        crate::domain::repos::CleanupReason::ChatSoftDelete
    );
}

// ── State-machine tests with seeded DB ──────────────────────────────

/// Insert a minimal attachment row with `cleanup_status` = 'pending'
/// and `deleted_at` set (soft-deleted).
async fn seed_pending_attachment(
    db: &Arc<DbProvider>,
    chat_id: uuid::Uuid,
    tenant_id: uuid::Uuid,
    provider_file_id: Option<&str>,
) -> uuid::Uuid {
    use crate::domain::repos::{AttachmentRepository as _, InsertAttachmentParams};
    let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
    let scope = modkit_security::AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let att_id = uuid::Uuid::new_v4();
    // Insert in pending status
    repo.insert(
        &conn,
        &scope,
        InsertAttachmentParams {
            id: att_id,
            tenant_id,
            chat_id,
            uploaded_by_user_id: uuid::Uuid::new_v4(),
            filename: "test.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            size_bytes: 100,
            storage_backend: "openai".to_owned(),
            attachment_kind: "document".to_owned(),
            for_file_search: false,
            for_code_interpreter: false,
        },
    )
    .await
    .expect("insert attachment");

    // If provider_file_id is set, transition to uploaded
    if let Some(pfid) = provider_file_id {
        use crate::domain::repos::SetUploadedParams;
        repo.cas_set_uploaded(
            &conn,
            &scope,
            SetUploadedParams {
                id: att_id,
                provider_file_id: pfid.to_owned(),
                size_bytes: 100,
            },
        )
        .await
        .expect("set uploaded");
    }

    // Mark cleanup pending BEFORE soft-deleting (mimics the chat-deletion TX
    // where attachments are NOT individually soft-deleted, only marked pending).
    repo.mark_attachments_pending_for_chat(&conn, chat_id)
        .await
        .expect("mark pending");

    att_id
}

/// Create a soft-deleted chat in the DB.
async fn seed_deleted_chat(db: &Arc<DbProvider>) -> (uuid::Uuid, uuid::Uuid) {
    use crate::domain::repos::ChatRepository as _;
    let chat_repo =
        crate::infra::db::repo::chat_repo::ChatRepository::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        });
    let tenant_id = uuid::Uuid::new_v4();
    let chat_id = uuid::Uuid::new_v4();
    let scope = modkit_security::AccessScope::allow_all();
    let conn = db.conn().unwrap();
    let chat = crate::domain::models::Chat {
        id: chat_id,
        tenant_id,
        user_id: uuid::Uuid::new_v4(),
        model: "test-model".to_owned(),
        title: Some("test".to_owned()),
        is_temporary: false,
        created_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    };
    chat_repo.create(&conn, &scope, chat).await.unwrap();
    chat_repo.soft_delete(&conn, &scope, chat_id).await.unwrap();
    (chat_id, tenant_id)
}

#[tokio::test]
async fn chat_cleanup_processes_pending_attachment_success() {
    use crate::domain::repos::AttachmentRepository as _;
    use crate::domain::service::test_helpers::inmem_db;

    let db = inmem_db().await;
    let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());

    let (chat_id, tenant_id) = seed_deleted_chat(&db_provider).await;
    seed_pending_attachment(&db_provider, chat_id, tenant_id, Some("file-123")).await;

    let handler = build_chat_handler(Arc::clone(&db_provider));
    let msg = make_chat_cleanup_payload(chat_id);
    let result = handler.handle(&msg).await;

    assert!(
        matches!(result, MessageResult::Ok),
        "should succeed with NoopFileStorage, got: {result:?}"
    );

    // Verify attachment is now 'done'
    let conn = db_provider.conn().unwrap();
    let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
    let pending = repo
        .find_pending_cleanup_by_chat(&conn, chat_id)
        .await
        .unwrap();
    assert!(pending.is_empty(), "no attachments should remain pending");
}

#[tokio::test]
async fn chat_cleanup_retries_on_provider_failure() {
    use crate::domain::repos::AttachmentRepository as _;
    use crate::domain::service::test_helpers::{
        FailingFileStorage, NoopVectorStoreProvider, inmem_db,
    };

    let db = inmem_db().await;
    let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());

    let (chat_id, tenant_id) = seed_deleted_chat(&db_provider).await;
    seed_pending_attachment(&db_provider, chat_id, tenant_id, Some("file-456")).await;

    // Use FailingFileStorage — provider always errors
    let handler = ChatCleanupHandler::new(
        Arc::new(FailingFileStorage),
        Arc::new(NoopVectorStoreProvider),
        Arc::clone(&db_provider),
        crate::infra::db::repo::chat_repo::ChatRepository::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }),
        5, // max_attempts
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    );

    let msg = make_chat_cleanup_payload(chat_id);
    let result = handler.handle(&msg).await;

    assert!(
        matches!(result, MessageResult::Retry),
        "should retry on provider failure, got: {result:?}"
    );

    // Verify attachment is still pending with incremented attempts
    let conn = db_provider.conn().unwrap();
    let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
    let pending = repo
        .find_pending_cleanup_by_chat(&conn, chat_id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "attachment should still be pending");
    assert_eq!(
        pending[0].cleanup_attempts, 1,
        "attempts should be incremented"
    );
    assert!(
        pending[0].last_cleanup_error.is_some(),
        "error should be recorded"
    );
}

#[tokio::test]
async fn chat_cleanup_terminal_failure_at_max_attempts() {
    use crate::domain::repos::AttachmentRepository as _;
    use crate::domain::service::test_helpers::{
        FailingFileStorage, NoopVectorStoreProvider, inmem_db,
    };

    let db = inmem_db().await;
    let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());

    let (chat_id, tenant_id) = seed_deleted_chat(&db_provider).await;
    seed_pending_attachment(&db_provider, chat_id, tenant_id, Some("file-789")).await;

    // max_attempts = 1 → first failure is terminal
    let handler = ChatCleanupHandler::new(
        Arc::new(FailingFileStorage),
        Arc::new(NoopVectorStoreProvider),
        Arc::clone(&db_provider),
        crate::infra::db::repo::chat_repo::ChatRepository::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }),
        1, // max_attempts = 1 → immediately terminal
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    );

    let msg = make_chat_cleanup_payload(chat_id);
    let result = handler.handle(&msg).await;

    // All attachments terminal (failed) → handler proceeds to VS check → Success
    assert!(
        matches!(result, MessageResult::Ok),
        "all attachments terminal -> should succeed, got: {result:?}"
    );

    // Verify attachment is now 'failed'
    // Need the attachment ID — re-seed returns it
    // Actually we need to find it. Let's use find_pending which should return empty.
    let conn = db_provider.conn().unwrap();
    let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
    let pending = repo
        .find_pending_cleanup_by_chat(&conn, chat_id)
        .await
        .unwrap();
    assert!(
        pending.is_empty(),
        "no pending attachments -- the one we had should be 'failed'"
    );

    // Also verify count_failed returns 1
    let failed = repo
        .count_failed_cleanup_by_chat(&conn, chat_id)
        .await
        .unwrap();
    assert_eq!(
        failed, 1,
        "one attachment should be in terminal failed state"
    );
}
