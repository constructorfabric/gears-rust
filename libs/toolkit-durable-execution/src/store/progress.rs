//! Payload-free reads from the projection committed with each journal revision.
use super::{JournalStore, StoreError, run};
use crate::{
    RunId,
    observation::{RunProgress, WorkflowPage, WorkflowQuery},
};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QuerySelect};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_security::AccessScope;
#[derive(sea_orm::FromQueryResult)]
struct Projection {
    progress: Vec<u8>,
}
impl JournalStore {
    /// Reads one run's committed progress projection; `None` when absent or out of scope.
    pub async fn get_progress(
        &self,
        scope: &AccessScope,
        id: RunId,
    ) -> Result<Option<RunProgress>, StoreError> {
        self.get_progress_on(&self.db.conn()?, scope, id).await
    }
    /// Reads one run's progress projection on `conn`; `None` when absent or out of scope.
    pub(crate) async fn get_progress_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: RunId,
    ) -> Result<Option<RunProgress>, StoreError> {
        run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(run::Column::Id.eq(id.0)))
            .limit(1)
            .project_all(conn, |q| {
                q.select_only()
                    .column(run::Column::Progress)
                    .into_model::<Projection>()
            })
            .await?
            .pop()
            .map(|row| decode(&row.progress))
            .transpose()
    }
    /// Lists scoped run progress created since `query.since`, newest first, with a total count.
    pub async fn list_progress(
        &self,
        scope: &AccessScope,
        query: &WorkflowQuery,
    ) -> Result<WorkflowPage, StoreError> {
        self.list_progress_on(&self.db.conn()?, scope, query).await
    }
    /// Lists progress projections on `conn`, newest first; `limit` is clamped to 1..=100.
    pub(crate) async fn list_progress_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &WorkflowQuery,
    ) -> Result<WorkflowPage, StoreError> {
        let mut filter = Condition::all().add(run::Column::CreatedAt.gte(query.since));
        if let Some(status) = query.status {
            let value = serde_json::to_value(status)?
                .as_str()
                .ok_or(StoreError::Invariant("invalid progress status"))?
                .to_owned();
            filter = filter.add(run::Column::ProgressStatus.eq(value));
        }
        let total = run::Entity::find()
            .select_only()
            .column(run::Column::Id)
            .secure()
            .scope_with(scope)
            .filter(filter.clone())
            .count(conn)
            .await?;
        let rows = run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(filter)
            .order_by(run::Column::CreatedAt, sea_orm::Order::Desc)
            .order_by(run::Column::Id, sea_orm::Order::Desc)
            .offset(u64::from(query.offset))
            .limit(u64::from(query.limit.clamp(1, 100)))
            .project_all(conn, |q| {
                q.select_only()
                    .column(run::Column::Progress)
                    .into_model::<Projection>()
            })
            .await?;
        let items = rows
            .into_iter()
            .map(|row| decode(&row.progress))
            .collect::<Result<_, _>>()?;
        Ok(WorkflowPage { items, total })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProgressRecord {
    version: u8,
    progress: RunProgress,
}
pub(super) fn encode(progress: &RunProgress) -> Result<Vec<u8>, StoreError> {
    Ok(serde_json::to_vec(&ProgressRecord {
        version: 1,
        progress: progress.clone(),
    })?)
}
fn decode(bytes: &[u8]) -> Result<RunProgress, StoreError> {
    let record: ProgressRecord = serde_json::from_slice(bytes)?;
    if record.version != 1 {
        return Err(StoreError::Invariant("unsupported progress format"));
    }
    Ok(record.progress)
}

#[derive(sea_orm::FromQueryResult)]
pub struct ResultIdentity {
    pub tenant_id: uuid::Uuid,
    pub owner_id: uuid::Uuid,
    pub definition: String,
    pub fingerprint: String,
}
impl JournalStore {
    /// Owner and workflow identity of run `id`, used to check a result read
    /// before decoding the stored value.
    pub async fn result_identity(
        &self,
        scope: &AccessScope,
        id: RunId,
    ) -> Result<Option<ResultIdentity>, StoreError> {
        Ok(run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(run::Column::Id.eq(id.0)))
            .limit(1)
            .project_all(&self.db.conn()?, |q| {
                q.select_only()
                    .column(run::Column::TenantId)
                    .column(run::Column::OwnerId)
                    .column(run::Column::Definition)
                    .column(run::Column::Fingerprint)
                    .into_model::<ResultIdentity>()
            })
            .await?
            .pop())
    }
}
