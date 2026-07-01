//! Repository for the `events_outbox` table.
//!
//! @cpt-cf-file-storage-fr-file-events

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use toolkit_db::secure::{DBRunner, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::audit::FileEvent;
use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::events_outbox::{ActiveModel, Column, Entity, Model};

/// Repository over the `events_outbox` table.
#[derive(Clone, Default)]
pub struct EventsOutboxRepo;

impl EventsOutboxRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Enqueue a file-event row into `conn` (which MUST be a transaction runner
    /// so the row is committed atomically with the surrounding mutation).
    ///
    /// @cpt-cf-file-storage-fr-file-events
    pub async fn enqueue<C: DBRunner>(
        &self,
        conn: &C,
        event: &FileEvent,
    ) -> Result<(), DomainError> {
        let am = ActiveModel {
            event_id: Set(Uuid::now_v7()),
            tenant_id: Set(event.tenant_id),
            owner_id: Set(event.owner_id),
            file_id: Set(event.file_id),
            event_type: Set(event.event_type.clone()),
            payload: Set(event.payload.clone()),
            occurred_at: Set(time::OffsetDateTime::now_utc()),
            published_at: Set(None),
        };
        // No tenant scope on this table — allow_all() is intentional.
        secure_insert::<Entity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// List event rows for a specific file ordered by occurrence time — useful in tests.
    ///
    /// @cpt-cf-file-storage-fr-file-events
    pub async fn list_for_file<C: DBRunner>(
        &self,
        conn: &C,
        file_id: Uuid,
    ) -> Result<Vec<Model>, DomainError> {
        let rows = Entity::find()
            .filter(Column::FileId.eq(file_id))
            .order_by_asc(Column::OccurredAt)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_err)?;
        Ok(rows)
    }
}
