//! SeaORM-backed implementation of `FilesRepo`.

use async_trait::async_trait;
use file_storage_sdk::{FileInfo, FileMetaUpdate, FileStatus};
use modkit_db::secure::{
    AccessScope, DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, IntoSimpleExpr, Set, sea_query::Expr,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::etag::compose;
use crate::domain::repo::{
    ChangeStatusOutcome, DeleteOutcome, FilesRepo, InsertPendingArgs, ListFilesArgs,
    ListFilesPage, MutationOutcome, PersistenceFields,
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
            meta_revision: Set(0),
            status: Set("pending_upload".to_owned()),
            custom_metadata: Set(args.custom_metadata_json.clone()),
            upload_expires_at: Set(args.upload_expires_at),
            created_at: Set(args.now),
            modified_at: Set(args.now),
        };

        let scope = AccessScope::allow_all();
        FilesEntity::insert(am)
            .secure()
            .scope_unchecked(&scope)
            .map_err(DomainError::from)?
            .exec(runner)
            .await
            .map_err(DomainError::from)?;

        let model = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(Column::Id.eq(args.file_id)))
            .one(runner)
            .await
            .map_err(DomainError::from)?
            .ok_or(DomainError::Internal(
                "freshly-inserted row not visible".to_owned(),
            ))?;
        Ok(entity_to_file_info(model))
    }

    async fn get_by_id<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
    ) -> Result<Option<FileInfo>, DomainError> {
        let scope = AccessScope::allow_all();
        let model = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::Id.eq(file_id)),
            )
            .one(runner)
            .await
            .map_err(DomainError::from)?;
        Ok(model.map(entity_to_file_info))
    }

    async fn get_by_id_system<C: DBRunner>(
        &self,
        runner: &C,
        file_id: Uuid,
    ) -> Result<Option<FileInfo>, DomainError> {
        let scope = AccessScope::allow_all();
        let model = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(Column::Id.eq(file_id)))
            .one(runner)
            .await
            .map_err(DomainError::from)?;
        Ok(model.map(entity_to_file_info))
    }

    async fn get_persistence_fields<C: DBRunner>(
        &self,
        runner: &C,
        file_id: Uuid,
    ) -> Result<Option<PersistenceFields>, DomainError> {
        let scope = AccessScope::allow_all();
        let model = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(Column::Id.eq(file_id)))
            .one(runner)
            .await
            .map_err(DomainError::from)?;
        Ok(model.map(|m| PersistenceFields {
            backend_id: m.backend_id,
            meta_revision: m.meta_revision,
        }))
    }

    async fn update_metadata_etag_conditional<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: &str,
        update: &FileMetaUpdate,
        new_content_hash: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError> {
        let scope = AccessScope::allow_all();
        let existing = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::Id.eq(file_id)),
            )
            .one(runner)
            .await
            .map_err(DomainError::from)?;
        let Some(existing) = existing else {
            return Ok(ChangeStatusOutcome::NoMatch);
        };

        let new_meta_revision = existing.meta_revision + 1;
        let derived_hash = derive_content_hash(&existing);
        let content_hash_for_etag = new_content_hash.unwrap_or(derived_hash.as_str());
        let new_etag = compose(content_hash_for_etag, new_meta_revision);

        let mut update_many = FilesEntity::update_many();
        update_many = update_many.col_expr(Column::Etag, Expr::value(new_etag.clone()));
        update_many =
            update_many.col_expr(Column::MetaRevision, Expr::value(new_meta_revision));
        update_many = update_many.col_expr(Column::ModifiedAt, Expr::value(now));

        if let Some(name) = &update.name {
            update_many = update_many.col_expr(Column::Name, Expr::value(name.clone()));
        }
        if let Some(mime_type) = &update.mime_type {
            update_many =
                update_many.col_expr(Column::MimeType, Expr::value(mime_type.clone()));
        }
        if let Some(custom) = &update.custom_metadata {
            let serialised = serde_json::to_string(custom).map_err(|e| {
                DomainError::internal(format!("custom_metadata serialisation failed: {e}"))
            })?;
            update_many =
                update_many.col_expr(Column::CustomMetadata, Expr::value(serialised));
        }

        let res = update_many
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::Id.eq(file_id))
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::Etag.eq(old_etag))
                    .add(Column::Status.eq("uploaded")),
            )
            .exec(runner)
            .await
            .map_err(DomainError::from)?;

        if res.rows_affected == 0 {
            return Ok(ChangeStatusOutcome::NoMatch);
        }

        let updated = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(Column::Id.eq(file_id)))
            .one(runner)
            .await
            .map_err(DomainError::from)?
            .ok_or(DomainError::NotFound)?;

        Ok(ChangeStatusOutcome::Applied(entity_to_file_info(updated)))
    }

    async fn change_status_with_supersession<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: &str,
        target: FileStatus,
        new_content_hash: &str,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError> {
        let scope = AccessScope::allow_all();
        let target_str = status_sdk_to_str(target);

        let existing = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::Id.eq(file_id)),
            )
            .one(runner)
            .await
            .map_err(DomainError::from)?;
        let Some(existing) = existing else {
            return Ok(ChangeStatusOutcome::NoMatch);
        };

        let new_meta_revision = existing.meta_revision + 1;
        let new_etag = compose(new_content_hash, new_meta_revision);

        let res = FilesEntity::update_many()
            .col_expr(Column::Etag, Expr::value(new_etag.clone()))
            .col_expr(Column::MetaRevision, Expr::value(new_meta_revision))
            .col_expr(Column::Status, Expr::value(target_str))
            .col_expr(Column::ModifiedAt, Expr::value(now))
            .col_expr(
                Column::UploadExpiresAt,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::Id.eq(file_id))
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::Etag.eq(old_etag)),
            )
            .exec(runner)
            .await
            .map_err(DomainError::from)?;

        if res.rows_affected == 0 {
            return Ok(ChangeStatusOutcome::NoMatch);
        }

        FilesEntity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::BackendId.eq(existing.backend_id))
                    .add(Column::FilePath.eq(existing.file_path.clone()))
                    .add(Column::Status.eq("uploaded"))
                    .add(Column::Id.ne(file_id)),
            )
            .exec(runner)
            .await
            .map_err(DomainError::from)?;

        let updated = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(Column::Id.eq(file_id)))
            .one(runner)
            .await
            .map_err(DomainError::from)?
            .ok_or(DomainError::NotFound)?;
        Ok(ChangeStatusOutcome::Applied(entity_to_file_info(updated)))
    }

    async fn delete_etag_conditional<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: &str,
    ) -> Result<DeleteOutcome, DomainError> {
        let scope = AccessScope::allow_all();

        let existing = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::Id.eq(file_id)),
            )
            .one(runner)
            .await
            .map_err(DomainError::from)?;
        let Some(existing) = existing else {
            return Ok(DeleteOutcome {
                outcome: MutationOutcome::NoMatch,
                backend_id: None,
            });
        };

        let res = FilesEntity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::Id.eq(file_id))
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::Etag.eq(old_etag))
                    .add(Column::Status.eq("uploaded")),
            )
            .exec(runner)
            .await
            .map_err(DomainError::from)?;

        if res.rows_affected == 0 {
            return Ok(DeleteOutcome {
                outcome: MutationOutcome::NoMatch,
                backend_id: None,
            });
        }

        Ok(DeleteOutcome {
            outcome: MutationOutcome::Applied,
            backend_id: Some(existing.backend_id),
        })
    }

    async fn repair_etag_system_context<C: DBRunner>(
        &self,
        runner: &C,
        file_id: Uuid,
        old_etag: &str,
        new_etag: &str,
    ) -> Result<MutationOutcome, DomainError> {
        let scope = AccessScope::allow_all();
        let res = FilesEntity::update_many()
            .col_expr(Column::Etag, Expr::value(new_etag.to_owned()))
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(Column::Id.eq(file_id))
                    .add(Column::Etag.eq(old_etag)),
            )
            .exec(runner)
            .await
            .map_err(DomainError::from)?;

        if res.rows_affected == 0 {
            Ok(MutationOutcome::NoMatch)
        } else {
            Ok(MutationOutcome::Applied)
        }
    }

    async fn list_paginated<C: DBRunner>(
        &self,
        runner: &C,
        args: ListFilesArgs,
    ) -> Result<ListFilesPage, DomainError> {
        let scope = AccessScope::allow_all();
        let mut cond = Condition::all().add(Column::TenantId.eq(args.tenant_id));

        if let Some(owner) = args.owner_id {
            cond = cond.add(Column::OwnerId.eq(owner));
        }

        if let Some(backend_id) = args.backend_id {
            cond = cond.add(Column::BackendId.eq(backend_id));
        }
        if let Some(mt) = args.mime_type {
            cond = cond.add(Column::MimeType.eq(mt));
        }
        if let Some(gts) = args.gts_file_type {
            cond = cond.add(Column::GtsFileType.eq(gts));
        }
        if let Some(after) = args.created_after {
            cond = cond.add(Column::CreatedAt.gte(after));
        }
        if let Some(before) = args.created_before {
            cond = cond.add(Column::CreatedAt.lte(before));
        }

        if let Some(cursor) = &args.cursor
            && let Ok(ts) = OffsetDateTime::parse(cursor, &time::format_description::well_known::Rfc3339)
        {
            cond = cond.add(Column::CreatedAt.lt(ts));
        }

        let limit = u64::from(args.limit);
        let limit_plus_one = limit + 1;

        let mut models = FilesEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(cond)
            .order_by(Column::CreatedAt.into_simple_expr(), sea_orm::Order::Desc)
            .limit(limit_plus_one)
            .all(runner)
            .await
            .map_err(DomainError::from)?;

        let next_cursor = if u64::try_from(models.len()).unwrap_or(0) > limit {
            let extra = models.pop();
            extra.map(|m| {
                m.created_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default()
            })
        } else {
            None
        };

        let items = models.into_iter().map(entity_to_file_info).collect();

        Ok(ListFilesPage { items, next_cursor })
    }
}

/// Best-effort derivation of the row's content hash for etag composition
/// when the caller doesn't explicitly supply a new content hash.
fn derive_content_hash(existing: &entity::Model) -> String {
    existing.etag.clone()
}
