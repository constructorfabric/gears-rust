use std::collections::HashMap;

use crate::domain::models::Chat;
use async_trait::async_trait;
use modkit_db::odata::{LimitCfg, paginate_odata};
use modkit_db::secure::{
    DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert, secure_update_with_scope,
};
use modkit_odata::{ODataQuery, Page, SortDir};
use modkit_security::AccessScope;
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{EntityTrait, FromQueryResult, QueryFilter, QuerySelect, Set};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::db::entity::chat::{ActiveModel, Column, Entity};
use crate::infra::db::entity::message::{Column as MsgColumn, Entity as MsgEntity};
use crate::infra::db::odata_mapper::{ChatCursorField, ChatODataMapper};

fn db_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::database(e.to_string())
}

/// ORM-based implementation of the `ChatRepository` trait.
#[derive(Clone)]
pub struct ChatRepository {
    limit_cfg: LimitCfg,
}

impl ChatRepository {
    #[must_use]
    pub fn new(limit_cfg: LimitCfg) -> Self {
        Self { limit_cfg }
    }
}

#[async_trait]
impl crate::domain::repos::ChatRepository for ChatRepository {
    async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Chat>, DomainError> {
        let found = Entity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(Column::Id).eq(id))
                    .add(Expr::col(Column::DeletedAt).is_null()),
            )
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_err)?;
        Ok(found.map(Into::into))
    }

    async fn list_page<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Chat>, DomainError> {
        let base_query = Entity::find()
            .filter(sea_orm::Condition::all().add(Expr::col(Column::DeletedAt).is_null()))
            .secure()
            .scope_with(scope);

        let page = paginate_odata::<ChatCursorField, ChatODataMapper, _, _, _, _>(
            base_query,
            conn,
            query,
            ("updated_at", SortDir::Desc),
            self.limit_cfg,
            Into::into,
        )
        .await
        .map_err(db_err)?;

        Ok(page)
    }

    async fn create<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat: Chat,
    ) -> Result<Chat, DomainError> {
        let m = ActiveModel {
            id: Set(chat.id),
            tenant_id: Set(chat.tenant_id),
            user_id: Set(chat.user_id),
            model: Set(chat.model.clone()),
            title: Set(chat.title.clone()),
            is_temporary: Set(chat.is_temporary),
            created_at: Set(chat.created_at),
            updated_at: Set(chat.updated_at),
            deleted_at: Set(None),
        };

        let _ = secure_insert::<Entity>(m, scope, conn)
            .await
            .map_err(db_err)?;
        Ok(chat)
    }

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat: Chat,
    ) -> Result<Chat, DomainError> {
        let m = ActiveModel {
            id: Set(chat.id),
            tenant_id: Set(chat.tenant_id),
            user_id: Set(chat.user_id),
            model: Set(chat.model.clone()),
            title: Set(chat.title.clone()),
            is_temporary: Set(chat.is_temporary),
            created_at: Set(chat.created_at),
            updated_at: Set(chat.updated_at),
            deleted_at: sea_orm::ActiveValue::NotSet,
        };

        let _ = secure_update_with_scope::<Entity>(m, scope, chat.id, conn)
            .await
            .map_err(db_err)?;
        Ok(chat)
    }

    async fn soft_delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = Entity::update_many()
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(Column::Id).eq(id))
                    .add(Expr::col(Column::DeletedAt).is_null()),
            )
            .col_expr(Column::DeletedAt, Expr::value(now))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;

        Ok(result.rows_affected > 0)
    }

    async fn get_for_update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Chat>, DomainError> {
        let found = Entity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(Column::Id).eq(id))
                    .add(Expr::col(Column::DeletedAt).is_null()),
            )
            .lock(LockType::Update)
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_err)?;
        Ok(found.map(Into::into))
    }

    async fn count_messages<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError> {
        let count = MsgEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(MsgColumn::ChatId).eq(chat_id))
                    .add(Expr::col(MsgColumn::DeletedAt).is_null()),
            )
            .secure()
            .scope_with(scope)
            .count(conn)
            .await
            .map_err(db_err)?;

        #[allow(clippy::cast_possible_wrap)]
        Ok(count as i64)
    }

    async fn count_messages_batch<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, i64>, DomainError> {
        if chat_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = MsgEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(Expr::col(MsgColumn::ChatId).is_in(chat_ids.iter().copied()))
                    .add(Expr::col(MsgColumn::DeletedAt).is_null()),
            )
            .secure()
            .scope_with(scope)
            .project_all(conn, |q| {
                q.select_only()
                    .column(MsgColumn::ChatId)
                    .column_as(Expr::col(MsgColumn::Id).count(), "cnt")
                    .group_by(MsgColumn::ChatId)
                    .into_model::<ChatMessageCount>()
            })
            .await
            .map_err(db_err)?;

        let mut counts = HashMap::with_capacity(rows.len());
        for row in rows {
            counts.insert(row.chat_id, row.cnt);
        }
        Ok(counts)
    }
}

#[derive(Debug, FromQueryResult)]
struct ChatMessageCount {
    chat_id: Uuid,
    cnt: i64,
}
