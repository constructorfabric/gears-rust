//! `SeaORM` migrations for the `credstore` module.
//!
//! * `m0001_initial_schema` — the full `credstore_secrets` table: lifecycle
//!   statuses (1=provisioning, 2=active, 3=deprovisioning), the monotonic
//!   `version` column, GTS secret typing (`secret_type`, `expires_at`), and
//!   all indexes. The stateful gear, the deprovisioning saga, and secret
//!   types shipped together, so the gear starts from one consolidated schema.

use sea_orm_migration::prelude::*;

pub mod m0001_initial_schema;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m0001_initial_schema::Migration)]
    }
}
