// Created: 2026-09-24 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-table:p1
//! `audit_records` refuses deleting a record younger than the platform
//! minimum retention.
//!
//! The first append-only trigger refused a `DELETE` only under an explicit
//! `retain_until` hold, and no production path sets one: every real record
//! could be deleted the moment it was written, by a faulty code path or
//! migration. This replaces the trigger function so a `DELETE` is also
//! refused while the record is younger than twelve months — the platform's
//! minimum retention, below which the configuration cannot go (`init` refuses
//! it), so the database needs no knowledge of the configured value. Between
//! that floor and a longer configured retention, the retention sweep is the
//! only deleting path.
//!
//! What this guards against is a mistake in code, not a privileged database
//! writer: whoever may drop the trigger may delete. That is for database
//! roles, which provisioning owns, and for the off-box copy R2 ships.
//! `SQLite`, the test backend, keeps the `UPDATE` guard only.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

// The floor (`interval '365 days'` below) is written out rather than taken
// from `crate::audit::MIN_RETENTION_DAYS`: a migration is a record of what was
// applied, and a change to the minimum takes a new migration. A test pins the
// two together.

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }
        manager
            .get_connection()
            .execute_unprepared(
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
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }
        manager
            .get_connection()
            .execute_unprepared(
                r"CREATE OR REPLACE FUNCTION settings_audit_reject_mutation() RETURNS trigger AS $$
                BEGIN
                    IF TG_OP = 'UPDATE' THEN
                        RAISE EXCEPTION 'audit_records is append-only: UPDATE is not permitted';
                    END IF;
                    IF TG_OP = 'DELETE' AND OLD.retain_until IS NOT NULL AND OLD.retain_until > now() THEN
                        RAISE EXCEPTION 'audit_records: row % is held until %', OLD.id, OLD.retain_until;
                    END IF;
                    RETURN OLD;
                END;
                $$ LANGUAGE plpgsql;",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260924_000002_audit_records_retention_floor_tests.rs"]
mod tests;
