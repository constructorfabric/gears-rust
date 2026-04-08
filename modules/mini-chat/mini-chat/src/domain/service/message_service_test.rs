use std::sync::Arc;

use modkit_db::secure::secure_insert;
use modkit_odata::ODataQuery;
use modkit_security::AccessScope;
use sea_orm::Set;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::NewChat;

use crate::domain::repos::{
    InsertAssistantMessageParams, InsertUserMessageParams, MessageRepository as MessageRepoTrait,
    ReactionRepository as ReactionRepoTrait, UpsertReactionParams,
};
use crate::domain::service::test_helpers::{
    MockThreadSummaryRepo, inmem_db, mock_db_provider, mock_enforcer, mock_model_resolver,
    mock_tenant_only_enforcer, mock_thread_summary_repo, test_security_ctx,
    test_security_ctx_with_id,
};
use crate::infra::db::entity::attachment::{
    ActiveModel as AttAm, AttachmentKind, AttachmentStatus, Entity as AttEntity,
};
use crate::infra::db::entity::message_attachment::{ActiveModel as MaAm, Entity as MaEntity};
use crate::infra::db::repo::chat_repo::ChatRepository as OrmChatRepository;
use crate::infra::db::repo::message_repo::MessageRepository as OrmMessageRepository;
use crate::infra::db::repo::reaction_repo::ReactionRepository as OrmReactionRepository;

use super::MessageService;
use crate::domain::service::ChatService;

// ── Test Helpers ──

fn limit_cfg() -> modkit_db::odata::LimitCfg {
    modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }
}

fn build_chat_service(
    db_provider: Arc<crate::domain::service::DbProvider>,
    chat_repo: Arc<OrmChatRepository>,
) -> ChatService<OrmChatRepository, MockThreadSummaryRepo> {
    ChatService::new(
        db_provider,
        chat_repo,
        mock_thread_summary_repo(),
        mock_enforcer(),
        mock_model_resolver(),
    )
}

fn build_message_service(
    db_provider: Arc<crate::domain::service::DbProvider>,
    chat_repo: Arc<OrmChatRepository>,
) -> MessageService<OrmMessageRepository, OrmChatRepository, OrmReactionRepository> {
    let message_repo = Arc::new(OrmMessageRepository::new(limit_cfg()));
    let reaction_repo = Arc::new(OrmReactionRepository);
    MessageService::new(
        db_provider,
        message_repo,
        chat_repo,
        reaction_repo,
        mock_enforcer(),
    )
}

fn build_message_service_tenant_only_authz(
    db_provider: Arc<crate::domain::service::DbProvider>,
    chat_repo: Arc<OrmChatRepository>,
) -> MessageService<OrmMessageRepository, OrmChatRepository, OrmReactionRepository> {
    let message_repo = Arc::new(OrmMessageRepository::new(limit_cfg()));
    let reaction_repo = Arc::new(OrmReactionRepository);
    MessageService::new(
        db_provider,
        message_repo,
        chat_repo,
        reaction_repo,
        mock_tenant_only_enforcer(),
    )
}

fn build_chat_service_tenant_only_authz(
    db_provider: Arc<crate::domain::service::DbProvider>,
    chat_repo: Arc<OrmChatRepository>,
) -> ChatService<OrmChatRepository, MockThreadSummaryRepo> {
    ChatService::new(
        db_provider,
        chat_repo,
        mock_thread_summary_repo(),
        mock_tenant_only_enforcer(),
        mock_model_resolver(),
    )
}

// ── Tests ──

#[tokio::test]
async fn list_messages_empty_chat() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Empty chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    let page = msg_svc
        .list_messages(&ctx, chat.id, &ODataQuery::default())
        .await
        .expect("list_messages failed");

    assert!(page.items.is_empty(), "Expected no messages in new chat");
}

