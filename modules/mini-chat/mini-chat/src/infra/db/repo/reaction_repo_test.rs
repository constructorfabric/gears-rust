use std::sync::Arc;

use modkit_db::DBProvider;
use modkit_db::odata::LimitCfg;
use modkit_db::secure::secure_insert;
use modkit_security::AccessScope;
use sea_orm::Set;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::models::ReactionKind;
use crate::domain::repos::{
    InsertAssistantMessageParams, MessageRepository as _, ReactionRepository as _,
    UpsertReactionParams,
};
use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};
use crate::infra::db::repo::message_repo::MessageRepository;
use crate::infra::db::repo::reaction_repo::ReactionRepository;

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

/// Insert an assistant message row (required by FK constraints on `message_reactions`).
/// Returns the message ID.
async fn insert_assistant_message(db: &Db, tenant_id: Uuid, chat_id: Uuid) -> Uuid {
    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let msg_id = Uuid::now_v7();
    repo.insert_assistant_message(
        &conn,
        &scope(),
        InsertAssistantMessageParams {
            id: msg_id,
            tenant_id,
            chat_id,
            request_id: Uuid::new_v4(),
            content: "test response".to_owned(),
            input_tokens: None,
            output_tokens: None,
            model: None,
            provider_response_id: None,
        },
    )
    .await
    .expect("insert assistant message");
    msg_id
}

// ════════════════════════════════════════════════════════════════════
// Upsert tests
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn upsert_inserts_on_first_call() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let msg_id = insert_assistant_message(&db, tenant_id, chat_id).await;

    let repo = ReactionRepository;
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let model = repo
        .upsert(
            &conn,
            &scope,
            UpsertReactionParams {
                id: Uuid::now_v7(),
                tenant_id,
                message_id: msg_id,
                user_id,
                reaction: ReactionKind::Like,
            },
        )
        .await
        .expect("upsert");

    assert_eq!(model.message_id, msg_id);
    assert_eq!(model.user_id, user_id);
    assert_eq!(model.reaction, "like");
    assert_eq!(model.tenant_id, tenant_id);
}

#[tokio::test]
async fn upsert_updates_on_conflict() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let msg_id = insert_assistant_message(&db, tenant_id, chat_id).await;

    let repo = ReactionRepository;
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    // First: insert "like"
    let first = repo
        .upsert(
            &conn,
            &scope,
            UpsertReactionParams {
                id: Uuid::now_v7(),
                tenant_id,
                message_id: msg_id,
                user_id,
                reaction: ReactionKind::Like,
            },
        )
        .await
        .expect("first upsert");
    assert_eq!(first.reaction, "like");

    // Second: upsert "dislike" — same (message_id, user_id), should update
    let second = repo
        .upsert(
            &conn,
            &scope,
            UpsertReactionParams {
                id: Uuid::now_v7(),
                tenant_id,
                message_id: msg_id,
                user_id,
                reaction: ReactionKind::Dislike,
            },
        )
        .await
        .expect("second upsert");

    assert_eq!(second.reaction, "dislike");
    assert_eq!(second.message_id, msg_id);
    assert_eq!(second.user_id, user_id);
    // created_at should be updated (newer)
    assert!(second.created_at >= first.created_at);
}

#[tokio::test]
async fn upsert_different_users_coexist() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let msg_id = insert_assistant_message(&db, tenant_id, chat_id).await;

    let repo = ReactionRepository;
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    let r_a = repo
        .upsert(
            &conn,
            &scope,
            UpsertReactionParams {
                id: Uuid::now_v7(),
                tenant_id,
                message_id: msg_id,
                user_id: user_a,
                reaction: ReactionKind::Like,
            },
        )
        .await
        .expect("upsert A");

    let r_b = repo
        .upsert(
            &conn,
            &scope,
            UpsertReactionParams {
                id: Uuid::now_v7(),
                tenant_id,
                message_id: msg_id,
                user_id: user_b,
                reaction: ReactionKind::Dislike,
            },
        )
        .await
        .expect("upsert B");

    // Both reactions exist independently
    assert_eq!(r_a.reaction, "like");
    assert_eq!(r_b.reaction, "dislike");
    assert_ne!(r_a.id, r_b.id);
}

#[tokio::test]
async fn delete_returns_true_when_exists() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let msg_id = insert_assistant_message(&db, tenant_id, chat_id).await;

    let repo = ReactionRepository;
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant_id);

    repo.upsert(
        &conn,
        &scope,
        UpsertReactionParams {
            id: Uuid::now_v7(),
            tenant_id,
            message_id: msg_id,
            user_id,
            reaction: ReactionKind::Like,
        },
    )
    .await
    .expect("upsert");

    let deleted = repo
        .delete_by_message_and_user(&conn, &scope, msg_id, user_id)
        .await
        .expect("delete");
    assert!(deleted, "should report deletion");

    // Second delete should return false (nothing to delete)
    let deleted_again = repo
        .delete_by_message_and_user(&conn, &scope, msg_id, user_id)
        .await
        .expect("delete again");
    assert!(!deleted_again, "idempotent: nothing left to delete");
}

// ════════════════════════════════════════════════════════════════════
// Tenant scope isolation
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn upsert_cross_tenant_denied() {
    let db = test_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    insert_chat(&db, tenant_a, chat_id).await;
    let msg_id = insert_assistant_message(&db, tenant_a, chat_id).await;

    let repo = ReactionRepository;
    let conn = db.conn().unwrap();

    // Attempt to upsert with tenant_a data but tenant_b scope → denied
    let scope_b = AccessScope::for_tenant(tenant_b);
    let result = repo
        .upsert(
            &conn,
            &scope_b,
            UpsertReactionParams {
                id: Uuid::now_v7(),
                tenant_id: tenant_a,
                message_id: msg_id,
                user_id,
                reaction: ReactionKind::Like,
            },
        )
        .await;

    assert!(
        result.is_err(),
        "upsert with mismatched tenant scope must fail"
    );
}

#[tokio::test]
async fn delete_cross_tenant_no_effect() {
    let db = test_db().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    insert_chat(&db, tenant_a, chat_id).await;
    let msg_id = insert_assistant_message(&db, tenant_a, chat_id).await;

    let repo = ReactionRepository;
    let conn = db.conn().unwrap();

    // Insert reaction under tenant_a
    let scope_a = AccessScope::for_tenant(tenant_a);
    repo.upsert(
        &conn,
        &scope_a,
        UpsertReactionParams {
            id: Uuid::now_v7(),
            tenant_id: tenant_a,
            message_id: msg_id,
            user_id,
            reaction: ReactionKind::Like,
        },
    )
    .await
    .expect("upsert");

    // Attempt delete with tenant_b scope — should not affect tenant_a's data
    let scope_b = AccessScope::for_tenant(tenant_b);
    let deleted = repo
        .delete_by_message_and_user(&conn, &scope_b, msg_id, user_id)
        .await
        .expect("delete cross-tenant");
    assert!(
        !deleted,
        "cross-tenant delete must not affect other tenant's reaction"
    );

    // Verify reaction still exists under tenant_a
    let deleted_a = repo
        .delete_by_message_and_user(&conn, &scope_a, msg_id, user_id)
        .await
        .expect("delete own tenant");
    assert!(deleted_a, "reaction must still exist under original tenant");
}
