use async_trait::async_trait;
use github_mirror_sdk::{Issue, Repository};
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, Order};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::{IssueRecord, IssueRepository, RepoRepository, RepositoryRecord};

use super::entity::issues::{self, Entity as IssueEntity};
use super::entity::repositories::{self, Entity as RepoEntity};

pub struct SeaOrmRepoRepository;

impl SeaOrmRepoRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmRepoRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::forbidden(msg),
        ScopeError::Invalid(msg) => DomainError::internal(format!("scope invalid: {msg}")),
        ScopeError::Db(e) => DomainError::internal(format!("database error: {e}")),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::forbidden(format!("tenant {tenant_id} not in scope"))
        }
    }
}

fn active_model(tenant_id: Uuid, r: &RepositoryRecord) -> repositories::ActiveModel {
    repositories::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        owner: ActiveValue::Set(r.owner.clone()),
        name: ActiveValue::Set(r.name.clone()),
        full_name: ActiveValue::Set(r.full_name.clone()),
        default_branch: ActiveValue::Set(r.default_branch.clone()),
        private: ActiveValue::Set(r.private),
        pushed_at: ActiveValue::Set(r.pushed_at.clone()),
        stars: ActiveValue::Set(r.stars),
        forks: ActiveValue::Set(r.forks),
        description: ActiveValue::Set(r.description.clone()),
    }
}

#[async_trait]
impl RepoRepository for SeaOrmRepoRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: RepositoryRecord,
    ) -> Result<Repository, DomainError> {
        let on_conflict = SecureOnConflict::<RepoEntity>::columns([
            repositories::Column::TenantId,
            repositories::Column::Id,
        ])
        .update_columns([
            repositories::Column::Owner,
            repositories::Column::Name,
            repositories::Column::FullName,
            repositories::Column::DefaultBranch,
            repositories::Column::Private,
            repositories::Column::PushedAt,
            repositories::Column::Stars,
            repositories::Column::Forks,
            repositories::Column::Description,
        ])
        .map_err(map_scope_error)?;

        RepoEntity::insert(active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Repository {
            id: record.id,
            owner: record.owner,
            name: record.name,
            full_name: record.full_name,
            private: record.private,
            description: record.description,
        })
    }

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        limit: u64,
    ) -> Result<Vec<Repository>, DomainError> {
        let rows = RepoEntity::find()
            .secure()
            .scope_with(scope)
            .order_by(repositories::Column::FullName, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn find_by_full_name<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        full_name: &str,
    ) -> Result<Option<Repository>, DomainError> {
        let row = RepoEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(repositories::Column::FullName.eq(full_name)))
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(row.map(Into::into))
    }
}

pub struct SeaOrmIssueRepository;

impl SeaOrmIssueRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmIssueRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn issue_active_model(tenant_id: Uuid, r: &IssueRecord) -> issues::ActiveModel {
    issues::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        number: ActiveValue::Set(r.number),
        title: ActiveValue::Set(r.title.clone()),
        body: ActiveValue::Set(r.body.clone()),
        state: ActiveValue::Set(r.state.clone()),
        is_pull_request: ActiveValue::Set(r.is_pull_request),
        created_at: ActiveValue::Set(r.created_at.clone()),
        updated_at: ActiveValue::Set(r.updated_at.clone()),
        closed_at: ActiveValue::Set(r.closed_at.clone()),
        html_url: ActiveValue::Set(r.html_url.clone()),
    }
}

#[async_trait]
impl IssueRepository for SeaOrmIssueRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: IssueRecord,
    ) -> Result<Issue, DomainError> {
        let on_conflict = SecureOnConflict::<IssueEntity>::columns([
            issues::Column::TenantId,
            issues::Column::Id,
        ])
        .update_columns([
            issues::Column::RepoId,
            issues::Column::Number,
            issues::Column::Title,
            issues::Column::Body,
            issues::Column::State,
            issues::Column::IsPullRequest,
            issues::Column::CreatedAt,
            issues::Column::UpdatedAt,
            issues::Column::ClosedAt,
            issues::Column::HtmlUrl,
        ])
        .map_err(map_scope_error)?;

        IssueEntity::insert(issue_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &issue_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Issue {
            id: record.id,
            repo_id: record.repo_id,
            number: record.number,
            title: record.title,
            body: record.body,
            state: record.state,
            is_pull_request: record.is_pull_request,
            created_at: record.created_at,
            updated_at: record.updated_at,
            closed_at: record.closed_at,
            html_url: record.html_url,
        })
    }

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<Issue>, DomainError> {
        let rows = IssueEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(issues::Column::RepoId.eq(repo_id)))
            .order_by(issues::Column::Number, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}
