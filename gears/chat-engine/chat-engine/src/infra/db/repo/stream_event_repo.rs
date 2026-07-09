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
use crate::domain::ports::{BufferedEvent, StreamEventBuffer};
use crate::infra::db::entity::stream_event::{
    self as stream_event_entity, Entity as StreamEventEntity,
};
use crate::infra::db::repo::ChatEngineDb;

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
#[path = "stream_event_repo_tests.rs"]
mod stream_event_repo_tests;
