use std::collections::HashMap;

use async_trait::async_trait;
use sea_orm::sea_query::ExprTrait;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, FromQueryResult, QueryFilter, QuerySelect, Set,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::llm::AttachmentRef;
use crate::domain::repos::{
    InsertAttachmentParams, SetFailedParams, SetReadyParams, SetSecondaryUploadParams,
    SetUploadedParams,
};
use crate::infra::db::entity::attachment::{
    ActiveModel, AttachmentKind, AttachmentStatus, CleanupStatus, Column, Entity,
    Model as AttachmentModel, SecondaryUploadStatus,
};

fn db_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::database(e.to_string())
}

/// Repository for attachment persistence operations.
pub struct AttachmentRepository;

// ── Upload reaper (system scope, background worker) ─────────────────────

impl AttachmentRepository {
    /// `pending` / `uploaded` rows not updated since `cutoff`, oldest first.
    /// Rows with a `cleanup_status` are skipped: chat deletion already owns
    /// their provider cleanup.
    pub async fn find_stale_uploads<C: DBRunner>(
        &self,
        runner: &C,
        cutoff: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<AttachmentModel>, DomainError> {
        Entity::find()
            .filter(
                Condition::all()
                    .add(
                        Column::Status
                            .is_in([AttachmentStatus::Pending, AttachmentStatus::Uploaded]),
                    )
                    .add(Column::DeletedAt.is_null())
                    .add(Column::CleanupStatus.is_null())
                    .add(Column::UpdatedAt.lt(cutoff)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .order_by(Column::UpdatedAt, sea_orm::Order::Asc)
            .limit(limit)
            .all(runner)
            .await
            .map_err(db_err)
    }

    /// CAS `from` → `failed` with `error_code = upload_abandoned`, only while
    /// the row is still stale. With `schedule_cleanup` the row also gets
    /// `cleanup_status = pending`, so the attachment cleanup handler can mark
    /// it done after deleting the provider file. Scoped to the row's tenant.
    /// Returns rows affected.
    pub async fn cas_abandon_upload<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        id: Uuid,
        from: AttachmentStatus,
        cutoff: OffsetDateTime,
        schedule_cleanup: bool,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let mut update = Entity::update_many()
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Failed))
            .col_expr(
                Column::ErrorCode,
                Expr::value(Some(UPLOAD_ABANDONED.to_owned())),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now));
        if schedule_cleanup {
            update = update
                .col_expr(
                    Column::CleanupStatus,
                    Expr::value(Some(CleanupStatus::Pending)),
                )
                .col_expr(Column::CleanupUpdatedAt, Expr::value(Some(now)));
        }
        let result = update
            .filter(
                Condition::all()
                    .add(Column::Id.eq(id))
                    .add(Column::Status.eq(from))
                    .add(Column::DeletedAt.is_null())
                    .add(Column::CleanupStatus.is_null())
                    .add(Column::UpdatedAt.lt(cutoff)),
            )
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant_id))
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }
}

