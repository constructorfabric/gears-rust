use std::collections::HashMap;

use async_trait::async_trait;
use modkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use modkit_security::AccessScope;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::ReactionKind;
use crate::domain::repos::UpsertReactionParams;
use crate::infra::db::entity::message_reaction::{
    ActiveModel, Column, Entity as ReactionEntity, Model as ReactionModel,
};

pub struct ReactionRepository;

#[async_trait]
impl crate::domain::repos::ReactionRepository for ReactionRepository {
    async fn upsert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: UpsertReactionParams,
    ) -> Result<ReactionModel, DomainError> {
        let now = OffsetDateTime::now_utc();

        let am = ActiveModel {
            id: Set(params.id),
            tenant_id: Set(params.tenant_id),
            message_id: Set(params.message_id),
            user_id: Set(params.user_id),
            reaction: Set(params.reaction.as_str().to_owned()),
            created_at: Set(now),
        };

        let on_conflict =
            SecureOnConflict::<ReactionEntity>::columns([Column::MessageId, Column::UserId])
                .update_columns([Column::Reaction, Column::CreatedAt])?;

        ReactionEntity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)?
            .on_conflict(on_conflict)
            .exec(runner)
            .await?;

        // SELECT back by natural key — works on both Postgres and SQLite
        // (`exec_with_returning` fails on the ON CONFLICT path in SQLite
        // because it looks up by the *new* PK which doesn't exist when
        // the row was updated rather than inserted).
        ReactionEntity::find()
            .filter(
                Condition::all()
                    .add(Column::MessageId.eq(params.message_id))
                    .add(Column::UserId.eq(params.user_id)),
            )
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .ok_or_else(|| DomainError::database("reaction row missing after upsert".to_owned()))
    }

    async fn batch_by_user<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        message_ids: &[Uuid],
        user_id: Uuid,
    ) -> Result<HashMap<Uuid, ReactionKind>, DomainError> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = ReactionEntity::find()
            .filter(
                Condition::all()
                    .add(Column::MessageId.is_in(message_ids.iter().copied()))
                    .add(Column::UserId.eq(user_id)),
            )
            .secure()
            .scope_with(scope)
            .all(runner)
            .await?;

        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            if let Some(kind) = ReactionKind::parse(&row.reaction) {
                map.insert(row.message_id, kind);
            }
        }
        Ok(map)
    }

    async fn delete_by_message_and_user<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        message_id: uuid::Uuid,
        user_id: uuid::Uuid,
    ) -> Result<bool, DomainError> {
        let result = ReactionEntity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::MessageId.eq(message_id))
                    .add(Column::UserId.eq(user_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(|e| DomainError::database(e.to_string()))?;

        Ok(result.rows_affected > 0)
    }
}

#[cfg(test)]
#[path = "reaction_repo_test.rs"]
mod tests;
