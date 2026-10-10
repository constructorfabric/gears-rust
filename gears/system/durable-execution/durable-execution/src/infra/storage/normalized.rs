//! Canonical per-activity checkpoints and immutable attempt/epoch history.
//! The run revision serializes aggregate transitions; it never spans external IO.
use super::{StoreError, activity, attempt, epoch, run};
use crate::domain::persisted::{ActivityAttempt, ActivityRun};
use crate::{
    domain::journal::Journal,
    domain::parallel::{Attempt, Step},
};

use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, sea_query::Expr};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredActivity {
    activity: ActivityRun,
    // SQL timestamp columns support indexed scheduling; these retain exact
    // precision for lease fencing and comparison with the serialized run header.
    lease_until: Option<chrono::DateTime<chrono::Utc>>,
    due_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(serde::Serialize)]
struct RunMetadata<'a> {
    id: &'a durable_execution_sdk::RunId,
    definition: &'a String,
    owner: &'a durable_execution_sdk::ExecutionOwner,
    status: &'a crate::domain::persisted::RunStatus,
    created_at: &'a chrono::DateTime<chrono::Utc>,
    updated_at: &'a chrono::DateTime<chrono::Utc>,
    next_attempt_at: &'a Option<chrono::DateTime<chrono::Utc>>,
    error_code: &'a Option<String>,
    execution_epoch: &'a u64,
    stop_reason: &'a Option<String>,
    activities: [(); 0],
    previous_executions: [(); 0],
}
#[derive(serde::Serialize)]
struct JournalMetadata<'a> {
    completed_at: &'a Option<chrono::DateTime<chrono::Utc>>,
    input: &'a serde_json::Value,
    registration_generation: &'a u64,
    fingerprint: &'a String,
    revision: &'a i64,
    fence: &'a i64,
    lease_until: &'a Option<chrono::DateTime<chrono::Utc>>,
    delivery_generation: &'a i64,
    cancellation_requested: &'a bool,
    run: RunMetadata<'a>,
    parallel: Option<StageMetadata>,
}
#[derive(serde::Serialize)]
struct StageMetadata {
    steps: [(); 0],
    stop_requested: bool,
    epoch: u64,
    last_resume: Option<bool>,
}
#[derive(serde::Serialize)]
struct ActivityCheckpoint<'a> {
    id: &'a durable_execution_sdk::ActivityId,
    status: &'a crate::domain::persisted::ActivityStatus,
    attempts: &'a u32,
    budget_attempts: &'a u32,
    result: &'a Option<serde_json::Value>,
    error_code: &'a Option<String>,
    started_at: &'a Option<chrono::DateTime<chrono::Utc>>,
    finished_at: &'a Option<chrono::DateTime<chrono::Utc>>,
    attempt_history: [(); 0],
}
#[derive(serde::Serialize)]
struct StoredActivityRef<'a> {
    activity: ActivityCheckpoint<'a>,
    lease_until: Option<chrono::DateTime<chrono::Utc>>,
    due_at: Option<chrono::DateTime<chrono::Utc>>,
}
pub fn metadata(journal: &Journal) -> Result<Vec<u8>, StoreError> {
    let header = JournalMetadata {
        completed_at: &journal.completed_at,
        input: &journal.input,
        registration_generation: &journal.registration_generation,
        fingerprint: &journal.fingerprint,
        revision: &journal.revision,
        fence: &journal.fence,
        lease_until: &journal.lease_until,
        delivery_generation: &journal.delivery_generation,
        cancellation_requested: &journal.cancellation_requested,
        run: RunMetadata {
            id: &journal.run.id,
            definition: &journal.run.definition,
            owner: &journal.run.owner,
            status: &journal.run.status,
            created_at: &journal.run.created_at,
            updated_at: &journal.run.updated_at,
            next_attempt_at: &journal.run.next_attempt_at,
            error_code: &journal.run.error_code,
            execution_epoch: &journal.run.execution_epoch,
            stop_reason: &journal.run.stop_reason,
            activities: [],
            previous_executions: [],
        },
        parallel: journal.parallel.as_ref().map(|p| StageMetadata {
            steps: [],
            stop_requested: p.stop_requested,
            epoch: p.epoch,
            last_resume: p.last_resume,
        }),
    };
    Ok(serde_json::to_vec(&header)?)
}
fn key(run: Uuid, kind: &str, suffix: &str) -> Uuid {
    Uuid::new_v5(&run, format!("{kind}:{suffix}").as_bytes())
}
fn number(n: u64) -> Result<i64, StoreError> {
    i64::try_from(n).map_err(|_| StoreError::Invariant("value out of range"))
}
fn status(value: crate::domain::persisted::ActivityStatus) -> Result<String, StoreError> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .ok_or(StoreError::Invariant("activity status is not a string"))?
        .to_owned())
}

