// @cpt-cf-chat-engine-dbtable-plugin-configs:p1
//
// Creates `plugin_configs` with composite PK `(plugin_instance_id,
// session_type_id)`. `config` is JSONB — its shape is owned by the plugin via
// its registered GTS schema; Chat Engine treats the value as opaque.

use sea_orm_migration::prelude::*;

use super::m20260417_000001_create_session_tables::SessionTypes;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PluginConfigs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PluginConfigs::PluginInstanceId)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PluginConfigs::SessionTypeId)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PluginConfigs::Config).json_binary().null())
                    .col(
                        ColumnDef::new(PluginConfigs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PluginConfigs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .name("pk_plugin_configs")
                            .col(PluginConfigs::PluginInstanceId)
                            .col(PluginConfigs::SessionTypeId)
                            .primary(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_plugin_configs_session_type")
                            .from(PluginConfigs::Table, PluginConfigs::SessionTypeId)
                            .to(SessionTypes::Table, SessionTypes::SessionTypeId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(PluginConfigs::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum PluginConfigs {
    Table,
    PluginInstanceId,
    SessionTypeId,
    Config,
    CreatedAt,
    UpdatedAt,
}
