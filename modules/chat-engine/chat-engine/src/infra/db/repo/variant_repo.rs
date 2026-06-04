//! SeaORM-backed implementation of
//! [`VariantRepo`](crate::domain::service::variant_service::VariantRepo).
//!
//! The trait lives in the domain layer alongside [`VariantService`]; the
//! impl below threads the modkit-db `DBProvider` through the same handle the
//! migration runner uses, so reads and writes always land on the canonical
//! connection rather than a sibling pool.
//
// @cpt-cf-chat-engine-infra-variant-repo:p6

use std::sync::Arc;

use async_trait::async_trait;
use modkit_db::secure::{
    AccessScope, SecureEntityExt, SecureInsertExt, SecureUpdateExt, TxConfig,
};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryOrder};
use serde_json::{Value as JsonValue, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::Message;
use crate::domain::service::variant_service::VariantRepo;
use crate::infra::db::repo::ChatEngineDb;

/// Sea-ORM-backed implementation of [`VariantRepo`].
///
/// Holds the modkit-db `DBProvider` so every query runs against the same
/// connection the migration runner used. `messages` and `sessions` are both
/// marked `#[secure(unrestricted)]`; the secure wrappers run with
/// `AccessScope::allow_all()` and exist only to expose a `&impl DBRunner`
/// execution path.
pub struct SeaVariantRepo {
    db: Arc<ChatEngineDb>,
}

impl SeaVariantRepo {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl VariantRepo for SeaVariantRepo {
    async fn list_siblings(
        &self,
        session_id: Uuid,
        parent_message_id: Option<Uuid>,
    ) -> Result<Vec<Message>> {
        use crate::infra::db::entity::message::{self as message_entity, Entity as MessageEntity};

        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let parent_cond = match parent_message_id {
            Some(p) => Condition::all().add(message_entity::Column::ParentMessageId.eq(p)),
            None => Condition::all().add(message_entity::Column::ParentMessageId.is_null()),
        };
        let rows = MessageEntity::find()
            .order_by_asc(message_entity::Column::VariantIndex)
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(message_entity::Column::SessionId.eq(session_id)))
            .filter(parent_cond)
            .all(&conn)
            .await?;
        Ok(rows.into_iter().map(Message::from).collect())
    }

    async fn insert_user_and_assistant_stub_for_branch(
        &self,
        session_id: Uuid,
        parent_message_id: Uuid,
        content: JsonValue,
        file_ids: Option<Vec<Uuid>>,
    ) -> Result<(Uuid, i32, Uuid)> {
        use crate::infra::db::entity::message as message_entity;
        use crate::infra::db::{
            VARIANT_INDEX_MAX_RETRIES, compute_next_variant_index, is_variant_unique_violation,
        };
        use sea_orm::ActiveValue::Set;

        // SELECT MAX(variant_index)+1 and the matching INSERT run in the
        // SAME SERIALIZABLE transaction, with the whole pair retried under
        // `VARIANT_INDEX_MAX_RETRIES` on
        // `uq_messages_session_parent_variant` collisions. The prior
        // implementation used `assign_variant_index` (its own
        // transaction) followed by a separate INSERT transaction, which
        // left a race window between the two for concurrent callers.
        let file_ids_json = file_ids
            .as_ref()
            .filter(|ids| !ids.is_empty())
            .and_then(|ids| serde_json::to_value(ids).ok());

        let mut last_err: Option<ChatEngineError> = None;
        for _attempt in 0..VARIANT_INDEX_MAX_RETRIES {
            let user_message_id = Uuid::new_v4();
            let assistant_message_id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            let content_attempt = content.clone();
            let file_ids_attempt = file_ids_json.clone();

            let outcome: Result<i32> = self
                .db
                .transaction_with_config(TxConfig::serializable(), move |tx| {
                    Box::pin(async move {
                        let user_variant_index =
                            compute_next_variant_index(tx, session_id, Some(parent_message_id))
                                .await?;
                        let scope = AccessScope::allow_all();
                        let user_active = message_entity::ActiveModel {
                            message_id: Set(user_message_id),
                            session_id: Set(session_id),
                            parent_message_id: Set(Some(parent_message_id)),
                            role: Set("user".to_string()),
                            content: Set(content_attempt),
                            file_ids: Set(file_ids_attempt),
                            variant_index: Set(user_variant_index),
                            is_active: Set(true),
                            is_complete: Set(true),
                            is_hidden_from_user: Set(false),
                            is_hidden_from_backend: Set(false),
                            metadata: Set(None),
                            created_at: Set(now),
                        };
                        let assistant_active = message_entity::ActiveModel {
                            message_id: Set(assistant_message_id),
                            session_id: Set(session_id),
                            parent_message_id: Set(Some(user_message_id)),
                            role: Set("assistant".to_string()),
                            content: Set(json!({ "text": "" })),
                            file_ids: Set(None),
                            variant_index: Set(0),
                            is_active: Set(true),
                            is_complete: Set(false),
                            is_hidden_from_user: Set(false),
                            is_hidden_from_backend: Set(false),
                            metadata: Set(None),
                            created_at: Set(now),
                        };
                        message_entity::Entity::insert(user_active)
                            .secure()
                            .scope_unchecked(&scope)?
                            .exec(tx)
                            .await?;
                        message_entity::Entity::insert(assistant_active)
                            .secure()
                            .scope_unchecked(&scope)?
                            .exec(tx)
                            .await?;
                        Ok(user_variant_index)
                    })
                })
                .await;

            match outcome {
                Ok(user_variant_index) => {
                    return Ok((user_message_id, user_variant_index, assistant_message_id));
                }
                Err(e) => {
                    // Drill down into the original `DbErr` (if any) so the
                    // retry classifier still sees the structured SQLSTATE,
                    // not just a flattened string.
                    if let Some(db_err) = chat_engine_db_err(&e) {
                        if !is_variant_unique_violation(db_err) {
                            return Err(e);
                        }
                    } else {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }

        let base = format!(
            "variant index allocation contended; exhausted {VARIANT_INDEX_MAX_RETRIES} retries"
        );
        Err(ChatEngineError::conflict(match last_err {
            Some(e) => format!("{base}: {e}"),
            None => base,
        }))
    }

    async fn ancestor_chain(
        &self,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<Vec<Uuid>> {
        use crate::infra::db::entity::message::{self as message_entity, Entity as MessageEntity};

        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let mut chain = Vec::new();
        let mut cursor: Option<Uuid> = Some(message_id);
        let mut guard = 0_usize;
        while let Some(cur) = cursor {
            chain.push(cur);
            guard += 1;
            if guard > 10_000 {
                return Err(ChatEngineError::internal(
                    "ancestor_chain exceeded depth guard",
                ));
            }
            let row = MessageEntity::find_by_id(cur)
                .secure()
                .scope_with(&scope)
                .filter(
                    Condition::all().add(message_entity::Column::SessionId.eq(session_id)),
                )
                .one(&conn)
                .await?;
            cursor = match row {
                Some(r) => r.parent_message_id,
                None => return Err(ChatEngineError::not_found("message", cur)),
            };
        }
        Ok(chain)
    }

    async fn collect_descendants(
        &self,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<Vec<Uuid>> {
        use crate::infra::db::entity::message::{self as message_entity, Entity as MessageEntity};

        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let mut out: Vec<Uuid> = Vec::new();
        let mut frontier: Vec<Uuid> = vec![message_id];
        while !frontier.is_empty() {
            let children: Vec<Uuid> = MessageEntity::find()
                .secure()
                .scope_with(&scope)
                .filter(
                    Condition::all()
                        .add(message_entity::Column::SessionId.eq(session_id))
                        .add(message_entity::Column::ParentMessageId.is_in(frontier.clone())),
                )
                .all(&conn)
                .await?
                .into_iter()
                .map(|m| m.message_id)
                .collect();
            if children.is_empty() {
                break;
            }
            out.extend(&children);
            frontier = children;
        }
        Ok(out)
    }

    async fn apply_active_flips(
        &self,
        session_id: Uuid,
        activate_ids: Vec<Uuid>,
        deactivate_ids: Vec<Uuid>,
    ) -> Result<()> {
        use crate::infra::db::entity::message::{self as message_entity, Entity as MessageEntity};

        // Defense in depth: drop any id that appears in both lists from
        // the deactivate set. The SQL below applies activation first
        // and deactivation second, so an overlap would silently flip
        // is_active=false on a node the caller asked to activate.
        let activate_set: std::collections::HashSet<Uuid> =
            activate_ids.iter().copied().collect();
        let deactivate_ids: Vec<Uuid> = deactivate_ids
            .into_iter()
            .filter(|id| !activate_set.contains(id))
            .collect();

        self.db
            .transaction_with_config(TxConfig::serializable(), move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    if !activate_ids.is_empty() {
                        MessageEntity::update_many()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all()
                                    .add(message_entity::Column::SessionId.eq(session_id))
                                    .add(
                                        message_entity::Column::MessageId.is_in(activate_ids.clone()),
                                    ),
                            )
                            .col_expr(message_entity::Column::IsActive, Expr::value(true))
                            .exec(tx)
                            .await?;
                    }
                    if !deactivate_ids.is_empty() {
                        MessageEntity::update_many()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all()
                                    .add(message_entity::Column::SessionId.eq(session_id))
                                    .add(
                                        message_entity::Column::MessageId
                                            .is_in(deactivate_ids.clone()),
                                    ),
                            )
                            .col_expr(message_entity::Column::IsActive, Expr::value(false))
                            .exec(tx)
                            .await?;
                    }
                    Ok(())
                })
            })
            .await
    }

    async fn update_session_type(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        new_session_type_id: Uuid,
        new_capabilities: JsonValue,
    ) -> Result<crate::infra::db::entity::session::Model> {
        use crate::infra::db::entity::session::{self as session_entity, Entity as SessionEntity};

        let tenant_id = tenant_id.to_owned();
        let user_id = user_id.to_owned();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    let owned_cond = Condition::all()
                        .add(session_entity::Column::SessionId.eq(session_id))
                        .add(session_entity::Column::TenantId.eq(tenant_id.clone()))
                        .add(session_entity::Column::UserId.eq(user_id.clone()));

                    let _existing = SessionEntity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(owned_cond.clone())
                        .one(tx)
                        .await?
                        .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;

                    let now = OffsetDateTime::now_utc();
                    SessionEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(owned_cond.clone())
                        .col_expr(
                            session_entity::Column::SessionTypeId,
                            Expr::value(Some(new_session_type_id)),
                        )
                        .col_expr(
                            session_entity::Column::EnabledCapabilities,
                            Expr::value(Some(new_capabilities.clone())),
                        )
                        .col_expr(session_entity::Column::UpdatedAt, Expr::value(now))
                        .exec(tx)
                        .await?;

                    let updated = SessionEntity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(owned_cond)
                        .one(tx)
                        .await?
                        .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
                    Ok(updated)
                })
            })
            .await
    }
}

/// Reach into a `ChatEngineError` to recover the underlying `sea_orm::DbErr`
/// for retry classification. Only the `Internal { source }` branch carries
/// one; everything else short-circuits the retry.
fn chat_engine_db_err(err: &ChatEngineError) -> Option<&sea_orm::DbErr> {
    let ChatEngineError::Internal { source, .. } = err else {
        return None;
    };
    let source = source.as_ref()?;
    source
        .downcast_ref::<sea_orm::DbErr>()
        .or_else(|| {
            // The `From<DbError>` impl wraps `DbError::Sea(DbErr)` directly
            // into `ChatEngineError::Internal`; the raw downcast above
            // already covers that path. `DbError::Other(anyhow)` errors
            // (used by the transaction helpers when a domain error
            // bubbles through) appear as `modkit_db::DbError`, not as a
            // bare `DbErr` — so peek through that wrapper too.
            source
                .downcast_ref::<modkit_db::DbError>()
                .and_then(|dbe| match dbe {
                    modkit_db::DbError::Sea(inner) => Some(inner),
                    _ => None,
                })
        })
}
