use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend, Statement, Value};

#[derive(DeriveMigrationName)]
pub struct Migration;

const UUID_COLUMNS: [(&str, &str); 36] = [
    ("gm_repositories", "tenant_id"),
    ("gm_issues", "tenant_id"),
    ("gm_pull_requests", "tenant_id"),
    ("gm_commits", "tenant_id"),
    ("gm_comments", "tenant_id"),
    ("gm_review_comments", "tenant_id"),
    ("gm_reviews", "tenant_id"),
    ("gm_labels", "tenant_id"),
    ("gm_milestones", "tenant_id"),
    ("gm_releases", "tenant_id"),
    ("gm_branches", "tenant_id"),
    ("gm_contributors", "tenant_id"),
    ("gm_workflow_runs", "tenant_id"),
    ("gm_pull_request_files", "tenant_id"),
    ("gm_tags", "tenant_id"),
    ("gm_commit_files", "tenant_id"),
    ("gm_review_threads", "tenant_id"),
    ("gm_commit_comments", "tenant_id"),
    ("gm_issue_events", "tenant_id"),
    ("gm_deployments", "tenant_id"),
    ("gm_pull_request_commits", "tenant_id"),
    ("gm_commit_statuses", "tenant_id"),
    ("gm_workflow_jobs", "tenant_id"),
    ("gm_issue_reactions", "tenant_id"),
    ("gm_check_runs", "tenant_id"),
    ("gm_issue_timeline", "tenant_id"),
    ("gm_sync_sessions", "tenant_id"),
    ("gm_sync_sessions", "id"),
    ("gm_active_syncs", "tenant_id"),
    ("gm_active_syncs", "session_id"),
    ("gm_active_syncs", "owner_id"),
    ("gm_sync_watermarks", "tenant_id"),
    ("gm_entity_fingerprints", "tenant_id"),
    ("gm_repo_sync_status", "tenant_id"),
    ("gm_repo_sync_status", "last_session_id"),
    ("gm_http_cache", "tenant_id"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::MySql {
            return Ok(());
        }
        let conn = manager.get_connection();
        for (table, column) in UUID_COLUMNS {
            let found = conn
                .query_one_raw(Statement::from_sql_and_values(
                    DatabaseBackend::MySql,
                    "SELECT DATA_TYPE, IS_NULLABLE FROM information_schema.COLUMNS \
                     WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND COLUMN_NAME = ?",
                    [Value::from(table), Value::from(column)],
                ))
                .await?;
            let Some(found) = found else {
                continue;
            };
            let data_type: String = found.try_get("", "DATA_TYPE")?;
            if !data_type.eq_ignore_ascii_case("varchar") {
                continue;
            }
            let holds_rows = conn
                .query_one_raw(Statement::from_string(
                    DatabaseBackend::MySql,
                    format!("SELECT 1 FROM {table} LIMIT 1"),
                ))
                .await?
                .is_some();
            if holds_rows {
                return Err(DbErr::Custom(format!(
                    "{table}.{column} is still VARCHAR(36) and {table} holds rows: this MariaDB \
                     database was created by a build without MariaDB support; drop the gm_* \
                     tables and run the migrations again"
                )));
            }
            let nullable: String = found.try_get("", "IS_NULLABLE")?;
            let null = if nullable.eq_ignore_ascii_case("YES") {
                "NULL"
            } else {
                "NOT NULL"
            };
            conn.execute_unprepared(&format!(
                "ALTER TABLE {table} MODIFY {column} BINARY(16) {null};"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
