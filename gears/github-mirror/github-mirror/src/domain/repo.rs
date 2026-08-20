use async_trait::async_trait;
use github_mirror_sdk::{Issue, Repository};
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
pub trait RepoRepository: Send + Sync {
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

    async fn find_by_full_name<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        full_name: &str,
    ) -> Result<Option<Repository>, DomainError>;
}

/// Write-side record for a mirrored issue (pull requests included).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRecord {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub is_pull_request: bool,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub html_url: Option<String>,
}

#[async_trait]
pub trait IssueRepository: Send + Sync {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: IssueRecord,
    ) -> Result<Issue, DomainError>;

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<Issue>, DomainError>;
}
