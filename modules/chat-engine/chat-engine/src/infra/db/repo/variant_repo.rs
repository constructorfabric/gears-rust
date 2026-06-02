//! SeaORM-backed implementation of
//! [`VariantRepo`](crate::domain::service::variant_service::VariantRepo).
//!
//! The trait lives in the domain layer alongside [`VariantService`]; the
//! impl below carries a `DatabaseConnection` and so belongs in `infra/`
//! per the `#[domain_model]` DDD-light boundary.
//
// @cpt-cf-chat-engine-infra-variant-repo:p6

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::Message;
use crate::domain::service::variant_service::VariantRepo;

/// Sea-ORM-backed implementation of [`VariantRepo`].
pub struct SeaVariantRepo {
    db: sea_orm::DatabaseConnection,
}

impl SeaVariantRepo {
    #[must_use]
    pub fn new(db: sea_orm::DatabaseConnection) -> Self {
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
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let mut query = MessageEntity::find()
            .filter(message_entity::Column::SessionId.eq(session_id))
            .order_by_asc(message_entity::Column::VariantIndex);
        query = match parent_message_id {
            Some(p) => query.filter(message_entity::Column::ParentMessageId.eq(p)),
            None => query.filter(message_entity::Column::ParentMessageId.is_null()),
        };
        let rows = query.all(&self.db).await?;
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
            VARIANT_INDEX_MAX_RETRIES, compute_next_variant_index,
            is_variant_unique_violation,
        };
        use sea_orm::{
            AccessMode, ActiveModelTrait, ActiveValue::Set, IsolationLevel, TransactionError,
            TransactionTrait,
        };

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

        let mut last_err: Option<sea_orm::DbErr> = None;
        for _attempt in 0..VARIANT_INDEX_MAX_RETRIES {
            let user_message_id = Uuid::new_v4();
            let assistant_message_id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            let content_attempt = content.clone();
            let file_ids_attempt = file_ids_json.clone();

            let outcome: std::result::Result<i32, TransactionError<sea_orm::DbErr>> = self
                .db
                .transaction_with_config::<_, i32, sea_orm::DbErr>(
                    move |txn| {
                        Box::pin(async move {
                            let user_variant_index = compute_next_variant_index(
                                txn,
                                session_id,
                                Some(parent_message_id),
                            )
                            .await?;
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
                            user_active.insert(txn).await?;
                            assistant_active.insert(txn).await?;
                            Ok(user_variant_index)
                        })
                    },
                    Some(IsolationLevel::Serializable),
                    Some(AccessMode::ReadWrite),
                )
                .await;

            match outcome {
                Ok(user_variant_index) => {
                    return Ok((user_message_id, user_variant_index, assistant_message_id));
                }
                Err(TransactionError::Transaction(e))
                | Err(TransactionError::Connection(e)) => {
                    if !is_variant_unique_violation(&e) {
                        return Err(e.into());
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
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

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
                .filter(message_entity::Column::SessionId.eq(session_id))
                .one(&self.db)
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
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let mut out: Vec<Uuid> = Vec::new();
        let mut frontier: Vec<Uuid> = vec![message_id];
        while !frontier.is_empty() {
            let children: Vec<Uuid> = MessageEntity::find()
                .filter(message_entity::Column::SessionId.eq(session_id))
                .filter(message_entity::Column::ParentMessageId.is_in(frontier.clone()))
                .all(&self.db)
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
        use sea_orm::{
            AccessMode, ColumnTrait, EntityTrait, IsolationLevel, QueryFilter, TransactionError,
            TransactionTrait,
        };

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

        let outcome: std::result::Result<(), TransactionError<sea_orm::DbErr>> = self
            .db
            .transaction_with_config::<_, (), sea_orm::DbErr>(
                move |txn| {
                    Box::pin(async move {
                        if !activate_ids.is_empty() {
                            MessageEntity::update_many()
                                .filter(
                                    message_entity::Column::SessionId.eq(session_id),
                                )
                                .filter(
                                    message_entity::Column::MessageId.is_in(activate_ids.clone()),
                                )
                                .col_expr(
                                    message_entity::Column::IsActive,
                                    sea_orm::sea_query::Expr::value(true),
                                )
                                .exec(txn)
                                .await?;
                        }
                        if !deactivate_ids.is_empty() {
                            MessageEntity::update_many()
                                .filter(
                                    message_entity::Column::SessionId.eq(session_id),
                                )
                                .filter(
                                    message_entity::Column::MessageId.is_in(deactivate_ids.clone()),
                                )
                                .col_expr(
                                    message_entity::Column::IsActive,
                                    sea_orm::sea_query::Expr::value(false),
                                )
                                .exec(txn)
                                .await?;
                        }
                        Ok(())
                    })
                },
                Some(IsolationLevel::Serializable),
                Some(AccessMode::ReadWrite),
            )
            .await;
        match outcome {
            Ok(()) => Ok(()),
            Err(TransactionError::Transaction(e)) | Err(TransactionError::Connection(e)) => {
                Err(e.into())
            }
        }
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
        use sea_orm::{
            ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter,
        };

        let row = SessionEntity::find()
            .filter(session_entity::Column::SessionId.eq(session_id))
            .filter(session_entity::Column::TenantId.eq(tenant_id.to_owned()))
            .filter(session_entity::Column::UserId.eq(user_id.to_owned()))
            .one(&self.db)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;

        let mut active: session_entity::ActiveModel = row.into();
        active.session_type_id = Set(Some(new_session_type_id));
        active.enabled_capabilities = Set(Some(new_capabilities));
        active.updated_at = Set(OffsetDateTime::now_utc());
        let updated = active.update(&self.db).await?;
        Ok(updated)
    }
}
