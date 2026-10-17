//! Versioned platform policies. Header locks serialize pointer changes; the
//! partial unique index permits scope reuse after deletion without losing history.
use sea_orm_migration::prelude::*;

use super::ensure_supported;

#[derive(DeriveIden)]
enum QePolicies {
    Table,
    Id,
    ScopeKey,
    ActiveVersion,
    HighWater,
}

#[derive(DeriveIden)]
enum QePolicyVersions {
    Table,
    PolicyId,
    Version,
    State,
    Payload,
}

#[derive(DeriveIden)]
enum QePolicyOperationLog {
    Table,
    Id,
    PolicyId,
    Version,
    Operation,
    Actor,
    Comment,
    OccurredAt,
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        manager
            .create_table(
                Table::create()
                    .table(QePolicies::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QePolicies::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(QePolicies::ScopeKey).text().not_null())
                    .col(
                        ColumnDef::new(QePolicies::ActiveVersion)
                            .big_integer()
                            .null()
                            .check(
                                Expr::col(QePolicies::ActiveVersion)
                                    .is_null()
                                    .or(Expr::col(QePolicies::ActiveVersion).gte(1)),
                            ),
                    )
                    .col(
                        ColumnDef::new(QePolicies::HighWater)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(QePolicies::HighWater).gte(1)),
                    )
                    .check(Expr::col(QePolicies::ActiveVersion).is_null().or(
                        Expr::col(QePolicies::ActiveVersion).lte(Expr::col(QePolicies::HighWater)),
                    ))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_qe_policies_live_scope")
                    .table(QePolicies::Table)
                    .col(QePolicies::ScopeKey)
                    .unique()
                    .and_where(Expr::col(QePolicies::ActiveVersion).is_not_null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(QePolicyVersions::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(QePolicyVersions::PolicyId).text().not_null())
                    .col(
                        ColumnDef::new(QePolicyVersions::Version)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(QePolicyVersions::Version).gte(1)),
                    )
                    .col(
                        ColumnDef::new(QePolicyVersions::State)
                            .text()
                            .not_null()
                            .check(Expr::col(QePolicyVersions::State).is_in([
                                "active",
                                "superseded",
                                "rolled_back",
                                "deleted",
                            ])),
                    )
                    .col(ColumnDef::new(QePolicyVersions::Payload).text().not_null())
                    .primary_key(
                        Index::create()
                            .col(QePolicyVersions::PolicyId)
                            .col(QePolicyVersions::Version),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(QePolicyVersions::Table, QePolicyVersions::PolicyId)
                            .to(QePolicies::Table, QePolicies::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_qe_policy_one_active")
                    .table(QePolicyVersions::Table)
                    .col(QePolicyVersions::PolicyId)
                    .unique()
                    .and_where(Expr::col(QePolicyVersions::State).eq("active"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(QePolicyOperationLog::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QePolicyOperationLog::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(QePolicyOperationLog::PolicyId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QePolicyOperationLog::Version)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QePolicyOperationLog::Operation)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QePolicyOperationLog::Actor)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(QePolicyOperationLog::Comment).text().null())
                    .col(
                        ColumnDef::new(QePolicyOperationLog::OccurredAt)
                            .text()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(QePolicyOperationLog::Table, QePolicyOperationLog::PolicyId)
                            .to(QePolicies::Table, QePolicies::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;
        // Read back per policy in transition order, so the audit read stays indexed.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_qe_policy_log_policy")
                    .table(QePolicyOperationLog::Table)
                    .col(QePolicyOperationLog::PolicyId)
                    .col(QePolicyOperationLog::OccurredAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported(manager)?;
        manager
            .drop_table(
                Table::drop()
                    .table(QePolicyOperationLog::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(QePolicyVersions::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(QePolicies::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
