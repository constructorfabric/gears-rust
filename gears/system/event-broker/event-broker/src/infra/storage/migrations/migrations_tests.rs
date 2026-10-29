use sea_orm_migration::MigratorTrait;

use super::Migrator;

/// The `default` outbox group's tables come from the declared migrations
/// under its fixed prefix, never from toolkit-db's unprefixed default.
#[test]
fn the_outbox_tables_are_declared_under_the_default_groups_prefix() {
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "m20260819_000001_spec_cache",
            "m20260819_000002_storage_facade",
            "m20260819_000003_backend_binding",
            "m001_create_toolkit_outbox_schema__event_broker__ob_default",
        ]
    );
}
