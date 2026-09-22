//! Tells subscribers which of their batches are stuck retrying an entity.
//!
//! Separate from the notifier because it answers a different question with a
//! different query: the notifier delivers a finished batch's outcome once,
//! this reports an unfinished batch's situation for as long as it lasts. A
//! failure of either must not stop the other, and a completion should not wait
//! behind a retry report.
//!
//! Gated on somebody actually watching retries. A caller that only awaits
//! completion never asks for state changes, so an instance whose callers all do
//! that issues no query here at all.

use std::collections::HashSet;
use std::sync::Arc;

use sea_orm::{FromQueryResult, Statement};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::core::Outbox;
use super::super::store::OutboxStore;
use super::super::taskward::{Directive, WorkerAction};
use super::super::trace::TraceState;
use super::super::types::OutboxError;
use crate::Db;

#[derive(Debug, FromQueryResult)]
struct RetryRow {
    trace: String,
    entities: i64,
    pending: i64,
    failures: i64,
    attempts: i64,
    last_error: Option<String>,
    retrying_since: chrono::DateTime<chrono::Utc>,
}

impl RetryRow {
    fn into_state(self) -> TraceState {
        TraceState::Retrying {
            entities: self.entities,
            pending: self.pending,
            failures: self.failures,
            attempts: self.attempts,
            last_error: self.last_error,
            retrying_since: self.retrying_since,
        }
    }
}

/// Pushes the retry state of this instance's traces to whoever is watching them.
pub struct RetryReporter {
    pub outbox: Arc<Outbox>,
    pub db: Db,
    pub batch_size: u32,
}

impl WorkerAction for RetryReporter {
    /// How many retrying traces were reported this pass, for the stats listener.
    type Payload = u64;
    type Error = OutboxError;

    async fn execute(
        &mut self,
        _cancel: &CancellationToken,
    ) -> Result<Directive<u64>, Self::Error> {
        let mailbox = self.outbox.mailbox();
        if !mailbox.subscriptions().wants_retry_reports() {
            // Nobody is watching retries, so there is nothing a query could tell us.
            return Ok(Directive::Idle(0));
        }

        let store = OutboxStore::new(self.outbox.statements());
        let conn = self.db.sea_internal();
        let limit = i64::from(self.batch_size);
        let rows = match RetryRow::find_by_statement(Statement::from_sql_and_values(
            store.backend(),
            store.trace_retrying(),
            [mailbox.instance_id().into(), limit.into()],
        ))
        .all(&conn)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "retry reporter: failed to read this instance's retrying traces");
                return Err(e.into());
            }
        };

        // A full page proves nothing about the traces it did not reach, so a
        // retry is only cleared when the page held every retrying trace there
        // was. Reporting a retry a little too long is harmless; clearing one
        // that is still stuck would be a lie.
        let complete_picture = rows.len() < usize::try_from(limit).unwrap_or(usize::MAX);
        let mut retrying = HashSet::with_capacity(rows.len());
        for row in rows {
            retrying.insert(row.trace.clone());
            let trace = row.trace.clone();
            mailbox
                .subscriptions()
                .publish_retry(&trace, row.into_state());
        }
        if complete_picture {
            mailbox.subscriptions().clear_retries_except(&retrying);
        }

        // A retry is a state, not a backlog: there is never "more" of it to
        // fetch, so the next look belongs to the timer.
        let reported = u64::try_from(retrying.len()).unwrap_or(u64::MAX);
        Ok(Directive::Idle(reported))
    }
}
