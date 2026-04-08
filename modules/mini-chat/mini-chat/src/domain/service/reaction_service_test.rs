use std::sync::Arc;

use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::{NewChat, ReactionKind};

use crate::domain::repos::{
    InsertAssistantMessageParams, InsertUserMessageParams, MessageRepository as MessageRepoTrait,
};
use crate::domain::service::test_helpers::{
    MockThreadSummaryRepo, inmem_db, mock_db_provider, mock_enforcer, mock_model_resolver,
    mock_tenant_only_enforcer, mock_thread_summary_repo, test_security_ctx,
    test_security_ctx_with_id,
};
use crate::infra::db::repo::chat_repo::ChatRepository as OrmChatRepository;
use crate::infra::db::repo::message_repo::MessageRepository as OrmMessageRepository;
use crate::infra::db::repo::reaction_repo::ReactionRepository as OrmReactionRepository;

use super::ReactionService;
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

fn build_reaction_service(
    db_provider: Arc<crate::domain::service::DbProvider>,
    chat_repo: Arc<OrmChatRepository>,
) -> ReactionService<OrmReactionRepository, OrmMessageRepository, OrmChatRepository> {
    let reaction_repo = Arc::new(OrmReactionRepository);
    let message_repo = Arc::new(OrmMessageRepository::new(limit_cfg()));
    ReactionService::new(
        db_provider,
        reaction_repo,
        message_repo,
        chat_repo,
        mock_enforcer(),
    )
}

fn build_reaction_service_tenant_only_authz(
    db_provider: Arc<crate::domain::service::DbProvider>,
    chat_repo: Arc<OrmChatRepository>,
) -> ReactionService<OrmReactionRepository, OrmMessageRepository, OrmChatRepository> {
    let reaction_repo = Arc::new(OrmReactionRepository);
    let message_repo = Arc::new(OrmMessageRepository::new(limit_cfg()));
    ReactionService::new(
        db_provider,
        reaction_repo,
        message_repo,
        chat_repo,
        mock_tenant_only_enforcer(),
    )
}

/// Create a chat and insert a user + assistant message pair.
/// Returns `(chat_id, user_msg_id, assistant_msg_id)`.
async fn setup_chat_with_messages(
    db_provider: &Arc<crate::domain::service::DbProvider>,
    chat_repo: &Arc<OrmChatRepository>,
    ctx: &modkit_security::SecurityContext,
    tenant_id: Uuid,
) -> (Uuid, Uuid, Uuid) {
    let chat_svc = build_chat_service(Arc::clone(db_provider), Arc::clone(chat_repo));

    let chat = chat_svc
        .create_chat(
            ctx,
            NewChat {
                model: None,
                title: Some("Test chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    let scope = AccessScope::for_tenant(tenant_id);
    let conn = db_provider.conn().expect("conn failed");
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
        .expect("insert_user_message failed");

    let assistant_msg_id = Uuid::now_v7();
    message_repo
        .insert_assistant_message(
            &conn,
            &scope,
            InsertAssistantMessageParams {
                id: assistant_msg_id,
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

    (chat.id, user_msg_id, assistant_msg_id)
}

// ── Tests ──

#[tokio::test]
async fn set_reaction_on_assistant_message() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let (chat_id, _user_msg_id, assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx, tenant_id).await;

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let reaction = reaction_svc
        .set_reaction(&ctx, chat_id, assistant_msg_id, "like")
        .await
        .expect("set_reaction failed");

    assert_eq!(reaction.message_id, assistant_msg_id);
    assert_eq!(reaction.kind, ReactionKind::Like);
}

#[tokio::test]
async fn set_reaction_on_user_message_rejected() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let (chat_id, user_msg_id, _assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx, tenant_id).await;

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let result = reaction_svc
        .set_reaction(&ctx, chat_id, user_msg_id, "like")
        .await;

    assert!(result.is_err(), "Should reject reaction on user message");
    assert!(
        matches!(
            result.unwrap_err(),
            DomainError::InvalidReactionTarget { .. }
        ),
        "Expected InvalidReactionTarget"
    );
}

#[tokio::test]
async fn set_reaction_upsert_replaces() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let (chat_id, _user_msg_id, assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx, tenant_id).await;

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    // First: set "like"
    let r1 = reaction_svc
        .set_reaction(&ctx, chat_id, assistant_msg_id, "like")
        .await
        .expect("set_reaction like failed");
    assert_eq!(r1.kind, ReactionKind::Like);

    // Second: set "dislike" — should replace
    let r2 = reaction_svc
        .set_reaction(&ctx, chat_id, assistant_msg_id, "dislike")
        .await
        .expect("set_reaction dislike failed");
    assert_eq!(r2.kind, ReactionKind::Dislike);
}

#[tokio::test]
async fn set_reaction_invalid_value_rejected() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let (chat_id, _user_msg_id, assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx, tenant_id).await;

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let result = reaction_svc
        .set_reaction(&ctx, chat_id, assistant_msg_id, "love")
        .await;

    assert!(result.is_err(), "Should reject invalid reaction value");
    assert!(
        matches!(result.unwrap_err(), DomainError::Validation { .. }),
        "Expected Validation error"
    );
}

#[tokio::test]
async fn delete_reaction_happy_path() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let (chat_id, _user_msg_id, assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx, tenant_id).await;

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    // Set reaction first
    reaction_svc
        .set_reaction(&ctx, chat_id, assistant_msg_id, "like")
        .await
        .expect("set_reaction failed");

    // Delete reaction
    reaction_svc
        .delete_reaction(&ctx, chat_id, assistant_msg_id)
        .await
        .expect("delete_reaction failed");
}

