//! `MicrochatRepo` — `chat_attachments` queries against the same `Db`
//! that `cf-file-storage` uses. Two namespaces share one SQLite —
//! keeps the test harness simple. There is no FK to `cf-file-storage`'s
//! `files` table on purpose: the microchat keeps its own state and is
//! allowed to drift if the FS row is gone (mirrors real distributed
//! ownership of file metadata).

use file_storage_sdk::{Etag, FileId};
use modkit_db::secure::{
    AccessScope, DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use sea_orm::{
    ColumnTrait, EntityTrait, QueryFilter, Set, sea_query::Expr,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::entity::{self, Column, Entity as ChatAttachmentsEntity};
use crate::error::MicrochatError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentStatus {
    Pending,
    Active,
    Deleted,
}

impl AttachmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Deleted => "deleted",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub chat_id: Uuid,
    pub file_id: FileId,
    pub owner_id: Uuid,
    pub name: String,
    pub mime: String,
    pub status: AttachmentStatus,
    pub etag: Option<Etag>,
    pub size_bytes: Option<u64>,
    pub created_at: OffsetDateTime,
}

pub struct MicrochatRepo;

impl MicrochatRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub async fn insert_pending<C: DBRunner>(
        &self,
        runner: &C,
        file_id: FileId,
        chat_id: Uuid,
        owner_id: Uuid,
        name: &str,
        mime: &str,
        now: OffsetDateTime,
    ) -> Result<(), MicrochatError> {
        let am = entity::ActiveModel {
            file_id: Set(file_id),
            chat_id: Set(chat_id),
            owner_id: Set(owner_id),
            name: Set(name.to_owned()),
            mime: Set(mime.to_owned()),
            status: Set(AttachmentStatus::Pending.as_str().to_owned()),
            etag: Set(None),
            size_bytes: Set(None),
            created_at: Set(now
                .format(&Rfc3339)
                .map_err(|e| MicrochatError::Database(format!("rfc3339 format: {e}")))?),
        };
        secure_insert::<ChatAttachmentsEntity>(am, &allow_all(), runner)
            .await
            .map_err(map_scope_err)?;
        Ok(())
    }

    pub async fn mark_active<C: DBRunner>(
        &self,
        runner: &C,
        file_id: FileId,
        etag: &Etag,
        size_bytes: u64,
    ) -> Result<(), MicrochatError> {
        ChatAttachmentsEntity::update_many()
            .secure()
            .scope_with(&allow_all())
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Active.as_str()))
            .col_expr(Column::Etag, Expr::value(etag.clone()))
            .col_expr(Column::SizeBytes, Expr::value(size_bytes as i64))
            .filter(sea_orm::Condition::all().add(Column::FileId.eq(file_id)))
            .exec(runner)
            .await
            .map_err(map_scope_err)?;
        Ok(())
    }

    pub async fn update_after_meta_change<C: DBRunner>(
        &self,
        runner: &C,
        file_id: FileId,
        new_name: &str,
        new_mime: &str,
        new_etag: &Etag,
    ) -> Result<(), MicrochatError> {
        ChatAttachmentsEntity::update_many()
            .secure()
            .scope_with(&allow_all())
            .col_expr(Column::Name, Expr::value(new_name.to_owned()))
            .col_expr(Column::Mime, Expr::value(new_mime.to_owned()))
            .col_expr(Column::Etag, Expr::value(new_etag.clone()))
            .filter(sea_orm::Condition::all().add(Column::FileId.eq(file_id)))
            .exec(runner)
            .await
            .map_err(map_scope_err)?;
        Ok(())
    }

    pub async fn mark_deleted<C: DBRunner>(
        &self,
        runner: &C,
        file_id: FileId,
    ) -> Result<(), MicrochatError> {
        ChatAttachmentsEntity::update_many()
            .secure()
            .scope_with(&allow_all())
            .col_expr(Column::Status, Expr::value(AttachmentStatus::Deleted.as_str()))
            .filter(sea_orm::Condition::all().add(Column::FileId.eq(file_id)))
            .exec(runner)
            .await
            .map_err(map_scope_err)?;
        Ok(())
    }

    pub async fn delete_row<C: DBRunner>(
        &self,
        runner: &C,
        file_id: FileId,
    ) -> Result<(), MicrochatError> {
        ChatAttachmentsEntity::delete_many()
            .secure()
            .scope_with(&allow_all())
            .filter(sea_orm::Condition::all().add(Column::FileId.eq(file_id)))
            .exec(runner)
            .await
            .map_err(map_scope_err)?;
        Ok(())
    }

    /// Reject a new attach if the owner already holds `max` active or
    /// pending rows. Counts `pending` + `active`, never `deleted`.
    pub async fn enforce_quota<C: DBRunner>(
        &self,
        runner: &C,
        owner_id: Uuid,
        max: u32,
    ) -> Result<(), MicrochatError> {
        let count = self.count_active_for_owner(runner, owner_id).await?;
        if count >= max {
            return Err(MicrochatError::QuotaExceeded { max });
        }
        Ok(())
    }

    pub async fn count_active_for_owner<C: DBRunner>(
        &self,
        runner: &C,
        owner_id: Uuid,
    ) -> Result<u32, MicrochatError> {
        let count = ChatAttachmentsEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(Column::OwnerId.eq(owner_id))
                    .add(
                        sea_orm::Condition::any()
                            .add(Column::Status.eq(AttachmentStatus::Pending.as_str()))
                            .add(Column::Status.eq(AttachmentStatus::Active.as_str())),
                    ),
            )
            .secure()
            .scope_with(&allow_all())
            .count(runner)
            .await
            .map_err(map_scope_err)?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    pub async fn find<C: DBRunner>(
        &self,
        runner: &C,
        file_id: FileId,
    ) -> Result<Option<Attachment>, MicrochatError> {
        let row = ChatAttachmentsEntity::find()
            .filter(sea_orm::Condition::all().add(Column::FileId.eq(file_id)))
            .secure()
            .scope_with(&allow_all())
            .one(runner)
            .await
            .map_err(map_scope_err)?;
        row.map(model_to_attachment).transpose()
    }

    pub async fn list_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
    ) -> Result<Vec<Attachment>, MicrochatError> {
        let rows = ChatAttachmentsEntity::find()
            .filter(sea_orm::Condition::all().add(Column::ChatId.eq(chat_id)))
            .secure()
            .scope_with(&allow_all())
            .all(runner)
            .await
            .map_err(map_scope_err)?;
        rows.into_iter().map(model_to_attachment).collect()
    }
}

impl Default for MicrochatRepo {
    fn default() -> Self {
        Self::new()
    }
}

fn allow_all() -> AccessScope {
    AccessScope::allow_all()
}

fn map_scope_err<E: std::fmt::Display>(e: E) -> MicrochatError {
    MicrochatError::Database(e.to_string())
}

fn model_to_attachment(m: entity::Model) -> Result<Attachment, MicrochatError> {
    let status = AttachmentStatus::from_str(&m.status)
        .ok_or(MicrochatError::Database(format!("invalid status `{}`", m.status)))?;
    let created_at = OffsetDateTime::parse(&m.created_at, &Rfc3339)
        .map_err(|e| MicrochatError::Database(format!("created_at parse: {e}")))?;
    Ok(Attachment {
        chat_id: m.chat_id,
        file_id: m.file_id,
        owner_id: m.owner_id,
        name: m.name,
        mime: m.mime,
        status,
        etag: m.etag,
        size_bytes: m.size_bytes.map(|n| n as u64),
        created_at,
    })
}
