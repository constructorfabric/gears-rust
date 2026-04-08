use std::collections::HashMap;

use async_trait::async_trait;
use modkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use modkit_security::AccessScope;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, FromQueryResult, QueryFilter, QuerySelect, Set,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::llm::AttachmentRef;
use crate::domain::repos::{
    InsertAttachmentParams, SetFailedParams, SetReadyParams, SetUploadedParams,
};
use crate::infra::db::entity::attachment::{
    ActiveModel, AttachmentKind, AttachmentStatus, Column, Entity, Model as AttachmentModel,
};

fn db_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::database(e.to_string())
}

/// Repository for attachment persistence operations.
pub struct AttachmentRepository;

#[async_trait]
impl crate::domain::repos::AttachmentRepository for AttachmentRepository {
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: InsertAttachmentParams,
    ) -> Result<AttachmentModel, DomainError> {
        let now = OffsetDateTime::now_utc();
        let kind = match params.attachment_kind.as_str() {
            "document" => AttachmentKind::Document,
            "image" => AttachmentKind::Image,
            other => {
                return Err(DomainError::validation(format!(
                    "invalid attachment_kind: {other}"
                )));
            }
        };
        let am = ActiveModel {
            id: Set(params.id),
            tenant_id: Set(params.tenant_id),
            chat_id: Set(params.chat_id),
            uploaded_by_user_id: Set(params.uploaded_by_user_id),
            filename: Set(params.filename),
            content_type: Set(params.content_type),
            size_bytes: Set(params.size_bytes),
            storage_backend: Set(params.storage_backend),
            provider_file_id: Set(None),
            status: Set(AttachmentStatus::Pending),
            error_code: Set(None),
            attachment_kind: Set(kind),
            for_file_search: Set(params.for_file_search),
            for_code_interpreter: Set(params.for_code_interpreter),
            doc_summary: Set(None),
            img_thumbnail: Set(None),
            img_thumbnail_width: Set(None),
            img_thumbnail_height: Set(None),
            summary_model: Set(None),
            summary_updated_at: Set(None),
            cleanup_status: Set(None),
            cleanup_attempts: Set(0),
            last_cleanup_error: Set(None),
            cleanup_updated_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
        };
        Ok(secure_insert::<Entity>(am, scope, runner).await?)
    }

    async fn cas_set_uploaded<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetUploadedParams,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = Entity::update_many()
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Uploaded))
            .col_expr(
                Column::ProviderFileId,
                Expr::value(Some(params.provider_file_id)),
            )
            .col_expr(Column::SizeBytes, Expr::value(params.size_bytes))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(params.id))
                    .add(Column::Status.eq(AttachmentStatus::Pending))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn cas_set_ready<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetReadyParams,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = Entity::update_many()
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Ready))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(params.id))
                    .add(Column::Status.eq(AttachmentStatus::Uploaded))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn cas_set_failed<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetFailedParams,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        // from_status determines expected source: "pending" or "uploaded"
        let from_status = match params.from_status.as_str() {
            "pending" => AttachmentStatus::Pending,
            "uploaded" => AttachmentStatus::Uploaded,
            other => {
                return Err(DomainError::validation(format!(
                    "invalid from_status for set_failed: {other}"
                )));
            }
        };
        let result = Entity::update_many()
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Failed))
            .col_expr(Column::ErrorCode, Expr::value(Some(params.error_code)))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(params.id))
                    .add(Column::Status.eq(from_status))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<AttachmentModel>, DomainError> {
        // Returns all rows including soft-deleted — caller decides 404 policy.
        let found = Entity::find_by_id(id)
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(db_err)?;
        Ok(found)
    }

    async fn get_batch<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<AttachmentModel>, DomainError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = Entity::find()
            .filter(Column::Id.is_in(ids.iter().copied()))
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows)
    }

    async fn soft_delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError> {
        use crate::infra::db::entity::message_attachment::{
            Column as MaColumn, Entity as MaEntity,
        };

        // CAS-guarded soft-delete with message_attachments guard:
        // WHERE deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM message_attachments WHERE attachment_id = $1)
        let not_referenced = Expr::exists(
            Query::select()
                .expr(Expr::val(1i32))
                .from(MaEntity)
                .and_where(MaColumn::AttachmentId.eq(id))
                .to_owned(),
        )
        .not();

        let now = OffsetDateTime::now_utc();
        let result = Entity::update_many()
            .col_expr(Column::DeletedAt, Expr::value(Some(now)))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(id))
                    .add(Column::DeletedAt.is_null())
                    .add(not_referenced),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn count_ready_documents<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError> {
        let count = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::Status.eq(AttachmentStatus::Ready))
                    .add(Column::AttachmentKind.eq(AttachmentKind::Document))
                    .add(Column::ForFileSearch.eq(true))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .count(runner)
            .await
            .map_err(db_err)?;
        #[allow(clippy::cast_possible_wrap)]
        Ok(count as i64)
    }

    async fn count_documents<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError> {
        let count = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::AttachmentKind.eq(AttachmentKind::Document))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .count(runner)
            .await
            .map_err(db_err)?;
        #[allow(clippy::cast_possible_wrap)]
        Ok(count as i64)
    }

    async fn sum_size_bytes<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError> {
        #[derive(Debug, FromQueryResult)]
        struct SumRow {
            total: Option<i64>,
        }

        let rows: Vec<SumRow> = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .project_all(runner, |q| {
                q.select_only()
                    .column_as(
                        Column::SizeBytes
                            .sum()
                            .cast_as(sea_orm::sea_query::Alias::new("bigint")),
                        "total",
                    )
                    .into_model::<SumRow>()
            })
            .await
            .map_err(db_err)?;
        Ok(rows.first().and_then(|r| r.total).unwrap_or(0))
    }

    async fn build_provider_file_id_map<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<HashMap<String, AttachmentRef>, DomainError> {
        #[derive(Debug, FromQueryResult)]
        struct FileIdRow {
            id: Uuid,
            provider_file_id: String,
            filename: String,
        }

        let rows: Vec<FileIdRow> = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::Status.eq(AttachmentStatus::Ready))
                    .add(Column::DeletedAt.is_null())
                    .add(Column::ProviderFileId.is_not_null()),
            )
            .secure()
            .scope_with(scope)
            .project_all(runner, |q| {
                q.select_only()
                    .column(Column::Id)
                    .column(Column::ProviderFileId)
                    .column(Column::Filename)
                    .into_model::<FileIdRow>()
            })
            .await
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.provider_file_id,
                    AttachmentRef {
                        id: r.id,
                        filename: r.filename,
                    },
                )
            })
            .collect())
    }

    async fn get_code_interpreter_file_ids<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Vec<String>, DomainError> {
        #[derive(Debug, FromQueryResult)]
        struct ProviderFileIdRow {
            provider_file_id: Option<String>,
        }

        let rows: Vec<ProviderFileIdRow> = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::ForCodeInterpreter.eq(true))
                    .add(Column::Status.eq(AttachmentStatus::Ready))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .project_all(runner, |q| {
                q.select_only()
                    .column(Column::ProviderFileId)
                    .into_model::<ProviderFileIdRow>()
            })
            .await
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.provider_file_id)
            .collect())
    }
}
