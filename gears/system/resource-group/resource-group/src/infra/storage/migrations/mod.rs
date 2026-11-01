// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
//! Database migrations for the resource-group gear.

use sea_orm_migration::MigratorTrait;

mod m20260306_000001_initial;
mod m20261012_000001_types_in_types_registry;

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![
            Box::new(m20260306_000001_initial::Migration),
            Box::new(m20261012_000001_types_in_types_registry::Migration),
        ]
    }
}
