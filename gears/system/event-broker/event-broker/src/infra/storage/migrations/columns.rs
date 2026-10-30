//! Column types and index creation shared by this gear's migrations, so every
//! table maps to `SQLite`, `PostgreSQL` and `MySQL`/`MariaDB` the same way.
//!
//! UUIDs and timestamps use the engine's native types, because the entities
//! bind them as `Uuid` and `DateTime<Utc>` (secure scoping binds tenant
//! values as `Uuid`). Strings are sized `VARCHAR`s; only an unbounded
//! document is text.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DbBackend;

/// `GTS_ID_MAX_LENGTH`: the longest GTS identifier the grammar admits.
pub(super) const GTS_ID_LEN: u32 = 1024;

fn is_mysql(manager: &SchemaManager<'_>) -> bool {
    manager.get_database_backend() == DbBackend::MySql
}

/// A GTS identifier, compared byte-wise. On `MySQL` it is ASCII with a binary
/// collation: a GTS identifier is ASCII by grammar, the default `utf8mb4`
/// collation would compare it case-insensitively, and at four bytes per
/// character a 1024-character key exceeds `InnoDB`'s 3072-byte key limit.
pub(super) fn gts_id<T: IntoIden>(manager: &SchemaManager<'_>, col: T) -> ColumnDef {
    let mut def = ColumnDef::new(col);
    if is_mysql(manager) {
        def.custom(Alias::new(format!(
            "varchar({GTS_ID_LEN}) CHARACTER SET ascii COLLATE ascii_bin"
        )));
    } else {
        def.string_len(GTS_ID_LEN);
    }
    def.not_null().to_owned()
}

/// A UTC timestamp with microseconds. On `MySQL` a timestamp with time zone
/// becomes `timestamp`, which keeps whole seconds, so it is `timestamp(6)`
/// there; the driver sets the session time zone to UTC.
pub(super) fn timestamp<T: IntoIden>(manager: &SchemaManager<'_>, col: T) -> ColumnDef {
    let mut def = ColumnDef::new(col);
    if is_mysql(manager) {
        def.custom(Alias::new("timestamp(6)"));
    } else {
        def.timestamp_with_time_zone();
    }
    def.not_null().to_owned()
}

/// A JSON document stored as text, never queried into. `MySQL`'s `text` stops
/// at 64 KiB, so it is `longtext` there; the other engines' text is
/// unbounded.
pub(super) fn document<T: IntoIden>(manager: &SchemaManager<'_>, col: T) -> ColumnDef {
    let mut def = ColumnDef::new(col);
    if is_mysql(manager) {
        def.custom(Alias::new("longtext"));
    } else {
        def.text();
    }
    def.not_null().to_owned()
}

/// `CREATE [UNIQUE] INDEX name ON table (cols)`, skipped when the index
/// exists: `MySQL` has no `CREATE INDEX IF NOT EXISTS`.
pub(super) async fn create_index<T: IntoIden + Copy + 'static>(
    manager: &SchemaManager<'_>,
    name: &str,
    table: T,
    cols: &[T],
    unique: bool,
) -> Result<(), DbErr> {
    let table_name = table.into_iden().to_string();
    if manager.has_index(&table_name, name).await? {
        return Ok(());
    }
    let mut stmt = Index::create();
    stmt.name(name).table(table);
    for col in cols {
        stmt.col(*col);
    }
    if unique {
        stmt.unique();
    }
    manager.create_index(stmt).await
}