#[tokio::test]
async fn list_messages_returns_messages_chronologically() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("With messages".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    // Insert messages via the repo directly using tenant-scoped access
    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn failed");
    let message_repo = OrmMessageRepository::new(limit_cfg());

    let request_id = Uuid::new_v4();

    message_repo
        .insert_user_message(
            &conn,
            &scope,
            InsertUserMessageParams {
                id: Uuid::now_v7(),
                tenant_id,
                chat_id: chat.id,
                request_id,
                content: "Hello".to_owned(),
            },
        )
        .await
        .expect("insert_user_message failed");

    // Ensure distinct created_at timestamps (insert_*_message uses now_utc()).
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    message_repo
        .insert_assistant_message(
            &conn,
            &scope,
            InsertAssistantMessageParams {
                id: Uuid::now_v7(),
                tenant_id,
                chat_id: chat.id,
                request_id,
                content: "Hi there!".to_owned(),
                input_tokens: Some(10),
                output_tokens: Some(20),
                model: Some("gpt-5.2".to_owned()),
                provider_response_id: None,
            },
        )
        .await
        .expect("insert_assistant_message failed");

    let page = msg_svc
        .list_messages(&ctx, chat.id, &ODataQuery::default())
        .await
        .expect("list_messages failed");

    assert_eq!(page.items.len(), 2, "Expected 2 messages");
    assert_eq!(page.items[0].role, "user", "First message should be user");
    assert_eq!(
        page.items[1].role, "assistant",
        "Second message should be assistant"
    );
    assert!(
        page.items[0].created_at <= page.items[1].created_at,
        "Messages should be in chronological order"
    );
}

#[tokio::test]
async fn list_messages_chat_not_found() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let msg_svc = build_message_service(db_provider, chat_repo);

    let ctx = test_security_ctx(Uuid::new_v4());
    let random_chat_id = Uuid::new_v4();

    let result = msg_svc
        .list_messages(&ctx, random_chat_id, &ODataQuery::default())
        .await;

    assert!(result.is_err(), "Expected error for non-existent chat");
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound"
    );
}

#[tokio::test]
async fn list_messages_cross_tenant_returns_not_found() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let ctx_a = test_security_ctx(tenant_a);
    let ctx_b = test_security_ctx(tenant_b);

    // Tenant A creates a chat
    let chat = chat_svc
        .create_chat(
            &ctx_a,
            NewChat {
                model: None,
                title: Some("Tenant A chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    // Tenant B tries to list messages in Tenant A's chat
    let result = msg_svc
        .list_messages(&ctx_b, chat.id, &ODataQuery::default())
        .await;

    assert!(result.is_err(), "Cross-tenant list must fail");
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-tenant access"
    );
}

// ── Pagination Tests ──

/// Insert N user+assistant message pairs into a chat via the repo directly.
async fn insert_message_pairs(
    db_provider: &Arc<crate::domain::service::DbProvider>,
    tenant_id: Uuid,
    chat_id: Uuid,
    count: usize,
) {
    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn failed");
    let message_repo = OrmMessageRepository::new(limit_cfg());

    for _ in 0..count {
        let request_id = Uuid::new_v4();

        message_repo
            .insert_user_message(
                &conn,
                &scope,
                InsertUserMessageParams {
                    id: Uuid::now_v7(),
                    tenant_id,
                    chat_id,
                    request_id,
                    content: "Hello".to_owned(),
                },
            )
            .await
            .expect("insert_user_message failed");

        message_repo
            .insert_assistant_message(
                &conn,
                &scope,
                InsertAssistantMessageParams {
                    id: Uuid::now_v7(),
                    tenant_id,
                    chat_id,
                    request_id,
                    content: "Hi there!".to_owned(),
                    input_tokens: Some(10),
                    output_tokens: Some(20),
                    model: Some("gpt-5.2".to_owned()),
                    provider_response_id: None,
                },
            )
            .await
            .expect("insert_assistant_message failed");
    }
}

#[tokio::test]
async fn list_messages_pagination_forward_cursor() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Pagination chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    // Insert 5 pairs = 10 messages
    insert_message_pairs(&db_provider, tenant_id, chat.id, 5).await;

    // Page 1: request 3 items
    let query = ODataQuery::new().with_limit(3);
    let page1 = msg_svc
        .list_messages(&ctx, chat.id, &query)
        .await
        .expect("list_messages page 1 failed");

    assert_eq!(page1.items.len(), 3, "Page 1 should have 3 items");
    assert!(
        page1.page_info.next_cursor.is_some(),
        "Page 1 must have next_cursor (7 more messages remain)"
    );
    assert!(
        page1.page_info.prev_cursor.is_none(),
        "Page 1 must not have prev_cursor (first page)"
    );

    // Page 2: use next_cursor
    let cursor = modkit_odata::CursorV1::decode(page1.page_info.next_cursor.as_ref().unwrap())
        .expect("decode cursor failed");
    let query2 = ODataQuery::new().with_limit(3).with_cursor(cursor);
    let page2 = msg_svc
        .list_messages(&ctx, chat.id, &query2)
        .await
        .expect("list_messages page 2 failed");

    assert_eq!(page2.items.len(), 3, "Page 2 should have 3 items");
    assert!(
        page2.page_info.next_cursor.is_some(),
        "Page 2 must have next_cursor (4 more messages remain)"
    );

    // Continue until exhausted, collecting all IDs
    let mut all_ids: Vec<Uuid> = page1
        .items
        .iter()
        .chain(page2.items.iter())
        .map(|m| m.id)
        .collect();

    let mut current_page = page2;
    while let Some(ref next) = current_page.page_info.next_cursor {
        let cursor = modkit_odata::CursorV1::decode(next).expect("decode cursor failed");
        let q = ODataQuery::new().with_limit(3).with_cursor(cursor);
        current_page = msg_svc
            .list_messages(&ctx, chat.id, &q)
            .await
            .expect("list_messages next page failed");
        all_ids.extend(current_page.items.iter().map(|m| m.id));
    }

    assert_eq!(
        all_ids.len(),
        10,
        "Total messages across all pages should be 10"
    );
    let unique_count = {
        let mut sorted = all_ids.clone();
        sorted.sort();
        sorted.dedup();
        sorted.len()
    };
    assert_eq!(unique_count, 10, "All message IDs must be unique");
}

