use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

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
        let schema = match manager.get_database_backend() {
            DatabaseBackend::Postgres => POSTGRES,
            DatabaseBackend::MySql => MYSQL,
            DatabaseBackend::Sqlite => SQLITE,
            other => {
                return Err(DbErr::Custom(format!(
                    "migration has no DDL for database backend {other:?}"
                )));
            }
        };
        let conn = manager.get_connection();
        for statement in schema
            .split(';')
            .map(str::trim)
            .filter(|statement| !statement.is_empty())
        {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for table in TABLES {
            conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
                .await?;
        }
        Ok(())
    }
}
