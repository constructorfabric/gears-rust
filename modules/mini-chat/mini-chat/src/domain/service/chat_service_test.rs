use std::sync::Arc;

use crate::domain::models::{ChatPatch, NewChat};
use modkit_odata::ODataQuery;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::db::repo::chat_repo::ChatRepository as OrmChatRepository;

use super::ChatService;
use crate::domain::service::test_helpers::{
    MockThreadSummaryRepo, inmem_db, mock_db_provider, mock_enforcer, mock_model_resolver,
    mock_tenant_only_enforcer, mock_thread_summary_repo, test_security_ctx,
    test_security_ctx_with_id,
};

// ── Test Helpers ──

fn build_service_with_enforcer(
    db: modkit_db::Db,
    enforcer: authz_resolver_sdk::PolicyEnforcer,
) -> ChatService<OrmChatRepository, MockThreadSummaryRepo> {
    let db = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));

    ChatService::new(
        db,
        chat_repo,
        mock_thread_summary_repo(),
        enforcer,
        mock_model_resolver(),
    )
}

fn build_service(db: modkit_db::Db) -> ChatService<OrmChatRepository, MockThreadSummaryRepo> {
    build_service_with_enforcer(db, mock_enforcer())
}

fn build_service_tenant_only_authz(
    db: modkit_db::Db,
) -> ChatService<OrmChatRepository, MockThreadSummaryRepo> {
    build_service_with_enforcer(db, mock_tenant_only_enforcer())
}

// ── Tests ──

#[tokio::test]
async fn create_chat_default_model() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let result = svc
        .create_chat(
            &ctx,
            NewChat {
                model: None, // empty → default
                title: Some("Hello".to_owned()),
                is_temporary: false,
            },
        )
        .await;

    assert!(result.is_ok(), "create_chat failed: {result:?}");
    let detail = result.unwrap();
    assert_eq!(detail.model, "gpt-5.2"); // default model
    assert_eq!(detail.title.as_deref(), Some("Hello"));
    assert_eq!(detail.message_count, 0);
}

#[tokio::test]
async fn create_chat_explicit_valid_model() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let result = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: None,
                is_temporary: false,
            },
        )
        .await;

    assert!(result.is_ok(), "create_chat failed: {result:?}");
    assert_eq!(result.unwrap().model, "gpt-5.2");
}

#[tokio::test]
async fn create_chat_disabled_model_rejected() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let result = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5-mini".to_owned()),
                title: None,
                is_temporary: false,
            },
        )
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, DomainError::InvalidModel { .. }),
        "Expected InvalidModel for disabled model, got: {err:?}"
    );
}

#[tokio::test]
async fn create_chat_empty_title_rejected() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let result = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some(String::new()),
                is_temporary: false,
            },
        )
        .await;

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), DomainError::Validation { .. }),
        "Expected Validation error for empty title at create"
    );
}

#[tokio::test]
async fn create_chat_title_trimmed() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let result = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("  padded  ".to_owned()),
                is_temporary: false,
            },
        )
        .await;

    assert!(result.is_ok(), "create_chat failed: {result:?}");
    assert_eq!(result.unwrap().title.as_deref(), Some("padded"));
}

#[tokio::test]
async fn create_chat_invalid_model() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let result = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("nonexistent-model".to_owned()),
                title: None,
                is_temporary: false,
            },
        )
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, DomainError::InvalidModel { .. }),
        "Expected InvalidModel, got: {err:?}"
    );
}

#[tokio::test]
async fn get_chat_happy_path() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    // Create first
    let created = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("Test".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    // Get
    let fetched = svc.get_chat(&ctx, created.id).await.expect("get failed");
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.title.as_deref(), Some("Test"));
}

#[tokio::test]
async fn get_chat_not_found() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let result = svc.get_chat(&ctx, Uuid::new_v4()).await;

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound"
    );
}

#[tokio::test]
async fn update_chat_title_happy_path() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let created = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("Old Title".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    let updated = svc
        .update_chat(
            &ctx,
            created.id,
            ChatPatch {
                title: Some(Some("New Title".to_owned())),
            },
        )
        .await
        .expect("update failed");

    assert_eq!(updated.title.as_deref(), Some("New Title"));
}

