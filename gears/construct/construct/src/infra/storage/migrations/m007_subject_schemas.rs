use sea_orm_migration::prelude::*;

/// A person's profile schema, as the host set it: one row per tenant and subject, holding the schema document as
/// JSON text and how many names the person has made the process keep over the schema's life. A subject without a
/// row, or whose row holds the default schema (a charge can store one before any schema is set), has the default
/// schema.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-settings-storage:p1
#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLE: &str = "construct__subject_schemas";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new(TABLE))
                    .if_not_exists()
                    .col(ColumnDef::new(Alias::new("tenant_id")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("subject_id")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("schema_json")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("names_charged"))
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("version"))
                            .big_integer()
                            .not_null()
                            .default(1),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("tenant_id"))
                            .col(Alias::new("subject_id")),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new(TABLE))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
