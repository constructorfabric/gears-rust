use sea_orm_migration::prelude::*;

/// Keeps a record identity only as its key: drops the `provenance` and
/// `version` columns of `construct__record_ids`, since a provenance can hold
/// record content (a fact candidate's is the fact key and its value). The key
/// already holds the identity, so nothing reads the parts.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-storage:p1
#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLE: &str = "construct__record_ids";
const DROPPED: [&str; 2] = ["provenance", "version"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    /// Safe to run twice: a column that is already gone is skipped.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in DROPPED {
            if manager.has_column(TABLE, column).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(TABLE))
                            .drop_column(Alias::new(column))
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Brings the columns back, nullable: the values they held are gone.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in DROPPED {
            if !manager.has_column(TABLE, column).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(TABLE))
                            .add_column(ColumnDef::new(Alias::new(column)).text().null())
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm_migration::sea_orm::Database;
    use sea_orm_migration::{MigrationTrait, MigratorTrait};

    use super::{DROPPED, TABLE};
    use crate::infra::storage::migrations::Migrator;

    /// On a real `SQLite` database: the chain leaves the identity table
    /// without the provenance and the version, and running the step again
    /// changes nothing.
    #[tokio::test]
    async fn the_identity_table_keeps_no_provenance_or_version() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite connects");
        Migrator::up(&db, None).await.expect("the chain runs");
        let manager = sea_orm_migration::SchemaManager::new(&db);

        for column in DROPPED {
            assert!(
                !manager.has_column(TABLE, column).await.expect("schema"),
                "{column}"
            );
        }
        for kept in [
            "tenant_id",
            "record_key",
            "connector",
            "subject_id",
            "received_at",
        ] {
            assert!(
                manager.has_column(TABLE, kept).await.expect("schema"),
                "{kept}"
            );
        }

        super::Migration
            .up(&manager)
            .await
            .expect("a second run is a no-op");
    }
}
