use async_trait::async_trait;
use modkit_db::secure::{DBRunner, SecureEntityExt, secure_insert};
use modkit_security::AccessScope;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect, RelationTrait, Set,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::InsertMessageAttachmentParams;
use crate::infra::db::entity::attachment::Column as AttCol;
use crate::infra::db::entity::message_attachment::{ActiveModel, Column, Entity, Relation};

fn db_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::database(e.to_string())
}

/// Repository for `message_attachments` join-table operations.
pub struct MessageAttachmentRepository;

#[async_trait]
impl crate::domain::repos::MessageAttachmentRepository for MessageAttachmentRepository {
    async fn insert_batch<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: &[InsertMessageAttachmentParams],
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();
        for p in params {
            let am = ActiveModel {
                tenant_id: Set(p.tenant_id),
                chat_id: Set(p.chat_id),
                message_id: Set(p.message_id),
                attachment_id: Set(p.attachment_id),
                created_at: Set(now),
            };
            secure_insert::<Entity>(am, scope, runner).await?;
        }
        Ok(())
    }

    async fn copy_for_retry<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        original_message_id: Uuid,
        new_message_id: Uuid,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        // SELECT existing message_attachments for the original message,
        // JOIN with attachments to exclude soft-deleted ones.
        let existing = Entity::find()
            .join(JoinType::InnerJoin, Relation::Attachment.def())
            .filter(
                Condition::all()
                    .add(Column::MessageId.eq(original_message_id))
                    .add(Column::ChatId.eq(chat_id))
                    .add(AttCol::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(db_err)?;

        let now = OffsetDateTime::now_utc();
        let mut copied = 0u64;
        for row in &existing {
            let am = ActiveModel {
                tenant_id: Set(row.tenant_id),
                chat_id: Set(row.chat_id),
                message_id: Set(new_message_id),
                attachment_id: Set(row.attachment_id),
                created_at: Set(now),
            };
            secure_insert::<Entity>(am, scope, runner).await?;
            copied += 1;
        }
        Ok(copied)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "message_attachment_repo_tests.rs"]
mod tests;
