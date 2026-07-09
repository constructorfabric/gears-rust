//! `message_reactions` repository.
//!
//! Owns the persistence surface for per-`(message_id, user_id)` reactions
//! introduced by Phase 1 (migration 4). ADR-0020 specifies UPSERT semantics
//! on the composite primary key; this repo encapsulates the `INSERT ... ON
//! CONFLICT (message_id, user_id) DO UPDATE SET reaction_type = EXCLUDED,
//! updated_at = now()` query so the service layer never reaches into
//! Sea-ORM directly.
//!
//! The previous `reaction_type` value (used to populate
//! `MessageReactionEvent.previous_reaction_type`) is captured by issuing a
//! preceding SELECT in the same transaction — Sea-ORM's `OnConflict` does
//! not expose the pre-update row, and a single-roundtrip CTE would force
//! us to drop down to raw SQL. The two-statement approach keeps the repo
//! object-safe (mockable in unit tests) at the cost of one extra read; it
//! is well under the < 50ms p95 budget stated in the feature spec.
//!
//! The trait is object-safe so `Arc<dyn ReactionRepo>` flows into
//! [`crate::domain::service::reaction_service::ReactionService`]; the
//! Sea-ORM impl [`SeaReactionRepo`] is the only file that touches the
//! database.
//
// @cpt-cf-chat-engine-reaction-repo:p9
// @cpt-cf-chat-engine-adr-message-reactions:p9

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, SecureDeleteExt, SecureEntityExt, SecureInsertExt, TxConfig,
};
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::domain::ports::{ReactionDeleteOutcome, ReactionRepo, ReactionUpsertOutcome};
use crate::domain::reaction::{MessageReaction, ReactionType};
use crate::infra::db::entity::message_reaction::{
    self as reaction_entity, Column as ReactionColumn, Entity as ReactionEntity,
};
use crate::infra::db::repo::ChatEngineDb;

/// Sea-ORM-backed implementation of [`ReactionRepo`].
///
/// Holds the toolkit-db `DBProvider` so every query runs against the same
/// connection the migration runner used. `message_reactions` has no
/// tenant column (entity is marked `#[secure(unrestricted)]`); the
/// secure wrappers run with `AccessScope::allow_all()` and exist to
/// expose a `&impl DBRunner` execution path.
pub struct SeaReactionRepo {
    db: Arc<ChatEngineDb>,
}

impl SeaReactionRepo {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ReactionRepo for SeaReactionRepo {
    async fn get_by_pk(
        &self,
        message_id: Uuid,
        user_id: &str,
    ) -> Result<Option<MessageReaction>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = ReactionEntity::find_by_id((message_id, user_id.to_owned()))
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await?;
        Ok(row.map(MessageReaction::from))
    }

    async fn upsert(
        &self,
        message_id: Uuid,
        user_id: &str,
        reaction_type: ReactionType,
    ) -> Result<ReactionUpsertOutcome, ChatEngineError> {
        debug_assert!(
            reaction_type.is_persisted(),
            "upsert called with ReactionType::None \u{2014} caller must route to delete"
        );

        let user_id_owned = user_id.to_owned();
        let stored_value = reaction_type.as_str().to_owned();
        let now = OffsetDateTime::now_utc();

        // Single SERIALIZABLE transaction: capture the prior row, then
        // INSERT ... ON CONFLICT UPDATE. The OnConflict refreshes
        // `reaction_type` + `updated_at` only; `created_at` survives so
        // we keep the first-reaction timestamp.
        let (prev_row, after_row) = self
            .db
            .transaction_with_config::<(Option<reaction_entity::Model>, reaction_entity::Model), _>(
                TxConfig::serializable(),
                move |tx| {
                    Box::pin(async move {
                        let scope = AccessScope::allow_all();

                        let previous =
                            ReactionEntity::find_by_id((message_id, user_id_owned.clone()))
                                .secure()
                                .scope_with(&scope)
                                .one(tx)
                                .await?;

                        let am = reaction_entity::ActiveModel {
                            message_id: Set(message_id),
                            user_id: Set(user_id_owned.clone()),
                            reaction_type: Set(stored_value),
                            // For new rows `created_at` lands; for the
                            // updated path Postgres ignores it because the
                            // ON CONFLICT clause below only refreshes
                            // (reaction_type, updated_at).
                            created_at: Set(now),
                            updated_at: Set(now),
                        };

                        let on_conflict = OnConflict::columns([
                            ReactionColumn::MessageId,
                            ReactionColumn::UserId,
                        ])
                        .update_columns([ReactionColumn::ReactionType, ReactionColumn::UpdatedAt])
                        .to_owned();

                        ReactionEntity::insert(am)
                            .secure()
                            .scope_unchecked(&scope)?
                            .on_conflict_raw(on_conflict)
                            .exec(tx)
                            .await?;

                        // Read back the post-write row (cheap on the same
                        // session, primary-key lookup).
                        let after = ReactionEntity::find_by_id((message_id, user_id_owned))
                            .secure()
                            .scope_with(&scope)
                            .one(tx)
                            .await?
                            .ok_or_else(|| {
                                ChatEngineError::internal(
                                    "post-upsert read returned no row (race?)",
                                )
                            })?;

                        Ok((previous, after))
                    })
                },
            )
            .await?;

        let previous_reaction_type = prev_row
            .as_ref()
            .and_then(|m| ReactionType::from_str_value(&m.reaction_type));
        Ok(ReactionUpsertOutcome {
            reaction: MessageReaction::from(after_row),
            previous_reaction_type,
        })
    }

    async fn delete(
        &self,
        message_id: Uuid,
        user_id: &str,
    ) -> Result<ReactionDeleteOutcome, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();

        // Capture the prior value so the service can populate
        // `previous_reaction_type` on the fire-and-forget plugin event.
        let prior = ReactionEntity::find_by_id((message_id, user_id.to_owned()))
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await?;

        let previous_reaction_type = prior
            .as_ref()
            .and_then(|m| ReactionType::from_str_value(&m.reaction_type));

        let res = ReactionEntity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(ReactionColumn::MessageId.eq(message_id))
                    .add(ReactionColumn::UserId.eq(user_id.to_owned())),
            )
            .exec(&conn)
            .await?;

        Ok(ReactionDeleteOutcome {
            applied: res.rows_affected > 0,
            previous_reaction_type,
        })
    }

    async fn list_by_message(
        &self,
        message_id: Uuid,
    ) -> Result<Vec<MessageReaction>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = ReactionEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(ReactionColumn::MessageId.eq(message_id)))
            .all(&conn)
            .await?;
        Ok(rows.into_iter().map(MessageReaction::from).collect())
    }
}

#[cfg(test)]
#[path = "reaction_repo_tests.rs"]
mod reaction_repo_tests;
