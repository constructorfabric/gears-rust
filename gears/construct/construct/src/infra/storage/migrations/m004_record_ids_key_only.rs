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

    /// A row stored before the upgrade keeps its tenant, key, connector and
    /// subject, and its key still refuses a second row, so a resend of a
    /// record received before the upgrade is still a repeat.
    #[tokio::test]
    async fn a_row_stored_before_the_upgrade_still_makes_a_resend_a_repeat() {
        use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

        use crate::domain::record_intake::{IdentityParts, RecordKey};

        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite connects");
        Migrator::up(&db, Some(3)).await.expect("up to m003");
        let key = RecordKey::of(IdentityParts {
            connector: "connector-a",
            provenance: "chat_engine/thread-42/msg-7",
            version: "v1",
        });
        db.execute_unprepared(&format!(
            "INSERT INTO {TABLE} (tenant_id, record_key, connector, provenance, version, \
             subject_id, received_at) VALUES ('tenant-a', '{key}', 'connector-a', \
             'chat_engine/thread-42/msg-7', 'v1', 'subject-a', '2026-10-21T10:00:00Z')"
        ))
        .await
        .expect("a row under m003");

        Migrator::up(&db, None).await.expect("the upgrade");

        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                format!("SELECT tenant_id, record_key, connector, subject_id FROM {TABLE}"),
            ))
            .await
            .expect("query")
            .expect("the row survives");
        let column = |name: &str| row.try_get::<String>("", name).expect(name);
        assert_eq!(column("tenant_id"), "tenant-a");
        assert_eq!(column("record_key"), key.as_str());
        assert_eq!(column("connector"), "connector-a");
        assert_eq!(column("subject_id"), "subject-a");

        let again = db
            .execute_unprepared(&format!(
                "INSERT INTO {TABLE} (tenant_id, record_key, connector, subject_id, \
                 received_at) VALUES ('tenant-a', '{key}', 'connector-a', 'subject-a', \
                 '2026-10-21T11:00:00Z')"
            ))
            .await;
        assert!(again.is_err(), "the key refuses a second row: {again:?}");
    }
}
