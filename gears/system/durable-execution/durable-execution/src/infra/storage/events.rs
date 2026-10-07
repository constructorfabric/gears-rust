use super::{JournalStore, StoreError, event};
use crate::domain::journal::Journal;
use crate::domain::persisted::{ActivityStatus, RunStatus};
use durable_execution_sdk::{EventKind, RunEvent, RunId};
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{DBRunner, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

pub async fn record<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    previous: Option<&Journal>,
    journal: &Journal,
) -> Result<(), StoreError> {
    let mut events = Vec::new();
    let event = |kind, activity_id, attempt, error_code| RunEvent {
        sequence: 0,
        run_id: journal.run.id,
        activity_id,
        kind,
        epoch: journal.run.execution_epoch,
        attempt,
        at: journal.run.updated_at,
        error_code,
    };
    if previous.is_some_and(|p| !p.cancellation_requested) && journal.cancellation_requested {
        events.push(event(EventKind::StopRequested, None, None, None));
    }
    if let Some(old) = previous
        && old.run.execution_epoch != journal.run.execution_epoch
    {
        events.push(event(
            if old.run.status == RunStatus::Cancelled {
                EventKind::Resumed
            } else {
                EventKind::RetryRequested
            },
            None,
            None,
            None,
        ));
    }
    if previous.is_none_or(|p| p.run.status != journal.run.status) {
        let kind = match journal.run.status {
            RunStatus::Queued => EventKind::Queued,
            RunStatus::Running => EventKind::Started,
            RunStatus::Succeeded => EventKind::Completed,
            RunStatus::Failed => EventKind::Failed,
            RunStatus::Cancelled => EventKind::Stopped,
            RunStatus::Blocked => EventKind::Blocked,
            RunStatus::RetryWait => EventKind::RetryScheduled,
            RunStatus::Cancelling => EventKind::StopRequested,
        };
        // One stop request event even when the run simultaneously becomes Cancelling.
        if kind != EventKind::StopRequested || !events.iter().any(|e| e.kind == kind) {
            events.push(event(kind, None, None, journal.run.error_code.clone()));
        }
    }
    for a in &journal.run.activities {
        let old = previous.and_then(|p| p.run.activities.iter().find(|x| x.id == a.id));
        if old.is_some_and(|p| p.status == a.status && p.attempts == a.attempts) {
            continue;
        }
        let kind = match a.status {
            ActivityStatus::Pending => EventKind::Queued,
            ActivityStatus::Running => EventKind::Started,
            ActivityStatus::Succeeded => EventKind::Completed,
            ActivityStatus::RetryWait => EventKind::RetryScheduled,
            ActivityStatus::Failed => EventKind::Failed,
            ActivityStatus::Cancelled => EventKind::Stopped,
        };
        events.push(event(
            kind,
            Some(a.id.clone()),
            Some(a.attempts),
            a.error_code.clone(),
        ));
    }
    if events.len() >= 256 {
        return Err(StoreError::Invariant("too many events in one revision"));
    }
    let base = journal
        .revision
        .checked_mul(256)
        .ok_or(StoreError::Invariant("event sequence overflow"))?;
    for (index, mut value) in events.into_iter().enumerate() {
        let sequence = base
            .checked_add(
                i64::try_from(index)
                    .map_err(|_| StoreError::Invariant("event sequence overflow"))?
                    + 1,
            )
            .ok_or(StoreError::Invariant("event sequence overflow"))?;
        value.sequence = u64::try_from(sequence)
            .map_err(|_| StoreError::Invariant("event sequence overflow"))?;
        secure_insert::<event::Entity>(
            event::ActiveModel {
                id: Set(Uuid::new_v5(
                    &journal.run.id.0,
                    format!("event:{sequence}").as_bytes(),
                )),
                tenant_id: Set(journal.run.owner.tenant_id),
                owner_id: Set(journal.run.owner.subject_id),
                run_id: Set(journal.run.id.0),
                sequence: Set(sequence),
                state: Set(serde_json::to_vec(&value)?),
            },
            scope,
            txn,
        )
        .await?;
    }
    Ok(())
}
impl JournalStore {
    pub async fn events(
        &self,
        scope: &AccessScope,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, StoreError> {
        self.events_on(&self.db.conn()?, scope, id, after, limit)
            .await
    }
    pub(crate) async fn events_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let after = i64::try_from(after).map_err(|_| {
            StoreError::Domain(crate::domain::error::DomainError::InvalidRequest {
                field: "after",
                reason: durable_execution_sdk::reason::OUT_OF_RANGE,
                message: "event cursor out of range",
            })
        })?;
        event::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(event::Column::RunId.eq(id.0))
                    .add(event::Column::Sequence.gt(after)),
            )
            .order_by(event::Column::Sequence, sea_orm::Order::Asc)
            .limit(u64::from(limit.clamp(1, 200)))
            .all(conn)
            .await?
            .into_iter()
            .map(|r| serde_json::from_slice(&r.state).map_err(StoreError::from))
            .collect()
    }
}
