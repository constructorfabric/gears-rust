//! Resume-buffer repository for the SSE delta stream (FR-024).
//!
//! Backs the [`StreamEventBuffer`] port with the `stream_events` table — the
//! default (DB) backend of the resume buffer described in DESIGN
//! `cpt-cf-chat-engine-design-stream-resume`. An optional Redis backend would
//! implement the same trait; off by default to stay within
//! `cpt-cf-chat-engine-constraint-single-database`.
//
// @cpt-cf-chat-engine-dbtable-stream-events:p2
// @cpt-cf-chat-engine-design-stream-resume:p2

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryOrder};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, SecureDeleteExt, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::infra::db::entity::stream_event::{
    self as stream_event_entity, Entity as StreamEventEntity,
};
use crate::infra::db::repo::ChatEngineDb;

/// One buffered wire event, returned by [`StreamEventBuffer::read_since`].
#[derive(Debug, Clone)]
pub struct BufferedEvent {
    /// Per-message sequence number (the SSE `id:`).
    pub seq: u64,
    /// Serialized wire event, replayed verbatim.
    pub event: JsonValue,
}

/// Short-TTL append-only buffer that bridges SSE reconnects (`Last-Event-ID`).
/// Not durable history — the persisted message is the durable record.
#[async_trait]
pub trait StreamEventBuffer: Send + Sync {
    /// Append `event` at `(message_id, seq)` with the given TTL deadline.
    /// Idempotent on the PK: a re-append of the same `(message_id, seq)` is a
    /// no-op (so a retried write never errors).
    async fn append(
        &self,
        message_id: Uuid,
        seq: u64,
        event: JsonValue,
        expires_at: OffsetDateTime,
    ) -> Result<(), ChatEngineError>;

    /// Return buffered events for `message_id` with `seq > after_seq` (or all
    /// when `after_seq` is `None`), ordered by `seq` ascending.
    async fn read_since(
        &self,
        message_id: Uuid,
        after_seq: Option<u64>,
    ) -> Result<Vec<BufferedEvent>, ChatEngineError>;

    /// Delete all rows whose `expires_at` is at or before `now`. Returns the
    /// number of rows removed. Called by the periodic TTL sweep.
    async fn delete_expired(&self, now: OffsetDateTime) -> Result<u64, ChatEngineError>;
}

/// SeaORM-backed [`StreamEventBuffer`] over the `stream_events` table.
pub struct SeaStreamEventBuffer {
    db: Arc<ChatEngineDb>,
}

impl SeaStreamEventBuffer {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }
}

/// `u64` seq → stored `i64`. Seq is a small per-message counter, so this never
/// realistically overflows; clamp defensively rather than panic.
fn seq_to_i64(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

#[async_trait]
impl StreamEventBuffer for SeaStreamEventBuffer {
    async fn append(
        &self,
        message_id: Uuid,
        seq: u64,
        event: JsonValue,
        expires_at: OffsetDateTime,
    ) -> Result<(), ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let am = stream_event_entity::ActiveModel {
            message_id: Set(message_id),
            seq: Set(seq_to_i64(seq)),
            event: Set(event),
            created_at: Set(OffsetDateTime::now_utc()),
            expires_at: Set(expires_at),
        };
        // Each `(message_id, seq)` is emitted exactly once by the projector
        // (only the originating stream appends; reconnects read), so a plain
        // insert never collides on the PK.
        StreamEventEntity::insert(am)
            .secure()
            .scope_unchecked(&scope)?
            .exec(&conn)
            .await?;
        Ok(())
    }

    async fn read_since(
        &self,
        message_id: Uuid,
        after_seq: Option<u64>,
    ) -> Result<Vec<BufferedEvent>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let mut cond = Condition::all().add(stream_event_entity::Column::MessageId.eq(message_id));
        if let Some(after) = after_seq {
            cond = cond.add(stream_event_entity::Column::Seq.gt(seq_to_i64(after)));
        }
        let rows = StreamEventEntity::find()
            .order_by_asc(stream_event_entity::Column::Seq)
            .secure()
            .scope_with(&scope)
            .filter(cond)
            .all(&conn)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| BufferedEvent {
                seq: u64::try_from(r.seq).unwrap_or(0),
                event: r.event,
            })
            .collect())
    }

    async fn delete_expired(&self, now: OffsetDateTime) -> Result<u64, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let res = StreamEventEntity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(stream_event_entity::Column::ExpiresAt.lte(now)))
            .exec(&conn)
            .await?;
        Ok(res.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::json;
    use time::Duration;
    use toolkit_db::{ConnectOpts, DBProvider, connect_db};

    async fn buffer() -> SeaStreamEventBuffer {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts)
            .await
            .expect("connect sqlite::memory:");
        toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("apply migrations");
        SeaStreamEventBuffer::new(Arc::new(DBProvider::new(db)))
    }

    fn far_future() -> OffsetDateTime {
        OffsetDateTime::now_utc() + Duration::hours(1)
    }

    #[tokio::test]
    async fn append_then_read_since_returns_ordered_tail() {
        let buf = buffer().await;
        let mid = Uuid::new_v4();
        for seq in 0..4u64 {
            buf.append(mid, seq, json!({ "seq": seq }), far_future())
                .await
                .expect("append");
        }
        // Full read.
        let all = buf.read_since(mid, None).await.expect("read all");
        assert_eq!(
            all.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        // Tail after seq 1.
        let tail = buf.read_since(mid, Some(1)).await.expect("read tail");
        assert_eq!(tail.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
        // Isolated per message.
        let other = buf
            .read_since(Uuid::new_v4(), None)
            .await
            .expect("read other");
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn delete_expired_removes_only_past_ttl() {
        let buf = buffer().await;
        let mid = Uuid::new_v4();
        let past = OffsetDateTime::now_utc() - Duration::minutes(1);
        buf.append(mid, 0, json!({}), past).await.unwrap();
        buf.append(mid, 1, json!({}), far_future()).await.unwrap();
        let removed = buf.delete_expired(OffsetDateTime::now_utc()).await.unwrap();
        assert_eq!(removed, 1);
        let left = buf.read_since(mid, None).await.unwrap();
        assert_eq!(left.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1]);
    }
}
