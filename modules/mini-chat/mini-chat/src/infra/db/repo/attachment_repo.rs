use async_trait::async_trait;
use mini_chat_sdk::models::{Attachment, AttachmentKind, AttachmentStatus, ThumbnailData};

use crate::domain::repos::attachment_repo::AttachmentWithProvider;
use modkit_db::secure::{DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
use modkit_security::AccessScope;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::attachment_repo::NewAttachmentEntity;
use crate::infra::db::entity::attachment::{self, Entity as AttachmentEntity};

/// Stateless `SeaORM`-backed attachment repository.
pub struct AttachmentRepository;

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

fn entity_to_attachment(m: attachment::Model) -> Attachment {
    Attachment {
        id: m.id,
        chat_id: m.chat_id,
        filename: m.filename,
        content_type: m.content_type,
        size_bytes: m.size_bytes,
        storage_backend: m.storage_backend,
        status: AttachmentStatus::parse(&m.status).unwrap_or(AttachmentStatus::Pending),
        kind: AttachmentKind::parse(&m.attachment_kind).unwrap_or(AttachmentKind::Document),
        doc_summary: m.doc_summary,
        img_thumbnail: m.img_thumbnail.map(|data| ThumbnailData {
            data,
            width: m.img_thumbnail_width.unwrap_or(0),
            height: m.img_thumbnail_height.unwrap_or(0),
        }),
        error_code: m.error_code,
        summary_updated_at: m.summary_updated_at,
        created_at: m.created_at,
        deleted_at: m.deleted_at,
    }
}

#[allow(dead_code)]
fn entity_to_attachment_with_provider(m: attachment::Model) -> AttachmentWithProvider {
    let provider_file_id = m.provider_file_id.clone().unwrap_or_default();
    AttachmentWithProvider {
        attachment: entity_to_attachment(m),
        provider_file_id,
    }
}

#[async_trait]
impl crate::domain::repos::AttachmentRepository for AttachmentRepository {
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        entity: NewAttachmentEntity,
    ) -> Result<Attachment, DomainError> {
        let now = OffsetDateTime::now_utc();

        let active_model = attachment::ActiveModel {
            id: ActiveValue::Set(entity.id),
            tenant_id: ActiveValue::Set(entity.tenant_id),
            chat_id: ActiveValue::Set(entity.chat_id),
            uploaded_by_user_id: ActiveValue::Set(entity.uploaded_by_user_id),
            filename: ActiveValue::Set(entity.filename.clone()),
            content_type: ActiveValue::Set(entity.content_type.clone()),
            size_bytes: ActiveValue::Set(entity.size_bytes),
            storage_backend: ActiveValue::Set(entity.storage_backend.clone()),
            provider_file_id: ActiveValue::Set(None),
            status: ActiveValue::Set(AttachmentStatus::Pending.as_str().to_owned()),
            attachment_kind: ActiveValue::Set(entity.attachment_kind.clone()),
            error_code: ActiveValue::Set(None),
            doc_summary: ActiveValue::Set(None),
            img_thumbnail: ActiveValue::Set(None),
            img_thumbnail_width: ActiveValue::Set(None),
            img_thumbnail_height: ActiveValue::Set(None),
            summary_model: ActiveValue::Set(None),
            summary_updated_at: ActiveValue::Set(None),
            upload_blob: ActiveValue::Set(entity.upload_blob),
            cleanup_status: ActiveValue::Set(None),
            cleanup_attempts: ActiveValue::Set(0),
            last_cleanup_error: ActiveValue::Set(None),
            cleanup_updated_at: ActiveValue::Set(None),
            created_at: ActiveValue::Set(now),
            deleted_at: ActiveValue::Set(None),
        };

        AttachmentEntity::insert(active_model.clone())
            .secure()
            .scope_with_model(scope, &active_model)
            .map_err(map_scope_error)?
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        let kind =
            AttachmentKind::parse(&entity.attachment_kind).unwrap_or(AttachmentKind::Document);
        Ok(Attachment {
            id: entity.id,
            chat_id: entity.chat_id,
            filename: entity.filename,
            content_type: entity.content_type,
            size_bytes: entity.size_bytes,
            storage_backend: entity.storage_backend,
            status: AttachmentStatus::Pending,
            kind,
            doc_summary: None,
            img_thumbnail: None,
            error_code: None,
            summary_updated_at: None,
            created_at: now,
            deleted_at: None,
        })
    }

    async fn find_by_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Attachment>, DomainError> {
        let result = AttachmentEntity::find_by_id(id)
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(attachment::Column::DeletedAt.is_null()))
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(result.map(entity_to_attachment))
    }

    async fn update_status<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        status: AttachmentStatus,
        provider_file_id: Option<String>,
        error_code: Option<String>,
    ) -> Result<(), DomainError> {
        let mut update = AttachmentEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(attachment::Column::Id.eq(id)))
            .col_expr(
                attachment::Column::Status,
                sea_orm::sea_query::Expr::value(status.as_str()),
            );

        if let Some(ref file_id) = provider_file_id {
            update = update.col_expr(
                attachment::Column::ProviderFileId,
                sea_orm::sea_query::Expr::value(file_id.clone()),
            );
        }

        if let Some(ref code) = error_code {
            update = update.col_expr(
                attachment::Column::ErrorCode,
                sea_orm::sea_query::Expr::value(code.clone()),
            );
        }

        update.exec(conn).await.map_err(map_scope_error)?;

        Ok(())
    }

    async fn update_thumbnail<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        thumbnail: Vec<u8>,
        width: i32,
        height: i32,
    ) -> Result<(), DomainError> {
        AttachmentEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(attachment::Column::Id.eq(id)))
            .col_expr(
                attachment::Column::ImgThumbnail,
                sea_orm::sea_query::Expr::value(thumbnail),
            )
            .col_expr(
                attachment::Column::ImgThumbnailWidth,
                sea_orm::sea_query::Expr::value(width),
            )
            .col_expr(
                attachment::Column::ImgThumbnailHeight,
                sea_orm::sea_query::Expr::value(height),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(())
    }

    async fn update_doc_summary<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        summary: String,
        model: String,
    ) -> Result<(), DomainError> {
        AttachmentEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(attachment::Column::Id.eq(id)))
            .col_expr(
                attachment::Column::DocSummary,
                sea_orm::sea_query::Expr::value(summary),
            )
            .col_expr(
                attachment::Column::SummaryModel,
                sea_orm::sea_query::Expr::value(model),
            )
            .col_expr(
                attachment::Column::SummaryUpdatedAt,
                sea_orm::sea_query::Expr::value(OffsetDateTime::now_utc()),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(())
    }

    async fn mark_for_cleanup<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        let result = AttachmentEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(attachment::Column::ChatId.eq(chat_id))
                    .add(attachment::Column::DeletedAt.is_null()),
            )
            .col_expr(
                attachment::Column::CleanupStatus,
                sea_orm::sea_query::Expr::value("pending"),
            )
            .col_expr(
                attachment::Column::CleanupAttempts,
                sea_orm::sea_query::Expr::value(0),
            )
            .col_expr(
                attachment::Column::CleanupUpdatedAt,
                sea_orm::sea_query::Expr::value(OffsetDateTime::now_utc()),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(result.rows_affected)
    }

    async fn find_ready_by_ids<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<AttachmentWithProvider>, DomainError> {
        let results = AttachmentEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(attachment::Column::ChatId.eq(chat_id))
                    .add(attachment::Column::Id.is_in(ids.to_vec()))
                    .add(attachment::Column::Status.eq(AttachmentStatus::Ready.as_str()))
                    .add(attachment::Column::DeletedAt.is_null()),
            )
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(results
            .into_iter()
            .map(entity_to_attachment_with_provider)
            .collect())
    }

    async fn find_ready_documents_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Vec<AttachmentWithProvider>, DomainError> {
        let results = AttachmentEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(attachment::Column::ChatId.eq(chat_id))
                    .add(attachment::Column::Status.eq(AttachmentStatus::Ready.as_str()))
                    .add(attachment::Column::AttachmentKind.eq("document"))
                    .add(attachment::Column::DeletedAt.is_null()),
            )
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(results
            .into_iter()
            .map(entity_to_attachment_with_provider)
            .collect())
    }

    async fn count_uploads_by_user_today<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
    ) -> Result<u64, DomainError> {
        let cutoff = OffsetDateTime::now_utc() - time::Duration::hours(24);
        let count = AttachmentEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(attachment::Column::UploadedByUserId.eq(user_id))
                    .add(attachment::Column::CreatedAt.gte(cutoff))
                    .add(attachment::Column::DeletedAt.is_null()),
            )
            .count(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(count)
    }

    async fn count_documents_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        let count = AttachmentEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(attachment::Column::ChatId.eq(chat_id))
                    .add(attachment::Column::AttachmentKind.eq(AttachmentKind::Document.as_str()))
                    .add(attachment::Column::DeletedAt.is_null()),
            )
            .count(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(count)
    }

    async fn total_document_bytes_by_chat<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        use sea_orm::{FromQueryResult, QuerySelect};

        #[derive(Debug, FromQueryResult)]
        struct TotalBytes {
            total: Option<i64>,
        }

        let rows = AttachmentEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(attachment::Column::ChatId.eq(chat_id))
                    .add(attachment::Column::AttachmentKind.eq(AttachmentKind::Document.as_str()))
                    .add(attachment::Column::DeletedAt.is_null()),
            )
            .project_all(conn, |q: sea_orm::Select<attachment::Entity>| {
                q.select_only()
                    .column_as(attachment::Column::SizeBytes.sum(), "total")
                    .into_model::<TotalBytes>()
            })
            .await
            .map_err(|e| DomainError::Internal {
                message: format!("database error: {e}"),
            })?;

        #[allow(clippy::cast_sign_loss)]
        let total = rows.first().and_then(|r| r.total).unwrap_or(0).max(0) as u64;
        Ok(total)
    }

    async fn load_upload_blob<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        attachment_id: Uuid,
    ) -> Result<Option<Vec<u8>>, DomainError> {
        let row = AttachmentEntity::find_by_id(attachment_id)
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .ok_or(DomainError::AttachmentNotFound { id: attachment_id })?;
        Ok(row.upload_blob)
    }

    async fn clear_upload_blob<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        attachment_id: Uuid,
    ) -> Result<(), DomainError> {
        AttachmentEntity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                attachment::Column::UploadBlob,
                sea_orm::sea_query::Expr::value(sea_orm::Value::Bytes(None)),
            )
            .filter(Condition::all().add(attachment::Column::Id.eq(attachment_id)))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mini_chat_sdk::models::{AttachmentKind, AttachmentStatus};
    use modkit_db::migration_runner::run_migrations_for_testing;
    use modkit_db::secure::SecureInsertExt;
    use modkit_db::{ConnectOpts, DBProvider, Db, connect_db};
    use modkit_security::AccessScope;
    use sea_orm::ActiveValue::Set;
    use sea_orm::EntityTrait;
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::AttachmentRepository;
    use crate::domain::repos::AttachmentRepository as AttachmentRepositoryTrait;
    use crate::domain::repos::attachment_repo::NewAttachmentEntity;
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

    fn new_entity(tenant_id: Uuid, chat_id: Uuid, kind: &str, size: i64) -> NewAttachmentEntity {
        NewAttachmentEntity {
            id: Uuid::now_v7(),
            tenant_id,
            chat_id,
            uploaded_by_user_id: Uuid::new_v4(),
            filename: format!("file-{kind}.pdf"),
            content_type: if kind == "image" {
                "image/png".to_owned()
            } else {
                "application/pdf".to_owned()
            },
            size_bytes: size,
            storage_backend: "azure".to_owned(),
            attachment_kind: kind.to_owned(),
            upload_blob: None,
        }
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

    async fn soft_delete_attachment(db: &DbProvider, scope: &AccessScope, id: Uuid) {
        use crate::infra::db::entity::attachment;
        use modkit_db::secure::SecureUpdateExt;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let now = OffsetDateTime::now_utc();
        attachment::Entity::update_many()
            .col_expr(
                attachment::Column::DeletedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(attachment::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .exec(&db.conn().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn insert_and_find_by_id() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let entity = new_entity(tenant_id, chat_id, "document", 1024);
        let id = entity.id;
        let inserted = repo
            .insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        assert_eq!(inserted.id, id);
        assert_eq!(inserted.status, AttachmentStatus::Pending);
        assert_eq!(inserted.kind, AttachmentKind::Document);
        assert_eq!(inserted.size_bytes, 1024);
        let found = repo
            .find_by_id(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, id);
        assert_eq!(found.filename, inserted.filename);
    }

    #[tokio::test]
    async fn find_by_id_ignores_soft_deleted() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let entity = new_entity(tenant_id, chat_id, "document", 100);
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        soft_delete_attachment(&db, &scope, id).await;
        let found = repo
            .find_by_id(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap();
        assert!(
            found.is_none(),
            "soft-deleted attachment should not be found"
        );
    }

    #[tokio::test]
    async fn find_by_id_with_wrong_tenant_returns_none() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_chat(&db, tenant_a, chat_id).await;
        let entity = new_entity(tenant_a, chat_id, "document", 100);
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope_for(tenant_a), entity)
            .await
            .unwrap();
        let found = repo
            .find_by_id(&db.conn().unwrap(), &scope_for(tenant_b), id)
            .await
            .unwrap();
        assert!(
            found.is_none(),
            "different tenant should not see the attachment"
        );
    }

    #[tokio::test]
    async fn update_status_to_ready_with_provider_file_id() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let entity = new_entity(tenant_id, chat_id, "document", 100);
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        repo.update_status(
            &db.conn().unwrap(),
            &scope,
            id,
            AttachmentStatus::Ready,
            Some("file-123".to_owned()),
            None,
        )
        .await
        .unwrap();
        let found = repo
            .find_by_id(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.status, AttachmentStatus::Ready);
    }

    #[tokio::test]
    async fn update_status_to_failed_with_error_code() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let entity = new_entity(tenant_id, chat_id, "document", 100);
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        repo.update_status(
            &db.conn().unwrap(),
            &scope,
            id,
            AttachmentStatus::Failed,
            None,
            Some("provider_upload_failed".to_owned()),
        )
        .await
        .unwrap();
        let found = repo
            .find_by_id(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.status, AttachmentStatus::Failed);
        assert_eq!(found.error_code.as_deref(), Some("provider_upload_failed"));
    }

    #[tokio::test]
    async fn update_thumbnail_stores_binary_data() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let entity = new_entity(tenant_id, chat_id, "image", 5000);
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        let thumb_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        repo.update_thumbnail(&db.conn().unwrap(), &scope, id, thumb_bytes.clone(), 64, 48)
            .await
            .unwrap();
        let found = repo
            .find_by_id(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap()
            .unwrap();
        let thumb = found.img_thumbnail.unwrap();
        assert_eq!(thumb.data, thumb_bytes);
        assert_eq!(thumb.width, 64);
        assert_eq!(thumb.height, 48);
    }

    #[tokio::test]
    async fn update_doc_summary_stores_text() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let entity = new_entity(tenant_id, chat_id, "document", 100);
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        repo.update_doc_summary(
            &db.conn().unwrap(),
            &scope,
            id,
            "A brief summary.".to_owned(),
            "gpt-4o-mini".to_owned(),
        )
        .await
        .unwrap();
        let found = repo
            .find_by_id(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.doc_summary.as_deref(), Some("A brief summary."));
        assert!(found.summary_updated_at.is_some());
    }

    #[tokio::test]
    async fn mark_for_cleanup_marks_all_non_deleted() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let mut first_id = None;
        for _ in 0..3 {
            let entity = new_entity(tenant_id, chat_id, "document", 100);
            if first_id.is_none() {
                first_id = Some(entity.id);
            }
            repo.insert(&db.conn().unwrap(), &scope, entity)
                .await
                .unwrap();
        }
        soft_delete_attachment(&db, &scope, first_id.unwrap()).await;
        let count = repo
            .mark_for_cleanup(&db.conn().unwrap(), &scope, chat_id)
            .await
            .unwrap();
        assert_eq!(count, 2, "should mark 2 non-deleted attachments");
    }

    #[tokio::test]
    async fn find_ready_by_ids_returns_only_ready() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let mut ids = Vec::new();
        for i in 0..3 {
            let entity = new_entity(tenant_id, chat_id, "document", 100);
            let id = entity.id;
            ids.push(id);
            repo.insert(&db.conn().unwrap(), &scope, entity)
                .await
                .unwrap();
            if i < 2 {
                repo.update_status(
                    &db.conn().unwrap(),
                    &scope,
                    id,
                    AttachmentStatus::Ready,
                    Some(format!("file-{i}")),
                    None,
                )
                .await
                .unwrap();
            }
        }
        let found = repo
            .find_ready_by_ids(&db.conn().unwrap(), &scope, chat_id, &ids)
            .await
            .unwrap();
        assert_eq!(found.len(), 2, "only 2 of 3 are ready");
        for awp in &found {
            assert!(!awp.provider_file_id.is_empty());
        }
    }

    #[tokio::test]
    async fn find_ready_by_ids_ignores_soft_deleted() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let entity = new_entity(tenant_id, chat_id, "document", 100);
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        repo.update_status(
            &db.conn().unwrap(),
            &scope,
            id,
            AttachmentStatus::Ready,
            Some("file-1".to_owned()),
            None,
        )
        .await
        .unwrap();
        soft_delete_attachment(&db, &scope, id).await;
        let found = repo
            .find_ready_by_ids(&db.conn().unwrap(), &scope, chat_id, &[id])
            .await
            .unwrap();
        assert!(
            found.is_empty(),
            "soft-deleted ready attachment should not be returned"
        );
    }

    #[tokio::test]
    async fn count_documents_excludes_images() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for _ in 0..3 {
            repo.insert(
                &db.conn().unwrap(),
                &scope,
                new_entity(tenant_id, chat_id, "document", 100),
            )
            .await
            .unwrap();
        }
        for _ in 0..2 {
            repo.insert(
                &db.conn().unwrap(),
                &scope,
                new_entity(tenant_id, chat_id, "image", 5000),
            )
            .await
            .unwrap();
        }
        let count = repo
            .count_documents_by_chat(&db.conn().unwrap(), &scope, chat_id)
            .await
            .unwrap();
        assert_eq!(count, 3, "should only count documents, not images");
    }

    #[tokio::test]
    async fn count_documents_excludes_deleted() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let mut ids = Vec::new();
        for _ in 0..3 {
            let entity = new_entity(tenant_id, chat_id, "document", 100);
            ids.push(entity.id);
            repo.insert(&db.conn().unwrap(), &scope, entity)
                .await
                .unwrap();
        }
        soft_delete_attachment(&db, &scope, ids[0]).await;
        let count = repo
            .count_documents_by_chat(&db.conn().unwrap(), &scope, chat_id)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn total_document_bytes_sums_only_documents() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for size in [1000, 2000, 3000] {
            repo.insert(
                &db.conn().unwrap(),
                &scope,
                new_entity(tenant_id, chat_id, "document", size),
            )
            .await
            .unwrap();
        }
        repo.insert(
            &db.conn().unwrap(),
            &scope,
            new_entity(tenant_id, chat_id, "image", 5000),
        )
        .await
        .unwrap();
        let total = repo
            .total_document_bytes_by_chat(&db.conn().unwrap(), &scope, chat_id)
            .await
            .unwrap();
        assert_eq!(total, 6000);
    }

    #[tokio::test]
    async fn count_uploads_by_user_today_counts_all_kinds() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        let user_id = Uuid::new_v4();
        insert_chat(&db, tenant_id, chat_id).await;
        // Insert 2 documents + 1 image, all by the same user
        for kind in ["document", "document", "image"] {
            let mut entity = new_entity(tenant_id, chat_id, kind, 100);
            entity.uploaded_by_user_id = user_id;
            repo.insert(&db.conn().unwrap(), &scope, entity)
                .await
                .unwrap();
        }
        // Different user — should not count
        repo.insert(
            &db.conn().unwrap(),
            &scope,
            new_entity(tenant_id, chat_id, "document", 100),
        )
        .await
        .unwrap();
        let count = repo
            .count_uploads_by_user_today(&db.conn().unwrap(), &scope, user_id)
            .await
            .unwrap();
        assert_eq!(count, 3, "should count all 3 uploads by the target user");
    }

    #[tokio::test]
    async fn count_uploads_by_user_today_excludes_deleted() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        let user_id = Uuid::new_v4();
        insert_chat(&db, tenant_id, chat_id).await;
        let mut entity = new_entity(tenant_id, chat_id, "document", 100);
        entity.uploaded_by_user_id = user_id;
        let id = entity.id;
        repo.insert(&db.conn().unwrap(), &scope, entity)
            .await
            .unwrap();
        soft_delete_attachment(&db, &scope, id).await;
        let count = repo
            .count_uploads_by_user_today(&db.conn().unwrap(), &scope, user_id)
            .await
            .unwrap();
        assert_eq!(count, 0, "deleted uploads should not count");
    }

    // ── find_ready_documents_by_chat ────────────────────────────────────

    #[tokio::test]
    async fn find_ready_docs_returns_only_ready_documents() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        // 2 ready docs
        for _ in 0..2 {
            let e = new_entity(tenant_id, chat_id, "document", 100);
            let id = e.id;
            repo.insert(&db.conn().unwrap(), &scope, e).await.unwrap();
            repo.update_status(
                &db.conn().unwrap(),
                &scope,
                id,
                AttachmentStatus::Ready,
                Some("file-x".to_owned()),
                None,
            )
            .await
            .unwrap();
        }
        // 1 pending doc
        repo.insert(
            &db.conn().unwrap(),
            &scope,
            new_entity(tenant_id, chat_id, "document", 100),
        )
        .await
        .unwrap();
        // 1 ready image
        let img = new_entity(tenant_id, chat_id, "image", 100);
        let img_id = img.id;
        repo.insert(&db.conn().unwrap(), &scope, img).await.unwrap();
        repo.update_status(
            &db.conn().unwrap(),
            &scope,
            img_id,
            AttachmentStatus::Ready,
            Some("file-img".to_owned()),
            None,
        )
        .await
        .unwrap();

        let docs = repo
            .find_ready_documents_by_chat(&db.conn().unwrap(), &scope, chat_id)
            .await
            .unwrap();
        assert_eq!(docs.len(), 2, "should return only ready documents");
    }

    #[tokio::test]
    async fn find_ready_docs_empty_chat() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        let docs = repo
            .find_ready_documents_by_chat(&db.conn().unwrap(), &scope, chat_id)
            .await
            .unwrap();
        assert!(docs.is_empty());
    }

    #[tokio::test]
    async fn find_ready_docs_excludes_deleted() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        let e = new_entity(tenant_id, chat_id, "document", 100);
        let id = e.id;
        repo.insert(&db.conn().unwrap(), &scope, e).await.unwrap();
        repo.update_status(
            &db.conn().unwrap(),
            &scope,
            id,
            AttachmentStatus::Ready,
            Some("file-x".to_owned()),
            None,
        )
        .await
        .unwrap();
        soft_delete_attachment(&db, &scope, id).await;

        let docs = repo
            .find_ready_documents_by_chat(&db.conn().unwrap(), &scope, chat_id)
            .await
            .unwrap();
        assert!(docs.is_empty(), "soft-deleted docs must be excluded");
    }

    // ── load_upload_blob / clear_upload_blob ─────────────────────────────

    fn new_entity_with_blob(
        tenant_id: Uuid,
        chat_id: Uuid,
        blob: Option<Vec<u8>>,
    ) -> NewAttachmentEntity {
        NewAttachmentEntity {
            upload_blob: blob,
            ..new_entity(tenant_id, chat_id, "document", 100)
        }
    }

    #[tokio::test]
    async fn load_upload_blob_returns_bytes() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        let e = new_entity_with_blob(tenant_id, chat_id, Some(vec![1, 2, 3, 4]));
        let id = e.id;
        repo.insert(&db.conn().unwrap(), &scope, e).await.unwrap();

        let blob = repo
            .load_upload_blob(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap();
        assert_eq!(blob, Some(vec![1, 2, 3, 4]));
    }

    #[tokio::test]
    async fn load_upload_blob_returns_none_when_null() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        let e = new_entity_with_blob(tenant_id, chat_id, None);
        let id = e.id;
        repo.insert(&db.conn().unwrap(), &scope, e).await.unwrap();

        let blob = repo
            .load_upload_blob(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap();
        assert_eq!(blob, None);
    }

    #[tokio::test]
    async fn load_upload_blob_not_found() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let scope = scope_for(Uuid::new_v4());

        let result = repo
            .load_upload_blob(&db.conn().unwrap(), &scope, Uuid::new_v4())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn clear_upload_blob_sets_null() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        let e = new_entity_with_blob(tenant_id, chat_id, Some(vec![9, 8, 7]));
        let id = e.id;
        repo.insert(&db.conn().unwrap(), &scope, e).await.unwrap();

        repo.clear_upload_blob(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap();

        let blob = repo
            .load_upload_blob(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap();
        assert_eq!(blob, None, "blob should be NULL after clear");
    }

    #[tokio::test]
    async fn clear_upload_blob_idempotent() {
        let db = Arc::new(DbProvider::new(inmem_db().await));
        let repo = AttachmentRepository;
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let scope = scope_for(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        let e = new_entity_with_blob(tenant_id, chat_id, Some(vec![1]));
        let id = e.id;
        repo.insert(&db.conn().unwrap(), &scope, e).await.unwrap();

        // Clear twice — both should succeed
        repo.clear_upload_blob(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap();
        repo.clear_upload_blob(&db.conn().unwrap(), &scope, id)
            .await
            .unwrap();
    }
}