// Persist one journal revision as a single transaction across activity/history tables.
#[expect(
    clippy::cognitive_complexity,
    reason = "Persist the activity checkpoints and attempt history of one journal revision in the same transaction."
)]
pub async fn persist<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    journal: &Journal,
) -> Result<(), StoreError> {
    let rows = activity::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(activity::Column::RunId.eq(journal.run.id.0)))
        .all(txn)
        .await?;
    let attempts = attempt::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(attempt::Column::RunId.eq(journal.run.id.0)))
        .all(txn)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<std::collections::BTreeMap<_, _>>();
    let epochs = epoch::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(epoch::Column::RunId.eq(journal.run.id.0)))
        .all(txn)
        .await?;
    for (position, a) in journal.run.activities.iter().enumerate() {
        let id = key(journal.run.id.0, "activity", &a.id.0);
        let parallel = journal
            .parallel
            .as_ref()
            .and_then(|j| j.steps.get(position));
        let fence = match parallel {
            Some(p) => number(p.fence)?,
            None => match a.attempt_history.last() {
                Some(attempt) => number(attempt.fence)?,
                None if a.status == crate::domain::persisted::ActivityStatus::Running => {
                    journal.fence
                }
                None => 0,
            },
        };
        let lease = parallel.map_or_else(
            || {
                if a.status == crate::domain::persisted::ActivityStatus::Running {
                    journal.lease_until
                } else {
                    None
                }
            },
            |p| p.lease_until,
        );
        let due = parallel.map_or(journal.run.next_attempt_at, |p| Some(p.due_at));
        let state = serde_json::to_vec(&StoredActivityRef {
            activity: ActivityCheckpoint {
                id: &a.id,
                status: &a.status,
                attempts: &a.attempts,
                budget_attempts: &a.budget_attempts,
                result: &a.result,
                error_code: &a.error_code,
                started_at: &a.started_at,
                finished_at: &a.finished_at,
                attempt_history: [],
            },
            lease_until: lease,
            due_at: due,
        })?;
        let stage_index = parallel
            .map(|p| {
                i32::try_from(p.stage)
                    .map_err(|_| StoreError::Invariant("stage index out of range"))
            })
            .transpose()?;
        let epoch = number(journal.run.execution_epoch)?;
        let status = status(a.status)?;
        if let Some(old) = rows.iter().find(|r| r.id == id) {
            if old.state != state || old.fence != fence || old.epoch != epoch {
                let changed = activity::Entity::update_many()
                    .secure()
                    .scope_with(scope)
                    .filter(
                        Condition::all()
                            .add(activity::Column::Id.eq(id))
                            .add(activity::Column::Fence.eq(old.fence)),
                    )
                    .col_expr(activity::Column::State, Expr::value(state))
                    .col_expr(activity::Column::Status, Expr::value(status))
                    .col_expr(activity::Column::Epoch, Expr::value(epoch))
                    .col_expr(activity::Column::Fence, Expr::value(fence))
                    .col_expr(activity::Column::LeaseUntil, Expr::value(lease))
                    .col_expr(activity::Column::DueAt, Expr::value(due))
                    .exec(txn)
                    .await?;
                if changed.rows_affected != 1 {
                    return Err(StoreError::Conflict);
                }
            }
        } else {
            secure_insert::<activity::Entity>(
                activity::ActiveModel {
                    id: Set(id),
                    tenant_id: Set(journal.run.owner.tenant_id),
                    owner_id: Set(journal.run.owner.subject_id),
                    run_id: Set(journal.run.id.0),
                    activity_id: Set(a.id.0.clone()),
                    position: Set(i32::try_from(position)
                        .map_err(|_| StoreError::Invariant("activity position out of range"))?),
                    stage: Set(stage_index),
                    epoch: Set(epoch),
                    status: Set(status),
                    fence: Set(fence),
                    lease_until: Set(lease),
                    due_at: Set(due),
                    state: Set(state),
                },
                scope,
                txn,
            )
            .await?;
        }
        let history: Box<dyn Iterator<Item = std::borrow::Cow<'_, ActivityAttempt>> + Send + '_> =
            match parallel {
                Some(p) => Box::new(p.history.iter().map(|h| {
                    std::borrow::Cow::Owned(ActivityAttempt {
                        number: h.number,
                        epoch: h.epoch,
                        fence: h.fence,
                        started_at: h.started_at,
                        finished_at: h.finished_at,
                        status: h.status,
                        error_code: h.error.clone(),
                    })
                })),
                None => Box::new(a.attempt_history.iter().map(std::borrow::Cow::Borrowed)),
            };
        for h in history {
            let id = key(
                journal.run.id.0,
                "attempt",
                &format!("{}:{}", a.id.0, h.number),
            );
            let state = serde_json::to_vec(&h)?;
            if let Some(old) = attempts.get(&id) {
                if old.state != state {
                    let previous: ActivityAttempt = serde_json::from_slice(&old.state)?;
                    if previous.finished_at.is_some()
                        || previous.fence != h.fence
                        || previous.epoch != h.epoch
                    {
                        return Err(StoreError::Conflict);
                    }
                    attempt::Entity::update_many()
                        .secure()
                        .scope_with(scope)
                        .filter(Condition::all().add(attempt::Column::Id.eq(id)))
                        .col_expr(attempt::Column::State, Expr::value(state))
                        .exec(txn)
                        .await?;
                }
            } else {
                secure_insert::<attempt::Entity>(
                    attempt::ActiveModel {
                        id: Set(id),
                        tenant_id: Set(journal.run.owner.tenant_id),
                        owner_id: Set(journal.run.owner.subject_id),
                        run_id: Set(journal.run.id.0),
                        activity_id: Set(a.id.0.clone()),
                        number: Set(i64::from(h.number)),
                        epoch: Set(number(h.epoch)?),
                        state: Set(state),
                    },
                    scope,
                    txn,
                )
                .await?;
            }
        }
    }
    for snapshot in &journal.run.previous_executions {
        let id = key(journal.run.id.0, "epoch", &snapshot.epoch.to_string());
        let state = serde_json::to_vec(snapshot)?;
        if let Some(old) = epochs.iter().find(|e| e.id == id) {
            if old.state != state {
                return Err(StoreError::Conflict);
            }
        } else {
            secure_insert::<epoch::Entity>(
                epoch::ActiveModel {
                    id: Set(id),
                    tenant_id: Set(journal.run.owner.tenant_id),
                    owner_id: Set(journal.run.owner.subject_id),
                    run_id: Set(journal.run.id.0),
                    epoch: Set(number(snapshot.epoch)?),
                    state: Set(state),
                },
                scope,
                txn,
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn hydrate<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    row: run::Model,
) -> Result<Journal, StoreError> {
    let mut j: Journal = serde_json::from_slice(&row.journal)?;
    if i64::try_from(j.registration_generation).ok() != Some(row.registration_generation) {
        return Err(StoreError::Invariant("registration generation mismatch"));
    }
    if row.storage_version == 0 {
        return Ok(j);
    }
    if row.storage_version != 1 {
        return Err(StoreError::Invariant("unsupported storage version"));
    }
    let activities = activity::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(activity::Column::RunId.eq(row.id)))
        .order_by(activity::Column::Position, sea_orm::Order::Asc)
        .all(txn)
        .await?;
    if activities.len()
        != usize::try_from(row.activity_count)
            .map_err(|_| StoreError::Invariant("activity count out of range"))?
    {
        // Rows can be torn under READ COMMITTED; the reader retries on Conflict.
        return Err(StoreError::Conflict);
    }
    let history = attempt::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(attempt::Column::RunId.eq(row.id)))
        .order_by(attempt::Column::Number, sea_orm::Order::Asc)
        .all(txn)
        .await?;
    for a in activities {
        let stored: StoredActivity = serde_json::from_slice(&a.state)?;
        let mut state = stored.activity;
        state.attempt_history = history
            .iter()
            .filter(|h| h.activity_id == a.activity_id)
            .map(|h| serde_json::from_slice(&h.state))
            .collect::<Result<_, _>>()?;
        if let Some(p) = &mut j.parallel {
            p.steps.push(Step {
                id: a.activity_id,
                stage: u32::try_from(a.stage.ok_or(StoreError::Conflict)?)
                    .map_err(|_| StoreError::Invariant("stage index out of range"))?,
                status: state.status,
                fence: u64::try_from(a.fence)
                    .map_err(|_| StoreError::Invariant("fence out of range"))?,
                attempts: state.attempts,
                budget_attempts: state.budget_attempts,
                lease_until: stored.lease_until,
                due_at: stored.due_at.ok_or(StoreError::Conflict)?,
                result: state.result.clone(),
                error: state.error_code.clone(),
                history: state
                    .attempt_history
                    .iter()
                    .map(|h| Attempt {
                        number: h.number,
                        epoch: h.epoch,
                        fence: h.fence,
                        started_at: h.started_at,
                        finished_at: h.finished_at,
                        status: h.status,
                        error: h.error_code.clone(),
                    })
                    .collect(),
            });
        }
        j.run.activities.push(state);
    }
    j.run.previous_executions = epoch::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(epoch::Column::RunId.eq(row.id)))
        .order_by(epoch::Column::Epoch, sea_orm::Order::Asc)
        .all(txn)
        .await?
        .into_iter()
        .map(|r| serde_json::from_slice(&r.state))
        .collect::<Result<_, _>>()?;
    // READ COMMITTED may observe a newer ErasedActivity query than the header query.
    // A second revision read detects that race instead of returning mixed state.
    let current = run::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(run::Column::Id.eq(row.id)))
        .one(txn)
        .await?
        .ok_or(StoreError::Conflict)?;
    if current.revision != row.revision {
        return Err(StoreError::Conflict);
    }
    Ok(j)
}
