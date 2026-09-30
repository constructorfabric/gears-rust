// Created: 2026-09-25 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-table:p1
//! The audit trigger learns the configured retention, not only the minimum.
//!
//! The trigger refused deleting a record younger than twelve months, the
//! platform minimum, because the database knew no other number. A deployment
//! that keeps records longer — two years, say — was guarded for the first year
//! only: between the minimum and its configured horizon, a faulty code path or
//! migration could delete what the retention sweep itself would still keep.
//!
//! `settings_audit_policy` holds the configured retention in one row, which the
//! gear writes before every retention pass, and the trigger's floor becomes the
//! greater of the minimum and that row. The minimum stays the floor when the
//! row is missing, and a check keeps the row from ever going below it.
//!
//! What this guards against is still a mistake in code, not a privileged
//! database writer. `SQLite`, the test backend, gets the table so the gear
//! writes it the same way, but no trigger: its retention tests move the clock.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

// The minimum (`365` below, in the check and in `GREATEST`) is written out
// rather than taken from `crate::audit::MIN_RETENTION_DAYS`, as in the
// migration this one follows; a test pins the two together.

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            db.execute_unprepared(
                "CREATE TABLE IF NOT EXISTS settings_audit_policy (
                    id              smallint     PRIMARY KEY CHECK (id = 1),
                    retention_days  integer      NOT NULL CHECK (retention_days >= 365),
                    updated_at      timestamptz  NOT NULL
                );",
            )
            .await?;
            db.execute_unprepared(
                r"CREATE OR REPLACE FUNCTION settings_audit_reject_mutation() RETURNS trigger AS $$
                DECLARE
                    floor_days integer;
                BEGIN
                    IF TG_OP = 'UPDATE' THEN
                        RAISE EXCEPTION 'audit_records is append-only: UPDATE is not permitted';
                    END IF;
                    IF TG_OP = 'DELETE' AND OLD.retain_until IS NOT NULL AND OLD.retain_until > now() THEN
                        RAISE EXCEPTION 'audit_records: row % is held until %', OLD.id, OLD.retain_until;
                    END IF;
                    IF TG_OP = 'DELETE' AND OLD.retain_until IS NULL THEN
                        SELECT GREATEST(365, COALESCE(MAX(retention_days), 365))
                            INTO floor_days FROM settings_audit_policy;
                        IF OLD.occurred_at > now() - make_interval(days => floor_days) THEN
                            RAISE EXCEPTION 'audit_records: row % is inside the % day retention',
                                OLD.id, floor_days;
                        END IF;
                    END IF;
                    RETURN OLD;
                END;
                $$ LANGUAGE plpgsql;",
            )
            .await?;
        } else {
            db.execute_unprepared(
                "CREATE TABLE IF NOT EXISTS settings_audit_policy (
                    id              INTEGER  PRIMARY KEY CHECK (id = 1),
                    retention_days  INTEGER  NOT NULL CHECK (retention_days >= 365),
                    updated_at      TEXT     NOT NULL
                );",
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            // Back to the minimum-only floor of the migration before this one.
            db.execute_unprepared(
                r"CREATE OR REPLACE FUNCTION settings_audit_reject_mutation() RETURNS trigger AS $$
                BEGIN
                    IF TG_OP = 'UPDATE' THEN
                        RAISE EXCEPTION 'audit_records is append-only: UPDATE is not permitted';
                    END IF;
                    IF TG_OP = 'DELETE' AND OLD.retain_until IS NOT NULL AND OLD.retain_until > now() THEN
                        RAISE EXCEPTION 'audit_records: row % is held until %', OLD.id, OLD.retain_until;
                    END IF;
                    IF TG_OP = 'DELETE' AND OLD.retain_until IS NULL
                        AND OLD.occurred_at > now() - interval '365 days' THEN
                        RAISE EXCEPTION 'audit_records: row % is inside the minimum retention', OLD.id;
                    END IF;
                    RETURN OLD;
                END;
                $$ LANGUAGE plpgsql;",
            )
            .await?;
        }
        db.execute_unprepared("DROP TABLE IF EXISTS settings_audit_policy;")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260925_000002_audit_retention_policy_tests.rs"]
mod m20260925_000002_audit_retention_policy_tests;
