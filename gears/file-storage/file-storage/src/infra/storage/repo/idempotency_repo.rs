//! Repository for the `idempotency_keys` table.
//!
//! Insert-or-fetch: the first call stores the record, a retry gets it back unchanged.
//! Queries are keyed by `(tenant_id, owner_kind, owner_id, key)`.

use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::idempotency::IdempotencyRecord;
use crate::infra::storage::entity::idempotency_key::{ActiveModel, Column, Entity, Model};
use crate::infra::storage::store::IdempotencyInsert;

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
            .map_err(DomainError::from)?;
        Ok(found.map(record_from_model))
    }

    /// Insert an idempotency record, first deleting an **expired** row for the same key.
    ///
    /// Runs in the same transaction as the file creation it records. Failures
    /// propagate so the transaction rolls back; a live-key conflict from a racing
    /// create rolls that creation back and the client retries and replays the
    /// winner's record via `get`.
    pub async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        idem: &IdempotencyInsert,
        file_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        // Only a lapsed row is removed; a live row stays so the insert below hits the
        // PK and rolls back, preventing a duplicate file.
        Entity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(idem.tenant_id))
                    .add(Column::OwnerKind.eq(idem.owner_kind.clone()))
                    .add(Column::OwnerId.eq(idem.owner_id))
                    .add(Column::IdempotencyKey.eq(idem.key.clone()))
                    .add(Column::ExpiresAt.lte(now)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(DomainError::from)?;

        let am = ActiveModel {
            tenant_id: Set(idem.tenant_id),
            owner_kind: Set(idem.owner_kind.clone()),
            owner_id: Set(idem.owner_id),
            idempotency_key: Set(idem.key.clone()),
            subject_id: Set(idem.subject_id),
            file_id: Set(file_id),
            response_status: Set(idem.response_status),
            response_body: Set(idem.response_body.clone()),
            response_etag: Set(idem.response_etag.clone()),
            request_hash: Set(idem.request_hash.clone()),
            created_at: Set(now),
            expires_at: Set(idem.expires_at),
        };
        secure_insert::<Entity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Bulk-delete all rows whose `expires_at` is at or before `now`.
    ///
    /// Used by the cleanup sweep; returns the number of rows removed.
    pub async fn delete_expired<C: DBRunner>(
        &self,
        conn: &C,
        now: OffsetDateTime,
    ) -> Result<u64, DomainError> {
        let res = Entity::delete_many()
            .filter(Column::ExpiresAt.lte(now))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(res.rows_affected)
    }
}

fn record_from_model(m: Model) -> IdempotencyRecord {
    IdempotencyRecord {
        file_id: m.file_id,
        subject_id: m.subject_id,
        response_status: u16::try_from(m.response_status).unwrap_or(201),
        response_body: m.response_body,
        response_etag: m.response_etag,
        request_hash: m.request_hash,
    }
}
