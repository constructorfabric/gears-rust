//! Rename group and tenant schema IDs in one transaction, preserving dictionary keys.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};
use toolkit_gts::gts_id;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rename_group_prefix(
            manager,
            gts_id!("cf.core.rg.type.v1~"),
            gts_id!("cf.core.rg.group.v1~"),
        )
        .await?;
        rename_group_prefix(
            manager,
            gts_id!("cf.core.rg.group.v1~cf.core._.tenant.v1~"),
            gts_id!("cf.core.rg.group.v1~cf.core.rg.tenant.v1~"),
        )
        .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "irreversible group and tenant type rename: restore the pre-upgrade backup to recover the original IDs".into(),
        ))
    }
}

async fn rename_group_prefix(
    manager: &SchemaManager<'_>,
    old: &'static str,
    new: &'static str,
) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    let backend = manager.get_database_backend();
    let rename = format!("'{new}' || substr(schema_id, length('{old}') + 1)");
    let eligible = format!("substr(schema_id, 1, length('{old}')) = '{old}'");
    let preflight = format!(
        "WITH renames AS (SELECT {rename} AS target FROM gts_type WHERE {eligible}) \
         SELECT target FROM renames WHERE length(target) > 1024 \
         OR EXISTS (SELECT 1 FROM gts_type WHERE schema_id = renames.target) LIMIT 1"
    );
    if let Some(row) = conn
        .query_one_raw(Statement::from_string(backend, preflight))
        .await?
    {
        let target: String = row.try_get("", "target")?;
        let reason = if target.chars().count() > 1024 {
            "exceeds 1024 characters"
        } else {
            "already exists"
        };
        return Err(DbErr::Migration(format!(
            "cannot rename group type: target '{target}' {reason}"
        )));
    }
    conn.execute_unprepared(&format!(
        "UPDATE gts_type SET schema_id = {rename} WHERE {eligible}"
    ))
    .await?;
    Ok(())
}