#[tokio::test]
async fn update_chat_title_empty_rejected() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let created = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("Title".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    let result = svc
        .update_chat(
            &ctx,
            created.id,
            ChatPatch {
                title: Some(Some(String::new())),
            },
        )
        .await;

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), DomainError::Validation { .. }),
        "Expected Validation error"
    );
}

#[tokio::test]
async fn update_chat_title_whitespace_only_rejected() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let created = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("Title".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    let result = svc
        .update_chat(
            &ctx,
            created.id,
            ChatPatch {
                title: Some(Some("   ".to_owned())),
            },
        )
        .await;

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), DomainError::Validation { .. }),
        "Expected Validation error"
    );
}

#[tokio::test]
async fn update_chat_title_too_long_rejected() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let created = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("Title".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    let long_title = "a".repeat(256);
    let result = svc
        .update_chat(
            &ctx,
            created.id,
            ChatPatch {
                title: Some(Some(long_title)),
            },
        )
        .await;

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), DomainError::Validation { .. }),
        "Expected Validation error"
    );
}

#[tokio::test]
async fn delete_chat_happy_path() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    let created = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("To Delete".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    let result = svc.delete_chat(&ctx, created.id).await;
    assert!(result.is_ok(), "delete failed: {result:?}");

    // Should not be found after deletion
    let get_result = svc.get_chat(&ctx, created.id).await;
    assert!(matches!(
        get_result.unwrap_err(),
        DomainError::ChatNotFound { .. }
    ));
}

#[tokio::test]
async fn list_chats_returns_page() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    // Create two chats
    svc.create_chat(
        &ctx,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: Some("First".to_owned()),
            is_temporary: false,
        },
    )
    .await
    .expect("create 1 failed");

    svc.create_chat(
        &ctx,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: Some("Second".to_owned()),
            is_temporary: false,
        },
    )
    .await
    .expect("create 2 failed");

    let query = ODataQuery::default();
    let page = svc.list_chats(&ctx, &query).await.expect("list failed");

    assert_eq!(page.items.len(), 2);
    // Verify descending sort invariant (updated_at DESC, id DESC tiebreaker)
    assert!(
        page.items
            .windows(2)
            .all(|w| (w[0].updated_at, w[0].id) >= (w[1].updated_at, w[1].id)),
        "Expected items sorted by (updated_at, id) DESC"
    );
}

// ── Permission Denied Tests ──

#[tokio::test]
async fn list_chats_cross_tenant_returns_empty() {
    let db = inmem_db().await;
    let svc = build_service(db);

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let ctx_a = test_security_ctx(tenant_a);
    let ctx_b = test_security_ctx(tenant_b);

    // Tenant A creates a chat
    svc.create_chat(
        &ctx_a,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: Some("Tenant A chat".to_owned()),
            is_temporary: false,
        },
    )
    .await
    .expect("create failed");

    // Tenant B lists — should see nothing (owner_tenant_id constraint filters)
    let page = svc
        .list_chats(&ctx_b, &ODataQuery::default())
        .await
        .expect("list failed");
    assert_eq!(page.items.len(), 0, "Tenant B must not see Tenant A chats");
}

#[tokio::test]
async fn list_chats_cross_owner_returns_empty() {
    let db = inmem_db().await;
    let svc = build_service(db);

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    // User A creates a chat
    svc.create_chat(
        &ctx_a,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: Some("User A chat".to_owned()),
            is_temporary: false,
        },
    )
    .await
    .expect("create failed");

    // User B (same tenant) lists — should see nothing (owner_id constraint filters)
    let page = svc
        .list_chats(&ctx_b, &ODataQuery::default())
        .await
        .expect("list failed");
    assert_eq!(page.items.len(), 0, "User B must not see User A chats");
}