#[tokio::test]
async fn list_messages_pagination_no_cursor_when_all_fit() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Small chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    // Insert 2 pairs = 4 messages, request page of 20
    insert_message_pairs(&db_provider, tenant_id, chat.id, 2).await;

    let query = ODataQuery::new().with_limit(20);
    let page = msg_svc
        .list_messages(&ctx, chat.id, &query)
        .await
        .expect("list_messages failed");

    assert_eq!(page.items.len(), 4);
    assert!(
        page.page_info.next_cursor.is_none(),
        "No next_cursor when all messages fit in a single page"
    );
    assert!(
        page.page_info.prev_cursor.is_none(),
        "No prev_cursor on the first (and only) page"
    );
}

#[tokio::test]
async fn list_messages_pagination_backward_cursor() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Backward pagination chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    // Insert 5 pairs = 10 messages
    insert_message_pairs(&db_provider, tenant_id, chat.id, 5).await;

    // Page 1 forward (3 items)
    let query = ODataQuery::new().with_limit(3);
    let page1 = msg_svc
        .list_messages(&ctx, chat.id, &query)
        .await
        .expect("list_messages page 1 failed");
    assert_eq!(page1.items.len(), 3);

    // Page 2 forward
    let cursor = modkit_odata::CursorV1::decode(page1.page_info.next_cursor.as_ref().unwrap())
        .expect("decode cursor failed");
    let query2 = ODataQuery::new().with_limit(3).with_cursor(cursor);
    let page2 = msg_svc
        .list_messages(&ctx, chat.id, &query2)
        .await
        .expect("list_messages page 2 failed");
    assert_eq!(page2.items.len(), 3);
    assert!(
        page2.page_info.prev_cursor.is_some(),
        "Page 2 must have prev_cursor"
    );

    // Navigate backward from page 2
    let prev = modkit_odata::CursorV1::decode(page2.page_info.prev_cursor.as_ref().unwrap())
        .expect("decode prev cursor failed");
    let query_back = ODataQuery::new().with_limit(3).with_cursor(prev);
    let page_back = msg_svc
        .list_messages(&ctx, chat.id, &query_back)
        .await
        .expect("list_messages backward failed");

    assert_eq!(
        page_back.items.len(),
        page1.items.len(),
        "Backward page should have same count as page 1"
    );
    let back_ids: Vec<Uuid> = page_back.items.iter().map(|m| m.id).collect();
    let page1_ids: Vec<Uuid> = page1.items.iter().map(|m| m.id).collect();
    assert_eq!(
        back_ids, page1_ids,
        "Backward navigation must return to page 1 items"
    );
}

// ════════════════════════════════════════════════════════════════════
// Attachment integration tests
// ════════════════════════════════════════════════════════════════════

/// Insert an attachment row via `secure_insert`. Returns the attachment ID.
async fn insert_attachment(
    db_provider: &Arc<crate::domain::service::DbProvider>,
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
    let conn = db_provider.conn().expect("conn");
    let scope = AccessScope::allow_all();
    secure_insert::<AttEntity>(am, &scope, &conn)
        .await
        .expect("insert attachment");
    att_id
}