#[tokio::test]
async fn delete_reaction_idempotent() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let (chat_id, _user_msg_id, assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx, tenant_id).await;

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    // Delete without prior reaction — should still succeed
    reaction_svc
        .delete_reaction(&ctx, chat_id, assistant_msg_id)
        .await
        .expect("delete_reaction should be idempotent");
}

#[tokio::test]
async fn set_reaction_chat_not_found() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let reaction_svc = build_reaction_service(db_provider, chat_repo);

    let ctx = test_security_ctx(Uuid::new_v4());
    let random_chat_id = Uuid::new_v4();
    let random_msg_id = Uuid::new_v4();

    let result = reaction_svc
        .set_reaction(&ctx, random_chat_id, random_msg_id, "like")
        .await;

    assert!(result.is_err(), "Expected error for non-existent chat");
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound"
    );
}

#[tokio::test]
async fn set_reaction_message_not_found() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let ctx = test_security_ctx(tenant_id);

    let chat_svc = build_chat_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let chat = chat_svc
        .create_chat(
            &ctx,
            NewChat {
                model: None,
                title: Some("Test chat".to_owned()),
                is_temporary: false,
            },
        )
        .await
        .expect("create_chat failed");

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    let random_msg_id = Uuid::new_v4();
    let result = reaction_svc
        .set_reaction(&ctx, chat.id, random_msg_id, "like")
        .await;

    assert!(result.is_err(), "Expected error for non-existent message");
    assert!(
        matches!(result.unwrap_err(), DomainError::MessageNotFound { .. }),
        "Expected MessageNotFound"
    );
}

#[tokio::test]
async fn set_reaction_cross_tenant_rejected() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let ctx_a = test_security_ctx(tenant_a);
    let ctx_b = test_security_ctx(tenant_b);

    let (chat_id, _user_msg_id, assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx_a, tenant_a).await;

    let reaction_svc = build_reaction_service(Arc::clone(&db_provider), Arc::clone(&chat_repo));

    // Tenant B tries to react to Tenant A's chat message
    let result = reaction_svc
        .set_reaction(&ctx_b, chat_id, assistant_msg_id, "like")
        .await;

    assert!(result.is_err(), "Cross-tenant reaction must fail");
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-tenant access"
    );
}

// ── Tenant-only AuthZ: user isolation via ensure_owner ──

#[tokio::test]
async fn set_reaction_tenant_only_authz_cross_owner_not_found() {
    let db = inmem_db().await;
    let db_provider = mock_db_provider(db);
    let chat_repo = Arc::new(OrmChatRepository::new(limit_cfg()));

    let tenant_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let ctx_a = test_security_ctx_with_id(tenant_id, user_a);
    let ctx_b = test_security_ctx_with_id(tenant_id, user_b);

    // User A creates a chat with messages (using permissive enforcer for setup)
    let (chat_id, _user_msg_id, assistant_msg_id) =
        setup_chat_with_messages(&db_provider, &chat_repo, &ctx_a, tenant_id).await;

    // User B (same tenant) tries to react via tenant-only enforcer
    let reaction_svc =
        build_reaction_service_tenant_only_authz(Arc::clone(&db_provider), Arc::clone(&chat_repo));
    let result = reaction_svc
        .set_reaction(&ctx_b, chat_id, assistant_msg_id, "like")
        .await;

    assert!(
        result.is_err(),
        "Cross-owner reaction must fail with tenant-only authz"
    );
    assert!(
        matches!(result.unwrap_err(), DomainError::ChatNotFound { .. }),
        "Expected ChatNotFound for cross-owner access with tenant-only authz"
    );
}
