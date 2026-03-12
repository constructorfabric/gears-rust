use async_trait::async_trait;
use mini_chat_sdk::models::ChatVectorStore;
use modkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use modkit_security::AccessScope;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::db::entity::chat_vector_store::{self, Entity as VectorStoreEntity};

/// Check if a `SeaORM` `DbErr` is a unique constraint violation.
fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("unique") || msg.contains("duplicate key")
}

/// Stateless `SeaORM`-backed vector store repository.
pub struct VectorStoreRepository;

fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::Forbidden {
            message: msg.to_owned(),
        },
        ScopeError::Invalid(msg) => DomainError::Internal {
            message: format!("scope invalid: {msg}"),
        },
        ScopeError::Db(e) => DomainError::Internal {
            message: format!("database error: {e}"),
        },
        ScopeError::TenantNotInScope { tenant_id } => DomainError::Forbidden {
            message: format!("tenant {tenant_id} not in scope"),
        },
    }
}

fn entity_to_model(m: chat_vector_store::Model) -> ChatVectorStore {
    ChatVectorStore {
        id: m.id,
        tenant_id: m.tenant_id,
        chat_id: m.chat_id,
        vector_store_id: m.vector_store_id,
        provider: m.provider,
        file_count: m.file_count,
        created_at: m.created_at,
    }
}