#[tokio::test]
async fn get_chat_cross_tenant_not_found() {
    let db = inmem_db().await;
    let svc = build_service(db);

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let ctx_a = test_security_ctx(tenant_a);
    let ctx_b = test_security_ctx(tenant_b);

    let created = svc
        .create_chat(
            &ctx_a,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("Tenant A chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    // Tenant B tries to get Tenant A's chat — should fail
    let result = svc.get_chat(&ctx_b, created.id).await;
    assert!(result.is_err(), "Cross-tenant get must fail");
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-tenant access"
    );
}

#[tokio::test]
async fn delete_chat_cross_owner_not_found() {
    let db = inmem_db().await;
    let svc = build_service(db);

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    let created = svc
        .create_chat(
            &ctx_a,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("User A chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    // User B (same tenant) tries to delete User A's chat — should fail
    let result = svc.delete_chat(&ctx_b, created.id).await;
    assert!(result.is_err(), "Cross-owner delete must fail");
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-owner delete"
    );
}

// ── Message Count Tests ──

#[tokio::test]
async fn get_chat_message_count_reflects_inserted_messages() {
    use crate::domain::repos::{
        InsertAssistantMessageParams, InsertUserMessageParams,
        MessageRepository as MessageRepoTrait,
    };
    use crate::infra::db::repo::message_repo::MessageRepository as OrmMessageRepository;
    use modkit_security::AccessScope;

    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    }));

    let svc = ChatService::new(
        Arc::clone(&db_provider),
        Arc::clone(&chat_repo),
        mock_thread_summary_repo(),
        mock_enforcer(),
        mock_model_resolver(),
    );

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let created = svc
        .create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("Chat with messages".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    // Insert messages via repo directly
    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn failed");
    let message_repo = OrmMessageRepository::new(modkit_db::odata::LimitCfg {
        default: 20,
        max: 100,
    });
    let request_id = Uuid::new_v4();

    message_repo
        .insert_user_message(
            &conn,
            &scope,
            InsertUserMessageParams {
                id: Uuid::now_v7(),
                tenant_id,
                chat_id: created.id,
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
                chat_id: created.id,
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

    // get_chat should report message_count = 2
    let detail = svc
        .get_chat(&ctx, created.id)
        .await
        .expect("get_chat failed");
    assert_eq!(detail.message_count, 2, "get_chat should report 2 messages");

    // list_chats should also report message_count = 2
    let page = svc
        .list_chats(&ctx, &ODataQuery::default())
        .await
        .expect("list_chats failed");
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].message_count, 2,
        "list_chats should report 2 messages"
    );
}

// ── Pagination Tests ──

#[tokio::test]
async fn list_chats_pagination_forward_cursor() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    // Create 5 chats
    for i in 0..5 {
        svc.create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some(format!("Chat {i}")),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");
    }

    // Page 1: request 2 items
    let query = ODataQuery::new().with_limit(2);
    let page1 = svc
        .list_chats(&ctx, &query)
        .await
        .expect("list_chats page 1 failed");

    assert_eq!(page1.items.len(), 2, "Page 1 should have 2 items");
    assert!(
        page1.page_info.next_cursor.is_some(),
        "Page 1 must have next_cursor (3 more items remain)"
    );
    assert!(
        page1.page_info.prev_cursor.is_none(),
        "Page 1 must not have prev_cursor (first page)"
    );

    // Page 2: use next_cursor
    let cursor = modkit_odata::CursorV1::decode(page1.page_info.next_cursor.as_ref().unwrap())
        .expect("decode cursor failed");
    let query2 = ODataQuery::new().with_limit(2).with_cursor(cursor);
    let page2 = svc
        .list_chats(&ctx, &query2)
        .await
        .expect("list_chats page 2 failed");

    assert_eq!(page2.items.len(), 2, "Page 2 should have 2 items");
    assert!(
        page2.page_info.next_cursor.is_some(),
        "Page 2 must have next_cursor (1 more item remains)"
    );
    assert!(
        page2.page_info.prev_cursor.is_some(),
        "Page 2 must have prev_cursor"
    );

    // Page 3: use next_cursor — should return the last item
    let cursor = modkit_odata::CursorV1::decode(page2.page_info.next_cursor.as_ref().unwrap())
        .expect("decode cursor failed");
    let query3 = ODataQuery::new().with_limit(2).with_cursor(cursor);
    let page3 = svc
        .list_chats(&ctx, &query3)
        .await
        .expect("list_chats page 3 failed");

    assert_eq!(page3.items.len(), 1, "Page 3 should have 1 item");
    assert!(
        page3.page_info.next_cursor.is_none(),
        "Page 3 must not have next_cursor (no more items)"
    );

    // All IDs across pages must be unique and cover all 5 chats
    let mut all_ids: Vec<Uuid> = page1
        .items
        .iter()
        .chain(page2.items.iter())
        .chain(page3.items.iter())
        .map(|c| c.id)
        .collect();
    assert_eq!(all_ids.len(), 5, "Total items across pages should be 5");
    all_ids.sort();
    all_ids.dedup();
    assert_eq!(all_ids.len(), 5, "All IDs must be unique");
}

