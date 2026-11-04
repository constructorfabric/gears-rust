// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-persistence:p1
//! The writer's own tenant, recorded beside the writer's identity.
//!
//! The upward disclosure boundary shows an identity only when the tenant that
//! recorded it lies on the path from the reader down to the scope the identity
//! concerns, so each place that names an administrator also names the tenant
//! the administrator acted from: `setting_values.set_by_tenant`,
//! `tenant_permissions.set_by_tenant` and `audit_records.actor_tenant_id`.
//!
//! Additive: three nullable `UUID` columns, no default and no `NOT NULL`, so
//! every statement the previous release issued still applies and a rolling
//! upgrade can run old and new instances against the same schema.
//!
//! No backfill. A row written before the column existed has no recorded tenant
//! and reads `NULL`, which the boundary treats as "not on any path": its
//! identity is withheld from every reader but a platform administrator.
//! Deriving a tenant from the row's own scope would store a guess as though the
//! writer had recorded it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum SettingValues {
    Table,
    SetByTenant,
}

#[derive(DeriveIden)]
enum TenantPermissions {
    Table,
    SetByTenant,
}

#[derive(DeriveIden)]
enum AuditRecords {
    Table,
    ActorTenantId,
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SettingValues::Table)
                    .add_column(ColumnDef::new(SettingValues::SetByTenant).uuid().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TenantPermissions::Table)
                    .add_column(ColumnDef::new(TenantPermissions::SetByTenant).uuid().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AuditRecords::Table)
                    .add_column(ColumnDef::new(AuditRecords::ActorTenantId).uuid().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AuditRecords::Table)
                    .drop_column(AuditRecords::ActorTenantId)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TenantPermissions::Table)
                    .drop_column(TenantPermissions::SetByTenant)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(SettingValues::Table)
                    .drop_column(SettingValues::SetByTenant)
                    .to_owned(),
            )
            .await
    }
}
