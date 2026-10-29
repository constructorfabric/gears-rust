//! Ordered, forward-only PostgreSQL migrations; platform DDL stays with its owners.
use sea_orm_migration::prelude::*;

struct SqlMigration {
    name: &'static str,
    sql: &'static str,
}
impl MigrationName for SqlMigration {
    fn name(&self) -> &str {
        self.name
    }
}
#[async_trait::async_trait]
impl MigrationTrait for SqlMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != sea_orm::DatabaseBackend::Postgres {
            return Err(DbErr::Migration(
                "Orders storage requires PostgreSQL 15 or later".into(),
            ));
        }
        manager
            .get_connection()
            .execute_unprepared(self.sql)
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "Orders evidence is retained: deploy a forward correction".into(),
        ))
    }
}

/// The Orders SQL migrations in apply order, after the platform migrations. A migration that is
/// on `main` is never edited: the runner records names only, so an upgraded database would
/// never re-apply it. A change is a forward migration (D-242).
const ORDERS_SQL: [(&str, &str); 11] = [
    (
        "m20261006_000001_orders_foundation",
        include_str!("01_schema.sql"),
    ),
    (
        "m20261006_000002_orders_capture",
        include_str!("02_schema.sql"),
    ),
    (
        "m20261006_000003_orders_diagnostics",
        include_str!("03_schema.sql"),
    ),
    (
        "m20261006_000004_orders_preconditions",
        include_str!("04_schema.sql"),
    ),
    ("m20261006_000005_orders_ttl", include_str!("05_schema.sql")),
    (
        "m20261006_000006_orders_access_log",
        include_str!("06_schema.sql"),
    ),
    (
        "m20261006_000007_orders_integrity",
        include_str!("07_integrity.sql"),
    ),
    (
        "m20261006_000008_orders_roles",
        include_str!("08_roles.sql"),
    ),
    (
        "m20261007_000009_orders_maintenance",
        include_str!("09_maintenance.sql"),
    ),
    (
        "m20261007_000010_orders_runtime_history",
        include_str!("10_runtime_history.sql"),
    ),
    (
        "m20261027_000011_orders_item_keys",
        include_str!("11_item_keys.sql"),
    ),
];

/// Ordered migrations: Event Broker producer registration, outbox, then the Orders SQL files.
pub fn all() -> Vec<Box<dyn MigrationTrait>> {
    let mut migrations = event_broker_sdk::producer_registration_migrations();
    migrations.extend(toolkit_db::outbox::outbox_migrations());
    for (name, sql) in ORDERS_SQL {
        migrations.push(Box::new(SqlMigration { name, sql }));
    }
    migrations
}

#[cfg(test)]
mod tests {
    /// Every Orders migration on `main` (`7c8809c67`) with the SHA-256 of its SQL there
    /// (`git show upstream-main-7c8809c67:<file> | shasum -a 256`). Add a row when a migration
    /// reaches `main`; never change one.
    const SHIPPED: [(&str, &str); 10] = [
        (
            "m20261006_000001_orders_foundation",
            "94c06cef0d4634ef22568b4e9173cd0c418b6b4de0b25fa40115f0872936156e",
        ),
        (
            "m20261006_000002_orders_capture",
            "8a4494bc7290652b758d81c2dd1d6ca60a8d15bb209b2b885394a5c69c156a26",
        ),
        (
            "m20261006_000003_orders_diagnostics",
            "e6f7fe27522079d13b026fb8808abbfa9a230b7a9f033d07e6283c845d088eea",
        ),
        (
            "m20261006_000004_orders_preconditions",
            "b9874efbdbd92cfb6bf9d0b8774f443447fdb00a71d88fb60467a399638226c1",
        ),
        (
            "m20261006_000005_orders_ttl",
            "bf8f90da4eb95e36f9416536f65a73a6250f2eb8623a14814536fa5a7abe0fee",
        ),
        (
            "m20261006_000006_orders_access_log",
            "d0904419167825a4e560940750a736c8d02465150e7a041e0679fea33932f585",
        ),
        (
            "m20261006_000007_orders_integrity",
            "9152ad5743b1f7b3552ae9a731f86f68b5e00b14f5796c50de52aaeb6f79cee9",
        ),
        (
            "m20261006_000008_orders_roles",
            "3a54bb3b23d2745537db5fd657d2f5042e2bc3bf88cc24bda554d43029073188",
        ),
        (
            "m20261007_000009_orders_maintenance",
            "173728f3fd0a1baffe30ca5fdd33613d7cbe0a04e9aa35fec16a308132eca9ac",
        ),
        (
            "m20261007_000010_orders_runtime_history",
            "94bbac89c6c054342405fe8608a429eb01e0c121aeb970ef3d83c76f5d8fcc28",
        ),
    ];

    fn sha256_hex(text: &str) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, text.as_bytes())
            .as_ref()
            .iter()
            .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xf)]])
            .map(char::from)
            .collect()
    }

    /// D-242: the migration runner records names only, so a shipped migration edited in place
    /// never re-applies on a database `main` migrated; its change must be a forward migration.
    #[test]
    fn migrations_shipped_on_main_are_unchanged() {
        for ((name, sql), (shipped, digest)) in super::ORDERS_SQL.iter().zip(SHIPPED) {
            assert_eq!(
                *name, shipped,
                "a shipped migration keeps its name and position"
            );
            assert_eq!(
                sha256_hex(sql),
                digest,
                "{name} is on main: correct it in a forward migration, never in place"
            );
        }
    }
}
