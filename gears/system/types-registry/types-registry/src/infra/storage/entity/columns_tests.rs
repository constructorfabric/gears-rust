//! Anti-drift on the one string an entity cannot get from the compiler: the name
//! of the table it binds to.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{
    dependency, entity, instance, instance_revision, operation, operation_item, type_schema,
    type_schema_revision, version_family,
};

/// The table name each entity binds to. A typo is a runtime "no such table" that
/// only shows up on the first query, and the migration is the only other place
/// these strings appear.
#[test]
fn every_core_entity_binds_to_its_table_in_the_migration() {
    use sea_orm::EntityName;

    for (got, expected) in [
        (
            version_family::Entity.table_name(),
            "types_registry__version_family",
        ),
        (entity::Entity.table_name(), "types_registry__entity"),
        (
            type_schema_revision::Entity.table_name(),
            "types_registry__type_schema_revision",
        ),
        (
            type_schema::Entity.table_name(),
            "types_registry__type_schema",
        ),
        (operation::Entity.table_name(), "types_registry__operation"),
        (
            operation_item::Entity.table_name(),
            "types_registry__operation_item",
        ),
        (
            dependency::Entity.table_name(),
            "types_registry__dependency",
        ),
        (
            instance_revision::Entity.table_name(),
            "types_registry__instance_revision",
        ),
        (instance::Entity.table_name(), "types_registry__instance"),
    ] {
        assert_eq!(got, expected);
    }
}
