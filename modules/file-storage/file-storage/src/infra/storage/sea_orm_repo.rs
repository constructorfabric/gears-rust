//! SeaORM-backed implementation of `FilesRepo` (P1).

use async_trait::async_trait;
use file_storage_sdk::{FileInfo, FileMetaUpdate, FileStatus};
use modkit_db::secure::{
    AccessScope, DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use sea_orm::{
    ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    sea_query::Expr,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    ChangeStatusOutcome, FilesRepo, InsertPendingArgs, ListFilesArgs, ListFilesPage,
    MutationOutcome,
};

use super::entity::{self, Column, Entity as FilesEntity};
use super::mapper::{entity_to_file_info, status_sdk_to_str};

pub struct SeaOrmFilesRepository;

impl SeaOrmFilesRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmFilesRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn scope_for(tenant_id: Uuid) -> AccessScope {
    AccessScope::for_tenant(tenant_id)
}

#[async_trait]
impl FilesRepo for SeaOrmFilesRepository {
    async fn insert_pending<C: DBRunner>(
        &self,
        runner: &C,
        args: InsertPendingArgs,
    ) -> Result<FileInfo, DomainError> {
        let am = entity::ActiveModel {
            id: Set(args.file_id),
            tenant_id: Set(args.tenant_id),
            backend_id: Set(args.backend_id),
            file_path: Set(args.file_path.clone()),
            owner_id: Set(args.owner_id),
            name: Set(args.name.clone()),
            gts_file_type: Set(args.gts_file_type.clone()),
            mime_type: Set(args.mime_type.clone()),
            size_bytes: Set(0),
            etag: Set(args.etag_pinned.clone()),
            version_id: Set(None),
            status: Set("pending_upload".to_owned()),
            custom_metadata: Set(args.custom_metadata_json),
            upload_expires_at: Set(args.upload_expires_at),
            created_at: Set(args.now),
            updated_at: Set(args.now),
        };
        let scope = scope_for(args.tenant_id);
        let inserted = secure_insert::<FilesEntity>(am, &scope, runner).await?;
        Ok(entity_to_file_info(inserted))
    }

    async fn get_by_id<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
    ) -> Result<Option<FileInfo>, DomainError> {
        let scope = scope_for(tenant_id);
        let row = FilesEntity::find()
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .secure()
            .scope_with(&scope)
            .one(runner)
            .await?;
        Ok(row.map(entity_to_file_info))
    }

    async fn get_by_id_system<C: DBRunner>(
        &self,
        _runner: &C,
        _file_id: Uuid,
    ) -> Result<Option<FileInfo>, DomainError> {
        // System-context find — used by P2 GC sweep / reconciliation worker.
        // P1 stub: returns None (caller does not use this path in P1).
        Ok(None)
    }

    async fn begin_complete_upload<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<MutationOutcome, DomainError> {
        let scope = scope_for(tenant_id);
        let result = FilesEntity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(Column::Status, Expr::value("completing"))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .filter(sea_orm::Condition::all().add(Column::Status.eq("pending_upload")))
            .exec(runner)
            .await?;
        Ok(if result.rows_affected >= 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::NoMatch
        })
    }

    async fn finish_complete_upload<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        new_etag: &str,
        new_version_id: Option<&str>,
        new_size_bytes: u64,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError> {
        let scope = scope_for(tenant_id);
        let result = FilesEntity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(Column::Status, Expr::value("uploaded"))
            .col_expr(Column::Etag, Expr::value(new_etag.to_owned()))
            .col_expr(
                Column::VersionId,
                Expr::value(new_version_id.map(|s| s.to_owned())),
            )
            .col_expr(
                Column::SizeBytes,
                Expr::value(i64::try_from(new_size_bytes).unwrap_or(0)),
            )
            .col_expr(
                Column::UploadExpiresAt,
                Expr::value(None::<OffsetDateTime>),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .filter(sea_orm::Condition::all().add(Column::Status.eq("completing")))
            .exec(runner)
            .await?;
        if result.rows_affected == 0 {
            return Ok(ChangeStatusOutcome::NoMatch);
        }
        let row = self.get_by_id(runner, tenant_id, file_id).await?;
        Ok(row
            .map(ChangeStatusOutcome::Applied)
            .unwrap_or(ChangeStatusOutcome::NoMatch))
    }

    async fn begin_meta_update<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: Option<&str>,
        old_version_id: Option<&str>,
        update: &FileMetaUpdate,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError> {
        let scope = scope_for(tenant_id);
        let mut q = FilesEntity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(Column::Status, Expr::value("meta_updating"))
            .col_expr(Column::UpdatedAt, Expr::value(now));

        if let Some(name) = &update.name {
            q = q.col_expr(Column::Name, Expr::value(name.clone()));
        }
        if let Some(mime) = &update.mime_type {
            q = q.col_expr(Column::MimeType, Expr::value(mime.clone()));
        }
        if let Some(custom) = &update.custom_metadata {
            let json = serde_json::to_string(custom).map_err(|e| {
                DomainError::internal(format!("custom_metadata serialisation failed: {e}"))
            })?;
            q = q.col_expr(Column::CustomMetadata, Expr::value(json));
        }

        let mut q = q
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .filter(sea_orm::Condition::all().add(Column::Status.eq("uploaded")));
        if let Some(etag) = old_etag {
            q = q.filter(sea_orm::Condition::all().add(Column::Etag.eq(etag)));
        }
        if let Some(vid) = old_version_id {
            q = q.filter(sea_orm::Condition::all().add(Column::VersionId.eq(vid)));
        }
        let result = q.exec(runner).await?;
        if result.rows_affected == 0 {
            return Ok(ChangeStatusOutcome::NoMatch);
        }
        let row = self.get_by_id(runner, tenant_id, file_id).await?;
        Ok(row
            .map(ChangeStatusOutcome::Applied)
            .unwrap_or(ChangeStatusOutcome::NoMatch))
    }

    async fn finish_meta_update<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        new_etag: &str,
        new_version_id: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError> {
        let scope = scope_for(tenant_id);
        let result = FilesEntity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(Column::Status, Expr::value("uploaded"))
            .col_expr(Column::Etag, Expr::value(new_etag.to_owned()))
            .col_expr(
                Column::VersionId,
                Expr::value(new_version_id.map(|s| s.to_owned())),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .filter(sea_orm::Condition::all().add(Column::Status.eq("meta_updating")))
            .exec(runner)
            .await?;
        if result.rows_affected == 0 {
            return Ok(ChangeStatusOutcome::NoMatch);
        }
        let row = self.get_by_id(runner, tenant_id, file_id).await?;
        Ok(row
            .map(ChangeStatusOutcome::Applied)
            .unwrap_or(ChangeStatusOutcome::NoMatch))
    }

    async fn begin_delete<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: Option<&str>,
        old_version_id: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<MutationOutcome, DomainError> {
        let scope = scope_for(tenant_id);
        let mut q = FilesEntity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(Column::Status, Expr::value("deleting"))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .filter(sea_orm::Condition::all().add(Column::Status.eq("uploaded")));
        if let Some(etag) = old_etag {
            q = q.filter(sea_orm::Condition::all().add(Column::Etag.eq(etag)));
        }
        if let Some(vid) = old_version_id {
            q = q.filter(sea_orm::Condition::all().add(Column::VersionId.eq(vid)));
        }
        let result = q.exec(runner).await?;
        Ok(if result.rows_affected >= 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::NoMatch
        })
    }

    async fn finish_delete<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
    ) -> Result<MutationOutcome, DomainError> {
        let scope = scope_for(tenant_id);
        let result = FilesEntity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .filter(sea_orm::Condition::all().add(Column::Status.eq("deleting")))
            .exec(runner)
            .await?;
        Ok(if result.rows_affected >= 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::NoMatch
        })
    }

    async fn delete_pending_upload<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
    ) -> Result<MutationOutcome, DomainError> {
        let scope = scope_for(tenant_id);
        let result = FilesEntity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(sea_orm::Condition::all().add(Column::Id.eq(file_id)))
            .filter(sea_orm::Condition::all().add(Column::Status.eq("pending_upload")))
            .exec(runner)
            .await?;
        Ok(if result.rows_affected >= 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::NoMatch
        })
    }

    async fn rollforward_to_uploaded_system<C: DBRunner>(
        &self,
        _runner: &C,
        _file_id: Uuid,
        _from_status: FileStatus,
        _new_etag: &str,
        _new_version_id: Option<&str>,
        _new_size_bytes: u64,
        _now: OffsetDateTime,
    ) -> Result<MutationOutcome, DomainError> {
        // System-context UPDATE used by in-band recovery on read_file when
        // a row is stuck in transient state. P1 stub: returns NoMatch
        // (no rollforward attempted in this build). Production-grade
        // wiring requires a system-context runner that bypasses the
        // tenant scope guard — handled by modkit_db's secure layer
        // through DbConn variants, which require additional plumbing
        // not yet wired into FileStorage.
        let _ = status_sdk_to_str;
        Ok(MutationOutcome::NoMatch)
    }

    async fn list_paginated<C: DBRunner>(
        &self,
        runner: &C,
        args: ListFilesArgs,
    ) -> Result<ListFilesPage, DomainError> {
        let limit = args.limit.clamp(1, 1000) as u64;
        let scope = scope_for(args.tenant_id);
        let mut q = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .order_by(Column::CreatedAt, sea_orm::Order::Desc)
            .order_by(Column::Id, sea_orm::Order::Asc)
            .limit(limit + 1);
        if let Some(owner) = args.owner_id {
            q = q.filter(sea_orm::Condition::all().add(Column::OwnerId.eq(owner)));
        }
        if let Some(cursor) = args.cursor.as_deref() {
            if let Ok(uuid) = Uuid::parse_str(cursor) {
                q = q.filter(sea_orm::Condition::all().add(Column::Id.gt(uuid)));
            }
        }
        let mut rows = q.all(runner).await?;
        let next_cursor = if rows.len() as u64 > limit {
            let last = rows.pop().unwrap();
            Some(last.id.to_string())
        } else {
            None
        };
        let items = rows.into_iter().map(entity_to_file_info).collect();
        Ok(ListFilesPage { items, next_cursor })
    }
}
