// Updated: 2026-10-06 by Constructor Tech
//! `SeaORM` migrations for the `credstore` module.
//!
//! * `m0001_initial_schema` — the full `credstore_secrets` table: lifecycle
//!   statuses (1=provisioning, 2=active, 3=deprovisioning), the monotonic
//!   `version` column, GTS secret typing (`secret_type`, `expires_at`), and
//!   all indexes. The stateful gear, the deprovisioning saga, and secret
//!   types shipped together, so the gear starts from one consolidated schema.
//! * `m0002_value_versions` - ADR-0006 immutable value versions: the
//!   `value_version` pointer and `fallback` column on `credstore_secrets`,
//!   the narrowed two-status `CHECK`, the pointer/status `CHECK`, the
//!   removal of the fence columns and the reaper index, the write-intent
//!   journal `credstore_write_intents` and the store cleanup debts
//!   `credstore_store_cleanup`. No gc table, no outbox. Irreversible: its `down`
//!   always returns an error (it deletes saga rows, drops the value
//!   fingerprints and demotes rows to `declared`); roll back by restoring the
//!   pre-migration database and store snapshots (credstore DESIGN section 8).

use sea_orm_migration::prelude::*;

pub mod m0001_initial_schema;
pub mod m0002_value_versions;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_initial_schema::Migration),
            Box::new(m0002_value_versions::Migration),
        ]
    }
}
