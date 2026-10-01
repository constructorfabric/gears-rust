//! Initial policy-engine schema: bundles, versions, documents and
//! assignments.
//!
//! Tables are built with the portable schema builder. `document.version_id`
//! cascades, so deleting a draft removes its documents in one statement.
//! `bundle_version.bundle_id` and `assignment.bundle_id` reference the bundle
//! without a cascade. Two partial unique indexes keep at most one active and
//! at most one draft version per bundle.
//!
//! Supported backends: Postgres and `SQLite`. `MySQL` is refused.

use sea_orm_migration::prelude::*;

use super::ensure_supported_backend;
use crate::infra::storage::entity::bundle_version::{STATE_ACTIVE, STATE_DRAFT};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Bundle {
    #[sea_orm(iden = "policy_engine__bundle")]
    Table,
    Id,
    OwnerTenantId,
    Name,
    Description,
    CreatedAt,
    CreatedBy,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum BundleVersion {
    #[sea_orm(iden = "policy_engine__bundle_version")]
    Table,
    Id,
    BundleId,
    OwnerTenantId,
    Ordinal,
    State,
    CreatedAt,
    ActivatedAt,
    ActivatedBy,
}

#[derive(DeriveIden)]
enum Document {
    #[sea_orm(iden = "policy_engine__document")]
    Table,
    Id,
    VersionId,
    OwnerTenantId,
    Name,
    Content,
    ResourceTypes,
    Actions,
}

#[derive(DeriveIden)]
enum Assignment {
    #[sea_orm(iden = "policy_engine__assignment")]
    Table,
    Id,
    BundleId,
    TenantId,
    OwnerTenantId,
    Enforce,
    CreatedAt,
    UpdatedAt,
}

/// Tables in creation (dependency) order; `down` drops them in reverse.
pub const TABLES: &[&str] = &[
    "policy_engine__bundle",
    "policy_engine__bundle_version",
    "policy_engine__document",
    "policy_engine__assignment",
];

/// Explicitly named indexes this migration creates.
#[cfg(test)]
pub const INDEXES: &[&str] = &[
    "uq_policy_engine__bundle__tenant_name",
    "uq_policy_engine__bundle_version__bundle_ordinal",
    "uq_policy_engine__bundle_version__one_active",
    "uq_policy_engine__bundle_version__one_draft",
    "uq_policy_engine__document__version_name",
    "uq_policy_engine__assignment__bundle_tenant",
    "idx_policy_engine__assignment__tenant",
    "idx_policy_engine__assignment__bundle",
];

fn uuid_col<C: IntoIden>(col: C) -> ColumnDef {
    ColumnDef::new(col).uuid().not_null().to_owned()
}

fn uuid_null<C: IntoIden>(col: C) -> ColumnDef {
    ColumnDef::new(col).uuid().null().to_owned()
}

fn ts_col<C: IntoIden>(col: C) -> ColumnDef {
    ColumnDef::new(col)
        .timestamp_with_time_zone()
        .not_null()
        .to_owned()
}

fn ts_null<C: IntoIden>(col: C) -> ColumnDef {
    ColumnDef::new(col)
        .timestamp_with_time_zone()
        .null()
        .to_owned()
}

fn pk_uuid<C: IntoIden>(col: C) -> ColumnDef {
    ColumnDef::new(col)
        .uuid()
        .not_null()
        .primary_key()
        .to_owned()
}

fn bundle_table() -> TableCreateStatement {
    Table::create()
        .table(Bundle::Table)
        .if_not_exists()
        .col(pk_uuid(Bundle::Id))
        .col(uuid_col(Bundle::OwnerTenantId))
        .col(ColumnDef::new(Bundle::Name).text().not_null())
        .col(ColumnDef::new(Bundle::Description).text().null())
        .col(ts_col(Bundle::CreatedAt))
        .col(uuid_col(Bundle::CreatedBy))
        .col(ts_col(Bundle::UpdatedAt))
        .to_owned()
}

fn bundle_version_table() -> TableCreateStatement {
    Table::create()
        .table(BundleVersion::Table)
        .if_not_exists()
        .col(pk_uuid(BundleVersion::Id))
        .col(uuid_col(BundleVersion::BundleId))
        .col(uuid_col(BundleVersion::OwnerTenantId))
        .col(ColumnDef::new(BundleVersion::Ordinal).integer().not_null())
        .col(
            ColumnDef::new(BundleVersion::State)
                .small_integer()
                .not_null(),
        )
        .col(ts_col(BundleVersion::CreatedAt))
        .col(ts_null(BundleVersion::ActivatedAt))
        .col(uuid_null(BundleVersion::ActivatedBy))
        .foreign_key(
            ForeignKey::create()
                .name("fk_policy_engine__bundle_version__bundle")
                .from(BundleVersion::Table, BundleVersion::BundleId)
                .to(Bundle::Table, Bundle::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .to_owned()
}

fn document_table() -> TableCreateStatement {
    Table::create()
        .table(Document::Table)
        .if_not_exists()
        .col(pk_uuid(Document::Id))
        .col(uuid_col(Document::VersionId))
        .col(uuid_col(Document::OwnerTenantId))
        .col(ColumnDef::new(Document::Name).text().not_null())
        .col(ColumnDef::new(Document::Content).text().not_null())
        .col(
            ColumnDef::new(Document::ResourceTypes)
                .json_binary()
                .not_null(),
        )
        .col(ColumnDef::new(Document::Actions).json_binary().not_null())
        .foreign_key(
            ForeignKey::create()
                .name("fk_policy_engine__document__version")
                .from(Document::Table, Document::VersionId)
                .to(BundleVersion::Table, BundleVersion::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

fn assignment_table() -> TableCreateStatement {
    Table::create()
        .table(Assignment::Table)
        .if_not_exists()
        .col(pk_uuid(Assignment::Id))
        .col(uuid_col(Assignment::BundleId))
        .col(uuid_col(Assignment::TenantId))
        .col(uuid_col(Assignment::OwnerTenantId))
        .col(ColumnDef::new(Assignment::Enforce).boolean().not_null())
        .col(ts_col(Assignment::CreatedAt))
        .col(ts_col(Assignment::UpdatedAt))
        .foreign_key(
            ForeignKey::create()
                .name("fk_policy_engine__assignment__bundle")
                .from(Assignment::Table, Assignment::BundleId)
                .to(Bundle::Table, Bundle::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .to_owned()
}

fn indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("uq_policy_engine__bundle__tenant_name")
            .table(Bundle::Table)
            .col(Bundle::OwnerTenantId)
            .col(Bundle::Name)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("uq_policy_engine__bundle_version__bundle_ordinal")
            .table(BundleVersion::Table)
            .col(BundleVersion::BundleId)
            .col(BundleVersion::Ordinal)
            .unique()
            .if_not_exists()
            .to_owned(),
        // At most one active version per bundle: a partial unique index, which
        // both supported backends accept in this form.
        Index::create()
            .name("uq_policy_engine__bundle_version__one_active")
            .table(BundleVersion::Table)
            .col(BundleVersion::BundleId)
            .unique()
            .and_where(Expr::col(BundleVersion::State).eq(STATE_ACTIVE))
            .if_not_exists()
            .to_owned(),
        // At most one open draft per bundle, so a retried draft creation is a
        // conflict rather than a second draft.
        Index::create()
            .name("uq_policy_engine__bundle_version__one_draft")
            .table(BundleVersion::Table)
            .col(BundleVersion::BundleId)
            .unique()
            .and_where(Expr::col(BundleVersion::State).eq(STATE_DRAFT))
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("uq_policy_engine__document__version_name")
            .table(Document::Table)
            .col(Document::VersionId)
            .col(Document::Name)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("uq_policy_engine__assignment__bundle_tenant")
            .table(Assignment::Table)
            .col(Assignment::BundleId)
            .col(Assignment::TenantId)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_policy_engine__assignment__tenant")
            .table(Assignment::Table)
            .col(Assignment::TenantId)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_policy_engine__assignment__bundle")
            .table(Assignment::Table)
            .col(Assignment::BundleId)
            .if_not_exists()
            .to_owned(),
    ]
}

/// The Postgres rendering of every table and index statement, for
/// statement-level tests of the dialect no unit test can execute.
#[cfg(test)]
pub fn postgres_statements() -> Vec<String> {
    let mut out: Vec<String> = [
        bundle_table(),
        bundle_version_table(),
        document_table(),
        assignment_table(),
    ]
    .iter()
    .map(|t| t.to_string(PostgresQueryBuilder))
    .collect();
    out.extend(indexes().iter().map(|i| i.to_string(PostgresQueryBuilder)));
    out
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        ensure_supported_backend(backend)?;

        manager.create_table(bundle_table()).await?;
        manager.create_table(bundle_version_table()).await?;
        manager.create_table(document_table()).await?;
        manager.create_table(assignment_table()).await?;
        for index in indexes() {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_supported_backend(manager.get_database_backend())?;
        for table in TABLES.iter().rev() {
            manager
                .drop_table(
                    Table::drop()
                        .table(Alias::new(*table))
                        .if_exists()
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
