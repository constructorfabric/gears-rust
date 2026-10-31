use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES: &str = include_str!("m0001_initial/postgres.sql");
const MYSQL: &str = include_str!("m0001_initial/mysql.sql");
const SQLITE: &str = include_str!("m0001_initial/sqlite.sql");

const TABLES: [&str; 33] = [
    "gm_active_syncs",
    "gm_branches",
    "gm_check_runs",
    "gm_comments",
    "gm_commit_comments",
    "gm_commit_files",
    "gm_commit_statuses",
    "gm_commits",
    "gm_contributors",
    "gm_deployments",
    "gm_entity_fingerprints",
    "gm_http_cache",
    "gm_issue_events",
    "gm_issue_reactions",
    "gm_issue_timeline",
    "gm_issues",
    "gm_labels",
    "gm_logical_conversations",
    "gm_milestones",
    "gm_pull_request_commits",
    "gm_pull_request_files",
    "gm_pull_requests",
    "gm_releases",
    "gm_repo_sync_status",
    "gm_repositories",
    "gm_review_comments",
    "gm_review_threads",
    "gm_reviews",
    "gm_sync_sessions",
    "gm_sync_watermarks",
    "gm_tags",
    "gm_workflow_jobs",
    "gm_workflow_runs",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::run_schema(manager, POSTGRES, MYSQL, SQLITE).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::drop_tables(manager, &TABLES).await
    }
}
