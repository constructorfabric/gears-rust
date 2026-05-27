// @cpt-cf-chat-engine-dbtable-sessions:p1
// @cpt-cf-chat-engine-dbtable-session-types:p1
//
// Creates `session_types` (referenced FK target) and `sessions` (with the
// soft-delete columns mandated by ADR-0021). JSONB / TEXT mapping is delegated
// to SeaORM's portable `ColumnType::JsonBinary` so the same migration compiles
// for both the Postgres and SQLite backends exposed by `modkit-db`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SessionTypes::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionTypes::SessionTypeId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SessionTypes::Name).string().not_null())
                    .col(
                        ColumnDef::new(SessionTypes::PluginInstanceId)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SessionTypes::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionTypes::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Sessions::SessionId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Sessions::TenantId).string().not_null())
                    .col(ColumnDef::new(Sessions::UserId).string().not_null())
                    .col(ColumnDef::new(Sessions::ClientId).string().null())
                    .col(ColumnDef::new(Sessions::SessionTypeId).uuid().null())
                    .col(
                        ColumnDef::new(Sessions::EnabledCapabilities)
                            .json_binary()
                            .null(),
                    )
                    .col(ColumnDef::new(Sessions::Metadata).json_binary().null())
                    .col(
                        ColumnDef::new(Sessions::LifecycleState)
                            .string()
                            .not_null()
                            .default("active"),
                    )
                    .col(
                        ColumnDef::new(Sessions::ShareToken)
                            .string()
                            .null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Sessions::DeletedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::ScheduledHardDeleteAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sessions_session_type")
                            .from(Sessions::Table, Sessions::SessionTypeId)
                            .to(SessionTypes::Table, SessionTypes::SessionTypeId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_tenant_user")
                    .table(Sessions::Table)
                    .col(Sessions::TenantId)
                    .col(Sessions::UserId)
                    .col(Sessions::LifecycleState)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_sessions_tenant_user")
                    .table(Sessions::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Sessions::Table).if_exists().to_owned())
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(SessionTypes::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum SessionTypes {
    Table,
    SessionTypeId,
    Name,
    PluginInstanceId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum Sessions {
    Table,
    SessionId,
    TenantId,
    UserId,
    ClientId,
    SessionTypeId,
    EnabledCapabilities,
    Metadata,
    LifecycleState,
    ShareToken,
    DeletedAt,
    ScheduledHardDeleteAt,
    CreatedAt,
    UpdatedAt,
}
