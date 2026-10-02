//! The source-namespace ownership registry
//! (`cpt-cf-graph-storage-fr-source-ownership`).
//!
//! A reference node's identity is the triple `(source, kind, native id)`, and
//! that triple is what makes two producers converge on the same upstream
//! object (ADR-0002). It is also what would let a producer holding a generic
//! `write` permission submit *another* source's triple and overwrite the
//! projection its owner maintains — `source` inside a validly typed payload
//! proves nothing about who may speak for it.
//!
//! This table is the authority the ingest path consults. `node.owner_principal`
//! stays the immutable record of who created a row; this row says who may
//! write the namespace now, which is what makes an ownership transfer a change
//! of one row rather than a rewrite of history.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r"
CREATE TABLE IF NOT EXISTS source_namespace_owner (
    tenant_id             UUID        NOT NULL,
    namespace             TEXT        NOT NULL,
    owner_principal       TEXT        NOT NULL,
    claimed_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    previous_owner        TEXT,
    transferred_at        TIMESTAMPTZ,
    transferred_by_subject_id   UUID,
    transferred_by_subject_type  TEXT,
    PRIMARY KEY (tenant_id, namespace)
);
                ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS source_namespace_owner;")
            .await?;
        Ok(())
    }
}
