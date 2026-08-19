use async_trait::async_trait;
use github_mirror_sdk::Repository;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::error::DomainError;

/// Write-side record for a mirrored repository (what sync knows about it).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRecord {
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub default_branch: String,
    pub private: bool,
    pub pushed_at: Option<String>,
    pub stars: i64,
    pub forks: i64,
    pub description: Option<String>,
}

#[async_trait]
pub trait GithubRepoRepository: Send + Sync {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: RepositoryRecord,
    ) -> Result<Repository, DomainError>;

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        limit: u64,
    ) -> Result<Vec<Repository>, DomainError>;
}