/// Link a message to an attachment via `message_attachments`.
async fn link_message_attachment(
    db_provider: &Arc<crate::domain::service::DbProvider>,
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
    let conn = db_provider.conn().expect("conn");
    let scope = AccessScope::allow_all();
    secure_insert::<MaEntity>(am, &scope, &conn)
        .await
        .expect("link message attachment");
}

#[tokio::test]
async fn list_messages_returns_attachments() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Attachments test".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat");

    // Insert a user message
    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn");
    let message_repo = OrmMessageRepository::new(limit_cfg());
    let request_id = Uuid::new_v4();
    let msg_id = Uuid::now_v7();

    message_repo
        .insert_user_message(
            &conn,
            &scope,
            InsertUserMessageParams {
                id: msg_id,
                tenant_id,
                chat_id: chat.id,
                request_id,
                content: "See attached".to_owned(),
            },
        )
        .await
        .expect("insert_user_message");

    // Insert two attachments and link them to the message
    let att_a = insert_attachment(
        &db_provider,
        tenant_id,
        chat.id,
        AttachmentKind::Document,
        "report.pdf",
        AttachmentStatus::Ready,
        None,
    )
    .await;
    let att_b = insert_attachment(
        &db_provider,
        tenant_id,
        chat.id,
        AttachmentKind::Image,
        "photo.webp",
        AttachmentStatus::Ready,
        Some((vec![0xFF, 0xD8], 120, 80)),
    )
    .await;
    link_message_attachment(&db_provider, tenant_id, chat.id, msg_id, att_a).await;
    link_message_attachment(&db_provider, tenant_id, chat.id, msg_id, att_b).await;

    // list_messages should return the message with both attachments
    let page = msg_svc
        .list_messages(&ctx, chat.id, &ODataQuery::default())
        .await
        .expect("list_messages");

    assert_eq!(page.items.len(), 1);
    let msg = &page.items[0];
    assert_eq!(
        msg.attachments.len(),
        2,
        "message should have 2 attachments"
    );

    let att_ids: Vec<Uuid> = msg.attachments.iter().map(|a| a.attachment_id).collect();
    assert!(att_ids.contains(&att_a), "must contain att_a");
    assert!(att_ids.contains(&att_b), "must contain att_b");

    // Verify the image attachment has a thumbnail
    let img_att = msg
        .attachments
        .iter()
        .find(|a| a.attachment_id == att_b)
        .expect("image attachment");
    assert_eq!(img_att.kind, "image");
    assert_eq!(img_att.filename, "photo.webp");
    let thumb = img_att.img_thumbnail.as_ref().expect("thumbnail present");
    assert_eq!(thumb.width, 120);
    assert_eq!(thumb.height, 80);

    // Verify the document attachment has no thumbnail
    let doc_att = msg
        .attachments
        .iter()
        .find(|a| a.attachment_id == att_a)
        .expect("document attachment");
    assert_eq!(doc_att.kind, "document");
    assert!(doc_att.img_thumbnail.is_none());
}

#[tokio::test]
async fn list_messages_no_attachments_returns_empty_vec() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("No attachments".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat");

    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn");
    let message_repo = OrmMessageRepository::new(limit_cfg());

    message_repo
        .insert_user_message(
            &conn,
            &scope,
            InsertUserMessageParams {
                id: Uuid::now_v7(),
                tenant_id,
                chat_id: chat.id,
                request_id: Uuid::new_v4(),
                content: "Plain message".to_owned(),
            },
        )
        .await
        .expect("insert_user_message");

    let page = msg_svc
        .list_messages(&ctx, chat.id, &ODataQuery::default())
        .await
        .expect("list_messages");

    assert_eq!(page.items.len(), 1);
    assert!(
        page.items[0].attachments.is_empty(),
        "message without links must have empty attachments"
    );
}

