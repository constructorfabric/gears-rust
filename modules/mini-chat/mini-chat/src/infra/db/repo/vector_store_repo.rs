use async_trait::async_trait;
use modkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use modkit_security::AccessScope;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::InsertVectorStoreParams;
use crate::infra::db::entity::chat_vector_store::{
    ActiveModel, Column, Entity, Model as VectorStoreModel,
};

fn db_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::database(e.to_string())
}

/// Repository for vector store persistence operations.
pub struct VectorStoreRepository;

#[async_trait]
impl crate::domain::repos::VectorStoreRepository for VectorStoreRepository {
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: InsertVectorStoreParams,
    ) -> Result<VectorStoreModel, DomainError> {
        let now = OffsetDateTime::now_utc();
        let am = ActiveModel {
            id: Set(params.id),
            tenant_id: Set(params.tenant_id),
            chat_id: Set(params.chat_id),
            vector_store_id: Set(None),
            provider: Set(params.provider),
            file_count: Set(0),
            created_at: Set(now),
        };
        // Unique violation on (tenant_id, chat_id) automatically maps to
        // DomainError::Conflict via ScopeError::Db → map_db_err.
        Ok(secure_insert::<Entity>(am, scope, runner).await?)
    }

    async fn cas_set_vector_store_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        vector_store_id: &str,
    ) -> Result<u64, DomainError> {
        let result = Entity::update_many()
            .col_expr(
                Column::VectorStoreId,
                sea_orm::sea_query::Expr::value(Some(vector_store_id.to_owned())),
            )
            .filter(
                Condition::all()
                    .add(Column::Id.eq(id))
                    .add(Column::VectorStoreId.is_null()),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn find_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<VectorStoreModel>, DomainError> {
        let found = Entity::find()
            .filter(Column::ChatId.eq(chat_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(found)
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError> {
        let result = Entity::delete_many()
            .filter(Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }
}
