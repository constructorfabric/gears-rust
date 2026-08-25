// Created: 2026-08-13 by Constructor Tech
//! The `categories` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A settings category.
///
/// # Not tenant-scoped
///
/// `no_tenant` is deliberate and matches DESIGN.md §4.7: the table has no
/// tenant column. A category is part of the platform's settings *taxonomy* —
/// the structure administrators browse — not tenant data. Tenants differ in the
/// **values** they hold, which are scoped in `setting_values`; the categories
/// those values are organised under are the same everywhere.
///
/// # Flat, so `name` is globally unique
///
/// There is no parent column and no closure table. With nothing to scope a name
/// within, two categories sharing one would be indistinguishable to the
/// administrator choosing between them, so `uq_category_name` is global rather
/// than per-parent.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "categories")]
#[secure(no_tenant, resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Surrogate key. Stable across a rename, which is why the mutable `key`
    /// is not the primary key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,

    /// The category's stable slug, unique via `uq_category_key`.
    ///
    /// Stored verbatim: this becomes the category segment of every setting key
    /// declared under it, so trimming or case-folding here would give one
    /// category two spellings and break the keys beneath it.
    #[sea_orm(unique)]
    pub key: String,

    /// Display name, unique via `uq_category_name`.
    #[sea_orm(unique)]
    pub name: String,

    /// Optional long-form description.
    pub description: Option<String>,

    /// Optional domain this category belongs to, used to filter listings.
    pub domain_affinity: Option<String>,

    /// Ordering weight for listings; lower sorts first.
    pub sort_order: i32,

    /// Optional icon reference for administrative interfaces.
    pub icon: Option<String>,

    /// When the row was created.
    pub created_at: OffsetDateTime,

    /// When the row last changed. Refreshed on every write, and the value the
    /// `If-Match` `ETag` is derived from.
    pub updated_at: OffsetDateTime,
}

/// No relations yet.
///
/// `setting_declarations` gains the foreign key to this table in entry 2.3 —
/// which is also what makes the no-orphan deletion rule enforceable at the
/// database level rather than only in the service.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
