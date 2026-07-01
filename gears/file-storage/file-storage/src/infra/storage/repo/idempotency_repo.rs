//! Repository for the `idempotency_keys` table.
//!
//! Provides insert-or-fetch semantics: on the first call with a given key the
//! record is inserted; on a retry the stored record is returned unchanged.
//! All queries are scoped by `(tenant_id, owner_kind, owner_id, key)`.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::idempotency::IdempotencyRecord;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::idempotency_key::{ActiveModel, Column, Entity, Model};

/// Repository for idempotency key records.
#[derive(Clone, Default)]
pub struct IdempotencyRepo;

impl IdempotencyRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Fetch an idempotency record if it exists and has not expired.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        tenant_id: Uuid,
        owner_kind: &str,
        owner_id: Uuid,
        key: &str,
        now: OffsetDateTime,
    ) -> Result<Option<IdempotencyRecord>, DomainError> {
        let found = Entity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::OwnerKind.eq(owner_kind))
                    .add(Column::OwnerId.eq(owner_id))
                    .add(Column::IdempotencyKey.eq(key))
                    .add(Column::ExpiresAt.gt(now)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(conn)
            .await
            .map_err(db_err)?;
        Ok(found.map(record_from_model))
    }

    /// Insert a new idempotency record. If a record with the same PK already
    /// exists (e.g., concurrent request), silently ignore the conflict.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        tenant_id: Uuid,
        owner_kind: &str,
        owner_id: Uuid,
        key: &str,
        file_id: Uuid,
        response_status: i32,
        response_body: &str,
        response_etag: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let am = ActiveModel {
            tenant_id: Set(tenant_id),
            owner_kind: Set(owner_kind.to_owned()),
            owner_id: Set(owner_id),
            idempotency_key: Set(key.to_owned()),
            file_id: Set(file_id),
            response_status: Set(response_status),
            response_body: Set(response_body.to_owned()),
            response_etag: Set(response_etag.to_owned()),
            created_at: Set(now),
            expires_at: Set(expires_at),
        };
        // Ignore PK conflicts — a concurrent retry already stored the record.
        drop(
            secure_insert::<Entity>(am, &AccessScope::allow_all(), conn)
                .await
                .map_err(db_err),
        );
        Ok(())
    }
}

fn record_from_model(m: Model) -> IdempotencyRecord {
    IdempotencyRecord {
        file_id: m.file_id,
        response_status: u16::try_from(m.response_status).unwrap_or(201),
        response_body: m.response_body,
        response_etag: m.response_etag,
    }
}
