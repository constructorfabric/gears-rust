// Created: 2026-09-15 by Constructor Tech
//! `PendingSecretRepository` over `pending_secrets`.

use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secrets::pending::{PendingSecret, PendingSecretDraft, PendingSecretRepository};
use crate::infra::storage::entity::pending_secret::{self, Entity as PendingEntity};

/// The repository. Stateless: every operation takes its connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct PendingSecretRepo;

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

fn to_domain(model: pending_secret::Model) -> PendingSecret {
    PendingSecret {
        id: model.id,
        declaration_id: model.declaration_id,
        tenant_id: model.tenant_id,
        subject_id: model.subject_id,
        secret_ref: model.secret_ref,
        created_at: model.created_at,
        expires_at: model.expires_at,
    }
}

#[async_trait]
impl PendingSecretRepository for PendingSecretRepo {
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: PendingSecretDraft,
    ) -> Result<PendingSecret, DomainError> {
        let active = pending_secret::ActiveModel {
            id: Set(Uuid::new_v4()),
            declaration_id: Set(draft.declaration_id),
            tenant_id: Set(draft.tenant_id),
            subject_id: Set(draft.subject_id),
            secret_ref: Set(draft.secret_ref),
            created_at: Set(OffsetDateTime::now_utc()),
            expires_at: Set(draft.expires_at),
        };
        let model = toolkit_db::secure::secure_insert::<PendingEntity>(active, scope, conn)
            .await
            .map_err(db_error)?;
        Ok(to_domain(model))
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<PendingSecret>, DomainError> {
        let row = PendingEntity::find()
            .filter(pending_secret::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_error)?;
        Ok(row.map(to_domain))
    }

    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let outcome = PendingEntity::delete_many()
            .filter(pending_secret::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_error)?;
        Ok(outcome.rows_affected > 0)
    }

    async fn list_expired<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<PendingSecret>, DomainError> {
        let rows = PendingEntity::find()
            .filter(pending_secret::Column::ExpiresAt.lt(now))
            .order_by_asc(pending_secret::Column::ExpiresAt)
            .limit(limit)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(to_domain).collect())
    }
}
