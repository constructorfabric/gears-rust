//! The element audit envelope (`cpt-cf-graph-storage-fr-audit-envelope`).
//!
//! An element on the API is two documents joined: the producer-authored body
//! its GTS type describes, and a gear-assigned envelope the API schema
//! describes. This migration gives the envelope its storage.
//!
//! Three things change against `m0001`:
//!
//! - `node.created_by` was a single `TEXT` column, written empty on every
//!   path. It becomes a **subject pair** — `subject_id` plus optional
//!   `subject_type` — because the acting party is usually an automation or a
//!   service integration rather than a person, and the platform already has a
//!   vocabulary for that (`SecurityContext`).
//! - The same pair is added for the two other verbs, update and delete. A
//!   row records the *last* writer of each, not a chain of them: history is
//!   reconstructed from emitted change events and `ingest_audit`, never from
//!   a column here.
//! - `edge` gains `updated_at` and all three pairs. A re-synced static edge is
//!   rewritten rather than versioned, so `updated_by` answers "which producer
//!   last asserted this relationship" — the question that arises the moment
//!   two producers claim the same one.
//!
//! **One divergence from DESIGN § 3.7,** which types the subject id `TEXT`:
//! `SecurityContext::subject_id` is a `Uuid`, so `UUID` is what the value
//! actually is and `TEXT` would widen it for nothing. `subject_type` stays
//! `TEXT` — it is a GTS identifier. Recorded in the gear's development notes.
//!
//! Backfill: existing rows are stamped with the nil UUID, which is the same
//! "no subject recorded" the empty `created_by` meant. Nothing in the gear
//! reads a subject to make a decision — the envelope is reported, never
//! enforced on — so a nil subject is a missing answer, not a wrong one.

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
ALTER TABLE node
    DROP COLUMN IF EXISTS created_by,
    ADD COLUMN IF NOT EXISTS created_by_subject_id   UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    ADD COLUMN IF NOT EXISTS created_by_subject_type TEXT,
    ADD COLUMN IF NOT EXISTS updated_by_subject_id   UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    ADD COLUMN IF NOT EXISTS updated_by_subject_type TEXT,
    ADD COLUMN IF NOT EXISTS deleted_by_subject_id   UUID,
    ADD COLUMN IF NOT EXISTS deleted_by_subject_type TEXT;

ALTER TABLE edge
    ADD COLUMN IF NOT EXISTS updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN IF NOT EXISTS created_by_subject_id   UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    ADD COLUMN IF NOT EXISTS created_by_subject_type TEXT,
    ADD COLUMN IF NOT EXISTS updated_by_subject_id   UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    ADD COLUMN IF NOT EXISTS updated_by_subject_type TEXT,
    ADD COLUMN IF NOT EXISTS deleted_by_subject_id   UUID,
    ADD COLUMN IF NOT EXISTS deleted_by_subject_type TEXT;
                ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r"
ALTER TABLE node
    DROP COLUMN IF EXISTS created_by_subject_id,
    DROP COLUMN IF EXISTS created_by_subject_type,
    DROP COLUMN IF EXISTS updated_by_subject_id,
    DROP COLUMN IF EXISTS updated_by_subject_type,
    DROP COLUMN IF EXISTS deleted_by_subject_id,
    DROP COLUMN IF EXISTS deleted_by_subject_type,
    ADD COLUMN IF NOT EXISTS created_by TEXT NOT NULL DEFAULT '';

ALTER TABLE edge
    DROP COLUMN IF EXISTS updated_at,
    DROP COLUMN IF EXISTS created_by_subject_id,
    DROP COLUMN IF EXISTS created_by_subject_type,
    DROP COLUMN IF EXISTS updated_by_subject_id,
    DROP COLUMN IF EXISTS updated_by_subject_type,
    DROP COLUMN IF EXISTS deleted_by_subject_id,
    DROP COLUMN IF EXISTS deleted_by_subject_type;
                ",
            )
            .await?;
        Ok(())
    }
}
