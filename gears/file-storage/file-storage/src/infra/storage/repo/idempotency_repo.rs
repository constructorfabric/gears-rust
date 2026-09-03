//! Repository for the `idempotency_keys` table.
//!
//! Provides insert-or-fetch semantics: on the first call with a given key the
//! record is inserted; on a retry the stored record is returned unchanged.
//! All queries are scoped by `(tenant_id, owner_kind, owner_id, key)`.

use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::idempotency::IdempotencyRecord;
use crate::infra::storage::db::{conflict_on_unique_violation, db_err};
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
            .map_err(db_err)?;
        Ok(found.map(record_from_model))
    }

    /// Insert an idempotency record, replacing any prior row for the same key.
    ///
    /// This runs inside the same transaction as the file creation it records,
    /// so a committed create always leaves a replay record behind. A stale
    /// **expired** row for the same key is deleted first (its TTL lapsed, so the
    /// new request legitimately supersedes it — a bare insert would collide with
    /// the leftover primary key). Every failure is propagated — never swallowed —
    /// so the surrounding transaction rolls back rather than reporting success
    /// with no persisted record. A live-key conflict from a concurrent create
    /// racing the same key therefore also rolls that creation back; the client
    /// retries and replays the winner's record via [`get`] instead of creating a
    /// second file.
    ///
    /// Takes the bundled [`IdempotencyInsert`] plus the two fields it does not
    /// carry (`file_id`, produced by the caller's create-file flow, and `now`)
    /// instead of a long positional-argument list.
    pub async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        idem: &IdempotencyInsert,
        file_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        // Remove only a lapsed row for this key first (insert-or-replace on an
        // expired PK). A still-live row is deliberately left in place: if a
        // concurrent create already committed a fresh row for this key, our
        // insert below then hits the primary key and rolls this creation back —
        // exactly the behaviour that stops a duplicate file from being created.
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
            .map_err(db_err)?;

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
        // The design documented above *relies on* this insert losing the
        // primary-key race against a concurrent identical request: that is
        // how a duplicate `POST /files` retried after a client-side timeout
        // is turned away without creating a second file. Classify that race
        // as a conflict rather than an opaque `db_err` 500, so the racing
        // caller gets a 409 it can react to (re-fetch via `get` and replay
        // the winner's stored response) rather than a request that looks
        // like it failed outright.
        secure_insert::<Entity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(|e| {
                conflict_on_unique_violation(
                    e,
                    "a request with this idempotency key is already being processed or has \
                     already completed",
                )
            })?;
        Ok(())
    }

    /// Bulk-delete all rows whose `expires_at` is at or before `now`.
    ///
    /// Called by the cleanup sweep so the `idempotency_keys` table doesn't
    /// grow unboundedly -- [`Self::insert`] only ever removes a lapsed row
    /// *for the same key*, never sweeps the whole table. Returns the number
    /// of rows removed.
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
            .map_err(db_err)?;
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
