//! Fresh, scoped claim monitoring without loading checkpoint or execution history.
use super::{JournalStore, StoreError, activity, run};
use crate::domain::journal::{Claim, owns_activity};
use crate::domain::persisted::{ActivityStatus, RunStatus};
use chrono::{DateTime, Utc};
use durable_execution_sdk::RunId;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QuerySelect};
use serde::Deserialize;
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_security::AccessScope;

pub struct ClaimProbe {
    pub definition: String,
    pub registration_generation: u64,
    pub cancellation_requested: bool,
    pub lease_until: Option<DateTime<Utc>>,
    step: usize,
    fence: Option<i64>,
    status: ActivityStatus,
    eligible: bool,
}
impl ClaimProbe {
    pub(crate) fn owns(&self, claim: Claim, now: DateTime<Utc>) -> bool {
        self.eligible
            && claim.step == self.step
            && self.fence.is_some_and(|fence| {
                owns_activity(claim.fence, fence, self.status, self.lease_until, now)
            })
    }
}

#[derive(sea_orm::FromQueryResult)]
struct Header {
    revision: i64,
    registration_generation: i64,
    storage_version: i32,
    activity_count: i32,
    journal: Vec<u8>,
}
#[derive(sea_orm::FromQueryResult)]
struct Revision {
    revision: i64,
}
#[derive(sea_orm::FromQueryResult)]
struct ActivityRow {
    fence: i64,
    state: Vec<u8>,
}
#[derive(Deserialize)]
struct Control {
    run: RunControl,
    #[serde(default)]
    registration_generation: u64,
    cancellation_requested: bool,
    fence: i64,
    lease_until: Option<DateTime<Utc>>,
    parallel: Option<ParallelControl>,
}
#[derive(Deserialize)]
struct RunControl {
    definition: String,
    status: RunStatus,
    activities: Vec<ActivityControl>,
}
#[derive(Deserialize)]
struct ActivityControl {
    status: ActivityStatus,
}
#[derive(Deserialize)]
struct ParallelControl {
    steps: Vec<ParallelStep>,
}
#[derive(Deserialize)]
struct ParallelStep {
    status: ActivityStatus,
    fence: u64,
    lease_until: Option<DateTime<Utc>>,
}
#[derive(Deserialize)]
struct StoredActivityControl {
    activity: ActivityControl,
    lease_until: Option<DateTime<Utc>>,
}

impl JournalStore {
    pub(crate) async fn claim_probe(
        &self,
        scope: &AccessScope,
        id: RunId,
        claim: Claim,
    ) -> Result<Option<ClaimProbe>, StoreError> {
        let conn = self.db.conn()?;
        for _ in 0..8 {
            let Some(header) = read_header(&conn, scope, id).await? else {
                return Ok(None);
            };
            match read_claim(&conn, scope, id, claim, header).await {
                Err(StoreError::Conflict) => tokio::task::yield_now().await,
                result => return result.map(Some),
            }
        }
        Err(StoreError::Conflict)
    }
}

async fn read_header<C: DBRunner>(
    conn: &C,
    scope: &AccessScope,
    id: RunId,
) -> Result<Option<Header>, StoreError> {
    Ok(run::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(run::Column::Id.eq(id.0)))
        .limit(1)
        .project_all(conn, |query| {
            query
                .select_only()
                .column(run::Column::Revision)
                .column(run::Column::RegistrationGeneration)
                .column(run::Column::StorageVersion)
                .column(run::Column::ActivityCount)
                .column(run::Column::Journal)
                .into_model::<Header>()
        })
        .await?
        .pop())
}

async fn read_claim<C: DBRunner>(
    conn: &C,
    scope: &AccessScope,
    id: RunId,
    claim: Claim,
    header: Header,
) -> Result<ClaimProbe, StoreError> {
    // Unknown fields (input, results and old histories) are skipped by serde.
    let control: Control = serde_json::from_slice(&header.journal)?;
    if i64::try_from(control.registration_generation).ok() != Some(header.registration_generation) {
        return Err(StoreError::Invariant("registration generation mismatch"));
    }
    let parallel = control.parallel.is_some();
    let mut probe = ClaimProbe {
        definition: control.run.definition,
        registration_generation: control.registration_generation,
        cancellation_requested: control.cancellation_requested,
        lease_until: None,
        step: claim.step,
        fence: None,
        status: ActivityStatus::Pending,
        eligible: parallel
            || matches!(
                control.run.status,
                RunStatus::Running | RunStatus::Cancelling
            ),
    };
    match header.storage_version {
        0 => {
            if let Some(parallel) = control.parallel {
                if let Some(step) = parallel.steps.get(claim.step) {
                    probe.fence = i64::try_from(step.fence).ok();
                    probe.status = step.status;
                    probe.lease_until = step.lease_until;
                }
            } else if let Some(activity) = control.run.activities.get(claim.step) {
                probe.fence = Some(control.fence);
                probe.status = activity.status;
                probe.lease_until = control.lease_until;
            }
        }
        1 => {
            let count = usize::try_from(header.activity_count)
                .map_err(|_| StoreError::Invariant("activity count out of range"))?;
            if claim.step < count {
                let position = i32::try_from(claim.step)
                    .map_err(|_| StoreError::Invariant("activity position out of range"))?;
                let row = activity::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .filter(
                        Condition::all()
                            .add(activity::Column::RunId.eq(id.0))
                            .add(activity::Column::Position.eq(position)),
                    )
                    .limit(1)
                    .project_all(conn, |query| {
                        query
                            .select_only()
                            .column(activity::Column::Fence)
                            .column(activity::Column::State)
                            .into_model::<ActivityRow>()
                    })
                    .await?
                    .pop()
                    .ok_or(StoreError::Conflict)?;
                let stored: StoredActivityControl = serde_json::from_slice(&row.state)?;
                probe.status = stored.activity.status;
                if parallel {
                    probe.fence = Some(row.fence);
                    probe.lease_until = stored.lease_until;
                } else {
                    // The sequential aggregate fence is authoritative, including
                    // ownership revocation before another attempt is recorded.
                    probe.fence = Some(control.fence);
                    probe.lease_until = control.lease_until;
                }
            }
        }
        _ => return Err(StoreError::Invariant("unsupported storage version")),
    }
    let current = run::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(run::Column::Id.eq(id.0)))
        .limit(1)
        .project_all(conn, |query| {
            query
                .select_only()
                .column(run::Column::Revision)
                .into_model::<Revision>()
        })
        .await?
        .pop()
        .ok_or(StoreError::Conflict)?;
    if current.revision != header.revision {
        return Err(StoreError::Conflict);
    }
    Ok(probe)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../tests/unit/probe_tests.rs"]
mod tests;
