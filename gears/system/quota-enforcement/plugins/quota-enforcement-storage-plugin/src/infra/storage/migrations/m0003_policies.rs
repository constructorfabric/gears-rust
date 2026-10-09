//! Versioned platform policies. Header locks serialize pointer changes; the
//! partial unique index permits scope reuse after deletion without losing history.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const STATEMENTS: &[&str] = &[
    "CREATE TABLE qe_policies (id TEXT PRIMARY KEY NOT NULL, scope_key TEXT NOT NULL, active_version BIGINT NULL CHECK(active_version IS NULL OR active_version >= 1), high_water BIGINT NOT NULL CHECK(high_water >= 1), CHECK(active_version IS NULL OR active_version <= high_water));",
    "CREATE UNIQUE INDEX idx_qe_policies_live_scope ON qe_policies(scope_key) WHERE active_version IS NOT NULL;",
    "CREATE TABLE qe_policy_versions (policy_id TEXT NOT NULL REFERENCES qe_policies(id) ON DELETE RESTRICT, version BIGINT NOT NULL CHECK(version >= 1), state TEXT NOT NULL CHECK(state IN ('active','superseded','rolled_back','deleted')), payload TEXT NOT NULL, PRIMARY KEY(policy_id,version));",
    "CREATE UNIQUE INDEX idx_qe_policy_one_active ON qe_policy_versions(policy_id) WHERE state = 'active';",
    "CREATE TABLE qe_policy_operation_log (id TEXT PRIMARY KEY NOT NULL, policy_id TEXT NOT NULL REFERENCES qe_policies(id) ON DELETE RESTRICT, version BIGINT NOT NULL, operation TEXT NOT NULL, actor TEXT NOT NULL, comment TEXT NULL, occurred_at TEXT NOT NULL);",
    // The log is written once per transition and read back per policy in
    // transition order, so the audit read does not degrade into a table scan
    // as retained history accumulates.
    "CREATE INDEX idx_qe_policy_log_policy ON qe_policy_operation_log(policy_id, occurred_at);",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if matches!(
            manager.get_database_backend(),
            sea_orm::DatabaseBackend::MySql
        ) {
            return Err(DbErr::Custom("QE supports PostgreSQL and SQLite".into()));
        }
        for statement in STATEMENTS {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "qe_policy_operation_log",
            "qe_policy_versions",
            "qe_policies",
        ] {
            manager
                .get_connection()
                .execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
                .await?;
        }
        Ok(())
    }
}