#[tokio::test]
async fn list_chats_pagination_no_cursor_when_all_fit() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    // Create 3 chats, request page of 10
    for i in 0..3 {
        svc.create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some(format!("Chat {i}")),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");
    }

    let query = ODataQuery::new().with_limit(10);
    let page = svc
        .list_chats(&ctx, &query)
        .await
        .expect("list_chats failed");

    assert_eq!(page.items.len(), 3);
    assert!(
        page.page_info.next_cursor.is_none(),
        "No next_cursor when all items fit in a single page"
    );
    assert!(
        page.page_info.prev_cursor.is_none(),
        "No prev_cursor on the first (and only) page"
    );
}

#[tokio::test]
async fn list_chats_pagination_backward_cursor() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    // Create 5 chats
    for i in 0..5 {
        svc.create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some(format!("Chat {i}")),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");
    }

    // Page 1 forward (2 items)
    let query = ODataQuery::new().with_limit(2);
    let page1 = svc
        .list_chats(&ctx, &query)
        .await
        .expect("list_chats page 1 failed");
    assert_eq!(page1.items.len(), 2);

    // Page 2 forward
    let cursor = modkit_odata::CursorV1::decode(page1.page_info.next_cursor.as_ref().unwrap())
        .expect("decode cursor failed");
    let query2 = ODataQuery::new().with_limit(2).with_cursor(cursor);
    let page2 = svc
        .list_chats(&ctx, &query2)
        .await
        .expect("list_chats page 2 failed");
    assert_eq!(page2.items.len(), 2);

    // Now go backward from page 2 using prev_cursor
    let prev = modkit_odata::CursorV1::decode(page2.page_info.prev_cursor.as_ref().unwrap())
        .expect("decode prev cursor failed");
    let query_back = ODataQuery::new().with_limit(2).with_cursor(prev);
    let page_back = svc
        .list_chats(&ctx, &query_back)
        .await
        .expect("list_chats backward failed");

    assert_eq!(
        page_back.items.len(),
        page1.items.len(),
        "Backward page should have same count as page 1"
    );
    // Items should match page 1 (same IDs, same order)
    let back_ids: Vec<Uuid> = page_back.items.iter().map(|c| c.id).collect();
    let page1_ids: Vec<Uuid> = page1.items.iter().map(|c| c.id).collect();
    assert_eq!(
        back_ids, page1_ids,
        "Backward navigation must return to page 1 items"
    );
}

// ── Tenant-only AuthZ: user isolation via ensure_owner ──

#[tokio::test]
async fn list_chats_tenant_only_authz_cross_owner_returns_empty() {
    let db = inmem_db().await;
    let svc = build_service_tenant_only_authz(db);

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    // User A creates a chat (AuthZ returns only tenant constraint, no owner_id)
    svc.create_chat(
        &ctx_a,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: Some("User A chat".to_owned()),
            is_temporary: false,
        },
    )
    .await
    .expect("create failed");

    // User B (same tenant) lists — must still see nothing thanks to ensure_owner
    let page = svc
        .list_chats(&ctx_b, &ODataQuery::default())
        .await
        .expect("list failed");
    assert_eq!(
        page.items.len(),
        0,
        "User B must not see User A chats even when AuthZ returns tenant-only constraints"
    );

    // User A sees their own chat
    let page = svc
        .list_chats(&ctx_a, &ODataQuery::default())
        .await
        .expect("list failed");
    assert_eq!(page.items.len(), 1, "User A must see their own chat");
}