#[tokio::test]
async fn list_messages_mixed_messages_with_and_without_attachments() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Mixed".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat");

    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn");
    let message_repo = OrmMessageRepository::new(limit_cfg());
    let request_id = Uuid::new_v4();

    // User message with attachment
    let user_msg_id = Uuid::now_v7();
    message_repo
        .insert_user_message(
            &conn,
            &scope,
            InsertUserMessageParams {
                id: user_msg_id,
                tenant_id,
                chat_id: chat.id,
                request_id,
                content: "With file".to_owned(),
            },
        )
        .await
        .expect("insert_user_message");

    let att_id = insert_attachment(
        &db_provider,
        tenant_id,
        chat.id,
        AttachmentKind::Document,
        "notes.txt",
        AttachmentStatus::Ready,
        None,
    )
    .await;
    link_message_attachment(&db_provider, tenant_id, chat.id, user_msg_id, att_id).await;

    // Assistant message without attachment
    let asst_msg_id = Uuid::now_v7();
    message_repo
        .insert_assistant_message(
            &conn,
            &scope,
            InsertAssistantMessageParams {
                id: asst_msg_id,
                tenant_id,
                chat_id: chat.id,
                request_id,
                content: "Got it".to_owned(),
                input_tokens: None,
                output_tokens: None,
                model: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("insert_assistant_message");

    let page = msg_svc
        .list_messages(&ctx, chat.id, &ODataQuery::default())
        .await
        .expect("list_messages");

    assert_eq!(page.items.len(), 2);

    let user_msg = page
        .items
        .iter()
        .find(|m| m.id == user_msg_id)
        .expect("user msg");
    assert_eq!(user_msg.attachments.len(), 1);
    assert_eq!(user_msg.attachments[0].attachment_id, att_id);

    let asst_msg = page
        .items
        .iter()
        .find(|m| m.id == asst_msg_id)
        .expect("asst msg");
    assert!(
        asst_msg.attachments.is_empty(),
        "assistant message must have no attachments"
    );
}

// ════════════════════════════════════════════════════════════════════
// my_reaction integration tests
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_messages_returns_my_reaction() {
    use crate::domain::models::ReactionKind;

    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let msg_svc = build_message_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let ctx = test_security_ctx_with_id(tenant_id, user_id);

    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Reaction test".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat");

    // Insert user + assistant messages
    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn");
    let message_repo = OrmMessageRepository::new(limit_cfg());
    let request_id = Uuid::new_v4();

    let user_msg_id = Uuid::now_v7();
    message_repo
        .insert_user_message(
            &conn,
            &scope,
            InsertUserMessageParams {
                id: user_msg_id,
                tenant_id,
                chat_id: chat.id,
                request_id,
                content: "Hello".to_owned(),
            },
        )
        .await
        .expect("insert_user_message");

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let asst_msg_id = Uuid::now_v7();
    message_repo
        .insert_assistant_message(
            &conn,
            &scope,
            InsertAssistantMessageParams {
                id: asst_msg_id,
                tenant_id,
                chat_id: chat.id,
                request_id,
                content: "Hi there!".to_owned(),
                input_tokens: Some(10),
                output_tokens: Some(20),
                model: Some("gpt-5.2".to_owned()),
                provider_response_id: None,
            },
        )
        .await
        .expect("insert_assistant_message");

    // Add a "like" reaction on the assistant message
    let reaction_repo = OrmReactionRepository;
    let reaction_scope = AccessScope::allow_all();
    reaction_repo
        .upsert(
            &conn,
            &reaction_scope,
            UpsertReactionParams {
                id: Uuid::now_v7(),
                tenant_id,
                message_id: asst_msg_id,
                user_id,
                reaction: ReactionKind::Like,
            },
        )
        .await
        .expect("upsert reaction");

    // list_messages should return my_reaction
    let page = msg_svc
        .list_messages(&ctx, chat.id, &ODataQuery::default())
        .await
        .expect("list_messages");

    assert_eq!(page.items.len(), 2);

    let user_msg = page
        .items
        .iter()
        .find(|m| m.id == user_msg_id)
        .expect("user msg");
    assert_eq!(
        user_msg.my_reaction, None,
        "user message should have no reaction"
    );

    let asst_msg = page
        .items
        .iter()
        .find(|m| m.id == asst_msg_id)
        .expect("asst msg");
    assert_eq!(
        asst_msg.my_reaction,
        Some(ReactionKind::Like),
        "assistant message should have Like reaction"
    );
}

// ── Tenant-only AuthZ: user isolation via ensure_owner ──

#[tokio::test]
async fn list_messages_tenant_only_authz_cross_owner_not_found() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    // User A creates a chat via a tenant-only authz chat service
    let chat_svc =
        build_chat_service_tenant_only_authz(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let chat = chat_svc
        .create_chat(
            &ctx_a,
            NewChat {
                model: None,
                title: Some("User A chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    // User B (same tenant) tries to list messages in User A's chat
    let msg_svc =
        build_message_service_tenant_only_authz(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let result = msg_svc
        .list_messages(&ctx_b, chat.id, &ODataQuery::default())
        .await;

    assert!(
        result.is_err(),
        "Cross-owner list_messages must fail with tenant-only authz"
    );
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-owner access with tenant-only authz"
    );
}
