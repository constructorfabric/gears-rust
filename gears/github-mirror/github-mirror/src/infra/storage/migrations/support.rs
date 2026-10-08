//! Helpers shared by the column migrations.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Drop `column` from `table`, skipping tables the reverse pass already
/// removed.
///
/// Asking the schema first is the portable check: matching on the error text would only recognise
/// `SQLite`'s wording and would swallow unrelated failures that happen to
/// contain it.
///
/// # Errors
/// Any error from the existence check or the `ALTER TABLE`.
pub async fn drop_column(
    manager: &SchemaManager<'_>,
    table: &str,
    column: &str,
) -> Result<(), DbErr> {
    if !manager.has_table(table).await? {
        return Ok(());
    }
    manager
        .get_connection()
        .execute_unprepared(&format!("ALTER TABLE {table} DROP COLUMN {column};"))
        .await?;
    Ok(())
}
