//! Database migrations for the Types Registry gear.
//!
//! The initial migration creates nine P0 tables. Later migrations add
//! `coordination_state` and `operation_item.compat_forced`, drop the revision
//! tables' `content_hash`, and materialize `entity.chain_depth` with
//! `entity_gts_segment`; federation still owns `source_claim` and `routing`
//! (SPEC §9).
//!
//! Outbox tables are **not** created here. They come from
//! `toolkit_db::outbox::outbox_migrations_with_prefix("types_registry__outbox")`,
//! which the gear's `DatabaseCapability::migrations()` appends.

use sea_orm_migration::MigratorTrait;

mod m20260817_000001_initial;
mod m20260904_000002_coordination_state;
mod m20260908_000003_operation_item_compat_forced;
mod m20260924_000004_drop_revision_content_hash;
mod m20260925_000005_entity_gts_segment;

/// Migrator for the Types Registry managed-state schema.
pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![
            Box::new(m20260817_000001_initial::Migration),
            Box::new(m20260904_000002_coordination_state::Migration),
            Box::new(m20260908_000003_operation_item_compat_forced::Migration),
            Box::new(m20260924_000004_drop_revision_content_hash::Migration),
            Box::new(m20260925_000005_entity_gts_segment::Migration),
        ]
    }
}