/// `error_code` of an attachment reaped by the upload reaper.
pub const UPLOAD_ABANDONED: &str = "upload_abandoned";

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
            secondary_file_id: Set(None),
            secondary_status: Set(SecondaryUploadStatus::NotAttempted),
            secondary_provider_kind: Set(None),
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

    async fn touch_uploaded<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError> {
        let result = Entity::update_many()
            .col_expr(Column::UpdatedAt, Expr::value(OffsetDateTime::now_utc()))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(id))
                    .add(Column::Status.eq(AttachmentStatus::Uploaded))
                    .add(Column::DeletedAt.is_null())
                    // Chat deletion marks its attachments for cleanup
                    // without setting `deleted_at`; stop on those too.
                    .add(Column::CleanupStatus.is_null()),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn cas_fail_uploaded_for_cleanup<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        error_code: &str,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = Entity::update_many()
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Failed))
            .col_expr(Column::ErrorCode, Expr::value(Some(error_code.to_owned())))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .col_expr(
                Column::CleanupStatus,
                Expr::value(Some(CleanupStatus::Pending)),
            )
            .col_expr(Column::CleanupUpdatedAt, Expr::value(Some(now)))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(id))
                    .add(Column::Status.eq(AttachmentStatus::Uploaded))
                    .add(Column::DeletedAt.is_null())
                    .add(Column::CleanupStatus.is_null()),
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
        let mut query = Entity::update_many()
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Ready))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .col_expr(Column::ImgThumbnail, Expr::value(params.img_thumbnail))
            .col_expr(
                Column::ImgThumbnailWidth,
                Expr::value(params.img_thumbnail_width),
            )
            .col_expr(
                Column::ImgThumbnailHeight,
                Expr::value(params.img_thumbnail_height),
            );
        query = query.filter(
            Condition::all()
                .add(Column::Id.eq(params.id))
                .add(Column::Status.eq(AttachmentStatus::Uploaded))
                .add(Column::DeletedAt.is_null())
                // A row of a deleted chat (cleanup pending) never becomes
                // `ready`; the caller treats 0 rows as a concurrent delete.
                .add(Column::CleanupStatus.is_null()),
        );
        let result = query
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

    async fn set_secondary_upload<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetSecondaryUploadParams,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        // `NotAttempted` is the initial state set at INSERT time only — reject
        // it here so the forward-transition contract is enforced.
        if matches!(params.secondary_status, SecondaryUploadStatus::NotAttempted) {
            return Err(DomainError::validation(
                "set_secondary_upload cannot set status to not_attempted",
            ));
        }
        let result = Entity::update_many()
            .col_expr(
                Column::SecondaryStatus,
                Expr::value(params.secondary_status),
            )
            .col_expr(
                Column::SecondaryFileId,
                Expr::value(params.secondary_file_id),
            )
            .col_expr(
                Column::SecondaryProviderKind,
                Expr::value(params.secondary_provider_kind),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(params.id))
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
        // The attachment enters the provider-cleanup state machine here, as
        // in `mark_attachments_pending_for_chat`: the cleanup handler only
        // advances rows that are `pending`.
        let result = Entity::update_many()
            .col_expr(Column::DeletedAt, Expr::value(Some(now)))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .col_expr(
                Column::CleanupStatus,
                Expr::value(Some(CleanupStatus::Pending)),
            )
            .col_expr(Column::CleanupUpdatedAt, Expr::value(Some(now)))
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
                    .add(Column::DeletedAt.is_null())
                    // A failed upload holds no provider document; like
                    // `sum_size_bytes`, it does not count against the limit.
                    .add(Column::Status.ne(AttachmentStatus::Failed)),
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
                    .add(Column::DeletedAt.is_null())
                    .add(Column::Status.ne(AttachmentStatus::Failed)),
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

    async fn build_secondary_file_id_map<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        provider_kind: &str,
    ) -> Result<HashMap<String, String>, DomainError> {
        #[derive(Debug, FromQueryResult)]
        struct SecondaryIdRow {
            provider_file_id: String,
            secondary_file_id: String,
        }

        let rows: Vec<SecondaryIdRow> = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::DeletedAt.is_null())
                    .add(Column::ProviderFileId.is_not_null())
                    .add(Column::SecondaryFileId.is_not_null())
                    .add(Column::SecondaryStatus.eq(SecondaryUploadStatus::Uploaded))
                    .add(Column::SecondaryProviderKind.eq(provider_kind)),
            )
            .secure()
            .scope_with(scope)
            .project_all(runner, |q| {
                q.select_only()
                    .column(Column::ProviderFileId)
                    .column(Column::SecondaryFileId)
                    .into_model::<SecondaryIdRow>()
            })
            .await
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.provider_file_id, r.secondary_file_id))
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

    // ── Cleanup methods (no user session — background workers) ─────────
    //
    // These use `AccessScope::allow_all()` because background workers have
    // no user context. Safety: the outbox message was enqueued inside a
    // scoped transaction, so the chat_id is already authorized.

    async fn find_pending_cleanup_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
    ) -> Result<Vec<AttachmentModel>, DomainError> {
        let scope = AccessScope::allow_all();
        // No deleted_at filter: in the chat-deletion path, individual attachments
        // are NOT soft-deleted — only their cleanup_status is set to 'pending'.
        let rows = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::CleanupStatus.eq(CleanupStatus::Pending)),
            )
            .secure()
            .scope_with(&scope)
            .all(runner)
            .await
            .map_err(db_err)?;
        Ok(rows)
    }

    async fn mark_cleanup_done<C: DBRunner>(
        &self,
        runner: &C,
        attachment_id: Uuid,
    ) -> Result<u64, DomainError> {
        let scope = AccessScope::allow_all();
        let now = OffsetDateTime::now_utc();
        let result = Entity::update_many()
            .col_expr(
                Column::CleanupStatus,
                Expr::value(Some(CleanupStatus::Done)),
            )
            .col_expr(Column::CleanupUpdatedAt, Expr::value(Some(now)))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(attachment_id))
                    .add(Column::CleanupStatus.eq(CleanupStatus::Pending)),
            )
            .secure()
            .scope_with(&scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn record_cleanup_attempt<C: DBRunner>(
        &self,
        runner: &C,
        attachment_id: Uuid,
        error: &str,
        max_attempts: u32,
    ) -> Result<crate::domain::repos::CleanupOutcome, DomainError> {
        use crate::domain::repos::CleanupOutcome;

        let scope = AccessScope::allow_all();
        let now = OffsetDateTime::now_utc();

        // Atomic increment + conditional status transition in a single UPDATE.
        // CASE WHEN cleanup_attempts + 1 >= max THEN 'failed' ELSE 'pending' END
        // CAS guard: cleanup_status = 'pending' (skip if already terminal).
        #[allow(clippy::cast_possible_wrap)]
        let max_i32 = max_attempts as i32;

        let new_status_expr: sea_orm::sea_query::SimpleExpr = Expr::case(
            Expr::col(Column::CleanupAttempts)
                .add(Expr::val(1i32))
                .gte(Expr::val(max_i32)),
            Expr::val(Some(CleanupStatus::Failed)),
        )
        .finally(Expr::val(Some(CleanupStatus::Pending)))
        .into();

        let result = Entity::update_many()
            .col_expr(Column::CleanupStatus, new_status_expr)
            .col_expr(
                Column::CleanupAttempts,
                Expr::col(Column::CleanupAttempts).add(Expr::val(1i32)),
            )
            .col_expr(
                Column::LastCleanupError,
                Expr::value(Some(error.to_owned())),
            )
            .col_expr(Column::CleanupUpdatedAt, Expr::value(Some(now)))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(attachment_id))
                    .add(Column::CleanupStatus.eq(CleanupStatus::Pending)),
            )
            .secure()
            .scope_with(&scope)
            .exec(runner)
            .await
            .map_err(db_err)?;

        if result.rows_affected == 0 {
            // Already terminal (done/failed) or not found -- stale redelivery.
            return Ok(CleanupOutcome::AlreadyTerminal);
        }

        // Read back to determine whether we hit 'failed' or stayed 'pending'.
        let row = Entity::find_by_id(attachment_id)
            .secure()
            .scope_with(&scope)
            .one(runner)
            .await
            .map_err(db_err)?;

        match row.and_then(|r| r.cleanup_status) {
            Some(CleanupStatus::Failed) => Ok(CleanupOutcome::TerminalFailure),
            _ => Ok(CleanupOutcome::StillPending),
        }
    }

    async fn mark_attachments_pending_for_chat<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        let scope = AccessScope::allow_all();
        let now = OffsetDateTime::now_utc();
        let result = Entity::update_many()
            .col_expr(
                Column::CleanupStatus,
                Expr::value(Some(CleanupStatus::Pending)),
            )
            .col_expr(Column::CleanupUpdatedAt, Expr::value(Some(now)))
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::CleanupStatus.is_null())
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(&scope)
            .exec(runner)
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected)
    }

    async fn count_failed_cleanup_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        let scope = AccessScope::allow_all();
        let count = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::CleanupStatus.eq(CleanupStatus::Failed)),
            )
            .secure()
            .scope_with(&scope)
            .count(runner)
            .await
            .map_err(db_err)?;
        Ok(count)
    }
}
