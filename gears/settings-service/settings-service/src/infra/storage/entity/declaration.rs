// Created: 2026-08-25 by Constructor Tech
//! The `setting_declarations` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A setting declaration.
///
/// # Not tenant-scoped
///
/// `no_tenant` for the same reason as [`super::category`]: a declaration is part
/// of the platform's settings *catalogue*, not tenant data. Tenants differ in
/// the values they hold, which live in `setting_values` and carry the tenant
/// column; the declarations those values resolve against are the same
/// everywhere.
///
/// # Invariants the database owns
///
/// Three cross-field rules are `CHECK` constraints rather than application
/// guards, so no path can write a contradiction: an anonymous-exposable
/// declaration is never `secret` or `pii`, `data_classification = 'secret'` holds exactly when
/// `has_secret_trait` does, and `owner_module` is present exactly when `source`
/// is `module_contributed`. The mapping here is deliberately plain `String` and
/// `bool` -- the vocabulary is enforced where it cannot be bypassed.
// Three of the columns are booleans and the lint would rather see a state enum.
// This type is a row, not a domain model: the shape is the table's, and the
// invariants that would justify collapsing the flags are `CHECK` constraints in
// the schema, where they hold for every writer rather than only for this one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "setting_declarations")]
#[secure(no_tenant, resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Surrogate key, stable across a re-key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,

    /// The full setting key, unique via `uq_declaration_key`.
    #[sea_orm(unique)]
    pub key: String,

    /// The setting's own name slug, unique per category via
    /// `uq_declaration_category_slug`.
    pub leaf_slug: String,

    /// GTS id of the value type the default and every override validate against.
    pub value_type_id: String,

    /// Owning category. `ON DELETE RESTRICT`, which is what makes the no-orphan
    /// rule the database's to enforce.
    pub category_id: Uuid,

    /// The Schema Default -- authoritative, and never null. A setting with no
    /// meaningful default holds a JSON `null` rather than a SQL one.
    pub default_value: Json,

    /// `global`, `cascading`, or `local`. Override and inheritance behaviour is
    /// derived from this rather than from independently settable flags.
    pub scope_class: String,

    /// `standard` or `advanced`.
    pub mode: String,

    /// Whether changing this setting's value requires elevated confirmation.
    /// Defaults to `true`: a declaration that says nothing is protected, and
    /// clearing the flag is itself step-up-gated.
    pub requires_step_up: bool,

    /// Whether the effective value may be served on the unauthenticated read
    /// surface. Never true for a `secret` or `pii` declaration — a `CHECK`.
    pub anonymous_exposable: bool,

    /// Optional administrative domain, used to filter listings.
    pub domain_affinity: Option<String>,

    /// Denormalised from the value type's GTS traits so masking does not need a
    /// registry round trip.
    pub has_secret_trait: bool,

    /// `public`, `pii`, or `secret`; `secret` is derived from the trait.
    pub data_classification: String,

    /// `admin_authored` or `module_contributed`.
    pub source: String,

    /// The contributing module, present exactly when `source` is
    /// `module_contributed`.
    pub owner_module: Option<String>,

    /// Optional licence feature gating the setting.
    pub licence_feature: Option<String>,

    /// `active` or `retired`. Retirement is a soft delete: the row and its
    /// overrides remain, excluded from resolution.
    pub status: String,

    /// Optional long-form description.
    pub description: Option<String>,

    /// When the declaration last changed in a way an author would recognise.
    pub last_change_at: OffsetDateTime,

    /// When the row was created.
    pub created_at: OffsetDateTime,

    /// When the row last changed.
    pub updated_at: OffsetDateTime,

    /// Who created it.
    pub created_by: String,
}

/// The owning category.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// `ON DELETE RESTRICT`: a category cannot be removed while a declaration
    /// still points at it, whatever that declaration's `status`.
    #[sea_orm(
        belongs_to = "super::category::Entity",
        from = "Column::CategoryId",
        to = "super::category::Column::Id",
        on_delete = "Restrict"
    )]
    Category,
}

impl Related<super::category::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Category.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
