use async_trait::async_trait;
use github_mirror_sdk::Repository;
use sea_orm::{ActiveValue, EntityTrait, Order};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::{GithubRepoRepository, RepositoryRecord};

use super::entity::repositories::{self, Entity as GithubRepoEntity};

pub struct SeaOrmGithubRepoRepository;

impl SeaOrmGithubRepoRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmGithubRepoRepository {
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
impl GithubRepoRepository for SeaOrmGithubRepoRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: RepositoryRecord,
    ) -> Result<Repository, DomainError> {
        let on_conflict = SecureOnConflict::<GithubRepoEntity>::columns([
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

        GithubRepoEntity::insert(active_model(tenant_id, &record))
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
        let rows = GithubRepoEntity::find()
            .secure()
            .scope_with(scope)
            .order_by(repositories::Column::FullName, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}