#[async_trait]
impl crate::domain::repos::VectorStoreRepository for VectorStoreRepository {
    async fn insert_if_absent<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        chat_id: Uuid,
        provider: &str,
    ) -> Result<ChatVectorStore, DomainError> {
        let now = OffsetDateTime::now_utc();
        let id = Uuid::now_v7();

        let active_model = chat_vector_store::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(tenant_id),
            chat_id: ActiveValue::Set(chat_id),
            vector_store_id: ActiveValue::Set(None),
            provider: ActiveValue::Set(provider.to_owned()),
            file_count: ActiveValue::Set(0),
            created_at: ActiveValue::Set(now),
        };

        VectorStoreEntity::insert(active_model.clone())
            .secure()
            .scope_with_model(scope, &active_model)
            .map_err(map_scope_error)?
            .exec(conn)
            .await
            .map_err(|e| match e {
                ScopeError::Db(ref runtime_err) => {
                    if is_unique_violation(runtime_err) {
                        DomainError::AlreadyExists {
                            message: "chat_vector_store".into(),
                        }
                    } else {
                        map_scope_error(e)
                    }
                }
                other => map_scope_error(other),
            })?;

        Ok(ChatVectorStore {
            id,
            tenant_id,
            chat_id,
            vector_store_id: None,
            provider: provider.to_owned(),
            file_count: 0,
            created_at: now,
        })
    }

    async fn set_vector_store_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        vector_store_id: &str,
    ) -> Result<bool, DomainError> {
        let result = VectorStoreEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(chat_vector_store::Column::Id.eq(id))
                    .add(chat_vector_store::Column::VectorStoreId.is_null()),
            )
            .col_expr(
                chat_vector_store::Column::VectorStoreId,
                sea_orm::sea_query::Expr::value(vector_store_id),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(result.rows_affected == 1)
    }

    async fn find_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        chat_id: Uuid,
    ) -> Result<Option<ChatVectorStore>, DomainError> {
        let result = VectorStoreEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(chat_vector_store::Column::ChatId.eq(chat_id))
                    .add(chat_vector_store::Column::TenantId.eq(tenant_id)),
            )
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(result.map(entity_to_model))
    }

    async fn increment_file_count<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError> {
        let result = VectorStoreEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(chat_vector_store::Column::Id.eq(id)))
            .col_expr(
                chat_vector_store::Column::FileCount,
                sea_orm::sea_query::Expr::col(chat_vector_store::Column::FileCount).add(1),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        if result.rows_affected == 0 {
            return Err(DomainError::not_found("vector_store", id));
        }

        Ok(())
    }

    async fn delete_if_null<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError> {
        let result = VectorStoreEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(chat_vector_store::Column::Id.eq(id))
                    .add(chat_vector_store::Column::VectorStoreId.is_null()),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(result.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use modkit_db::migration_runner::run_migrations_for_testing;
    use modkit_db::secure::SecureInsertExt;
    use modkit_db::{ConnectOpts, DBProvider, Db, connect_db};
    use modkit_security::AccessScope;
    use sea_orm::ActiveValue::Set;
    use sea_orm::EntityTrait;
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::VectorStoreRepository;
    use crate::domain::error::DomainError;
    use crate::domain::repos::VectorStoreRepository as VectorStoreRepositoryTrait;
    use crate::infra::db::migrations::Migrator;

    type DbProvider = DBProvider<modkit_db::DbError>;

    mod test_chat_entity {
        use modkit_db_macros::Scopable;
        #[allow(clippy::wildcard_imports)]
        use sea_orm::entity::prelude::*;
        use time::OffsetDateTime;
        use uuid::Uuid;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
        #[sea_orm(table_name = "chats")]
        #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
        #[allow(clippy::struct_field_names)]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub tenant_id: Uuid,
            pub user_id: Uuid,
            #[sea_orm(column_type = "String(StringLen::N(64))")]
            pub model: String,
            pub title: Option<String>,
            pub is_temporary: bool,
            pub created_at: OffsetDateTime,
            pub updated_at: OffsetDateTime,
            pub deleted_at: Option<OffsetDateTime>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}
        impl ActiveModelBehavior for ActiveModel {}
    }

    async fn inmem_db() -> Db {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts).await.expect("connect");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        db
    }

    fn scope_for(tenant_id: Uuid) -> AccessScope {
        AccessScope::for_tenant(tenant_id)
    }

    async fn insert_chat(db: &DbProvider, tenant_id: Uuid, chat_id: Uuid) {
        let now = OffsetDateTime::now_utc();
        let scope = scope_for(tenant_id);
        let model = test_chat_entity::ActiveModel {
            id: Set(chat_id),
            tenant_id: Set(tenant_id),
            user_id: Set(Uuid::new_v4()),
            model: Set("gpt-4o".to_owned()),
            title: Set(None),
            is_temporary: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
        };
        test_chat_entity::Entity::insert(model.clone())
            .secure()
            .scope_with_model(&scope, &model)
            .unwrap()
            .exec(&db.conn().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn insert_if_absent_creates_new_row() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let store = repo
            .insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await
            .unwrap();
        assert_eq!(store.tenant_id, tenant_id);
        assert_eq!(store.chat_id, chat_id);
        assert!(store.vector_store_id.is_none());
        assert_eq!(store.file_count, 0);
        assert_eq!(store.provider, "openai");
    }

    #[tokio::test]
    async fn insert_if_absent_duplicate_returns_already_exists() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        repo.insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await
            .unwrap();
        let result = repo
            .insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await;
        assert!(
            matches!(result, Err(DomainError::AlreadyExists { .. })),
            "duplicate insert should fail: {result:?}"
        );
    }

    #[tokio::test]
    async fn set_vector_store_id_succeeds_when_null() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let store = repo
            .insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await
            .unwrap();
        let updated = repo
            .set_vector_store_id(&db.conn().unwrap(), &scope, store.id, "vs-abc-123")
            .await
            .unwrap();
        assert!(updated);
        let found = repo
            .find_by_chat(&db.conn().unwrap(), &scope, tenant_id, chat_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.vector_store_id.as_deref(), Some("vs-abc-123"));
    }

    #[tokio::test]
    async fn set_vector_store_id_fails_when_already_set() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let store = repo
            .insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await
            .unwrap();
        repo.set_vector_store_id(&db.conn().unwrap(), &scope, store.id, "vs-first")
            .await
            .unwrap();
        let updated = repo
            .set_vector_store_id(&db.conn().unwrap(), &scope, store.id, "vs-second")
            .await
            .unwrap();
        assert!(!updated);
        let found = repo
            .find_by_chat(&db.conn().unwrap(), &scope, tenant_id, chat_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.vector_store_id.as_deref(), Some("vs-first"));
    }

    #[tokio::test]
    async fn find_by_chat_returns_none_for_unknown() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        let found = repo
            .find_by_chat(&db.conn().unwrap(), &scope, tenant_id, chat_id)
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn increment_file_count_is_atomic() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let store = repo
            .insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await
            .unwrap();
        assert_eq!(store.file_count, 0);
        repo.increment_file_count(&db.conn().unwrap(), &scope, store.id)
            .await
            .unwrap();
        repo.increment_file_count(&db.conn().unwrap(), &scope, store.id)
            .await
            .unwrap();
        let found = repo
            .find_by_chat(&db.conn().unwrap(), &scope, tenant_id, chat_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.file_count, 2);
    }

    #[tokio::test]
    async fn delete_if_null_deletes_when_vector_store_id_is_null() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let store = repo
            .insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await
            .unwrap();
        let deleted = repo
            .delete_if_null(&db.conn().unwrap(), &scope, store.id)
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        let found = repo
            .find_by_chat(&db.conn().unwrap(), &scope, tenant_id, chat_id)
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn delete_if_null_preserves_row_when_vector_store_id_is_set() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = VectorStoreRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let store = repo
            .insert_if_absent(&db.conn().unwrap(), &scope, tenant_id, chat_id, "openai")
            .await
            .unwrap();
        repo.set_vector_store_id(&db.conn().unwrap(), &scope, store.id, "vs-set")
            .await
            .unwrap();
        let deleted = repo
            .delete_if_null(&db.conn().unwrap(), &scope, store.id)
            .await
            .unwrap();
        assert_eq!(deleted, 0);
        let found = repo
            .find_by_chat(&db.conn().unwrap(), &scope, tenant_id, chat_id)
            .await
            .unwrap();
        assert!(found.is_some());
    }
}