#[tokio::test]
async fn get_chat_tenant_only_authz_cross_owner_not_found() {
    let db = inmem_db().await;
    let svc = build_service_tenant_only_authz(db);

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    let created = svc
        .create_chat(
            &ctx_a,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("User A chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    // User B (same tenant) tries to get User A's chat — must fail
    let result = svc.get_chat(&ctx_b, created.id).await;
    assert!(
        result.is_err(),
        "Cross-owner get must fail with tenant-only authz"
    );
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-owner access with tenant-only authz"
    );
}

#[tokio::test]
async fn delete_chat_tenant_only_authz_cross_owner_not_found() {
    let db = inmem_db().await;
    let svc = build_service_tenant_only_authz(db);

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    let created = svc
        .create_chat(
            &ctx_a,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("User A chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    // User B (same tenant) tries to delete — must fail
    let result = svc.delete_chat(&ctx_b, created.id).await;
    assert!(
        result.is_err(),
        "Cross-owner delete must fail with tenant-only authz"
    );
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-owner delete with tenant-only authz"
    );
}

#[tokio::test]
async fn update_chat_tenant_only_authz_cross_owner_not_found() {
    let db = inmem_db().await;
    let svc = build_service_tenant_only_authz(db);

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    let created = svc
        .create_chat(
            &ctx_a,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some("User A chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");

    // User B (same tenant) tries to update User A's chat — must fail
    let result = svc
        .update_chat(
            &ctx_b,
            created.id,
            ChatPatch {
                title: Some(Some("Hijacked".to_owned())),
            },
        )
        .await;
    assert!(
        result.is_err(),
        "Cross-owner update must fail with tenant-only authz"
    );
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-owner update with tenant-only authz"
    );
}

// ── Filter by title tests ──

#[tokio::test]
async fn list_chats_filter_contains_title() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    for title in ["Q3 Financial Report", "Weekly Standup", "Report Draft"] {
        svc.create_chat(
            &ctx,
            NewChat {
                model: Some("gpt-5.2".to_owned()),
                title: Some(title.to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create failed");
    }

    // contains(title, 'Report') should match 2 chats
    let filter = modkit_odata::ast::Expr::Function(
        "contains".to_owned(),
        vec![
            modkit_odata::ast::Expr::Identifier("title".to_owned()),
            modkit_odata::ast::Expr::Value(modkit_odata::ast::Value::String("Report".to_owned())),
        ],
    );
    let query = ODataQuery::default().with_filter(filter);
    let page = svc.list_chats(&ctx, &query).await.expect("list failed");

    assert_eq!(page.items.len(), 2, "Expected 2 chats matching 'Report'");
    assert!(
        page.items
            .iter()
            .all(|c| c.title.as_deref().unwrap_or("").contains("Report")),
        "All results must contain 'Report' in title"
    );
}

#[tokio::test]
async fn list_chats_filter_contains_title_no_match() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    svc.create_chat(
        &ctx,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: Some("Weekly Standup".to_owned()),
            is_temporary: false,
        },
    )
    .await
    .expect("create failed");

    let filter = modkit_odata::ast::Expr::Function(
        "contains".to_owned(),
        vec![
            modkit_odata::ast::Expr::Identifier("title".to_owned()),
            modkit_odata::ast::Expr::Value(modkit_odata::ast::Value::String(
                "xyz_nonexistent".to_owned(),
            )),
        ],
    );
    let query = ODataQuery::default().with_filter(filter);
    let page = svc.list_chats(&ctx, &query).await.expect("list failed");

    assert_eq!(page.items.len(), 0, "No chats should match");
}

#[tokio::test]
async fn list_chats_filter_contains_title_excludes_null_titles() {
    let db = inmem_db().await;
    let svc = build_service(db);
    let ctx = test_security_ctx(Uuid::new_v4());

    // Chat with title
    svc.create_chat(
        &ctx,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: Some("Q3 Report".to_owned()),
            is_temporary: false,
        },
    )
    .await
    .expect("create failed");

    // Chat without title (NULL)
    svc.create_chat(
        &ctx,
        NewChat {
            model: Some("gpt-5.2".to_owned()),
            title: None,
            is_temporary: false,
        },
    )
    .await
    .expect("create failed");

    let filter = modkit_odata::ast::Expr::Function(
        "contains".to_owned(),
        vec![
            modkit_odata::ast::Expr::Identifier("title".to_owned()),
            modkit_odata::ast::Expr::Value(modkit_odata::ast::Value::String("Report".to_owned())),
        ],
    );
    let query = ODataQuery::default().with_filter(filter);
    let page = svc.list_chats(&ctx, &query).await.expect("list failed");

    assert_eq!(page.items.len(), 1, "Only the titled chat should match");
    assert_eq!(
        page.items[0].title.as_deref(),
        Some("Q3 Report"),
        "Matched chat must be the one with title"
    );
}
