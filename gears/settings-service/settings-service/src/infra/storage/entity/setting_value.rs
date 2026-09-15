// Created: 2026-09-06 by Constructor Tech
//! The `setting_values` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A stored setting value at one scope.
///
/// # Tenant-scoped
///
/// `tenant_col = "tenant_id"`: every value belongs to a tenant, the root
/// tenant's id standing for platform scope, so the ordinary row-scoped data path
/// applies. The one read that steps outside the caller's scope — the ancestor
/// walk of a `cascading` resolution — is the resolver's single named elevation,
/// not a property of this entity.
///
/// # Invariants the database owns
///
/// Exactly one of `value` and `secret_ref` is set, and which one follows the
/// denormalized `data_classification`; a subject is named by both of its
/// columns or by neither; and uniqueness is per scope shape through two partial
/// indexes. All of it is `CHECK`s and indexes in the schema, so the mapping here
/// is plain `Option`s and `String`s.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "setting_values")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Surrogate key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The declaration this value belongs to; `ON DELETE CASCADE`.
    pub declaration_id: Uuid,
    /// Scope as a tenant id — the root tenant's id for platform scope — never
    /// a path and never `NULL`.
    pub tenant_id: Uuid,
    /// GTS type id of the subject the value is attached to, if any.
    #[sea_orm(nullable)]
    pub subject_type: Option<String>,
    /// The subject's own identifier within its type; present exactly when
    /// `subject_type` is.
    #[sea_orm(nullable)]
    pub subject_id: Option<String>,
    /// The inline value; `None` when the value is a secret held by reference.
    /// A JSON `null` is a value, not an absence.
    #[sea_orm(nullable)]
    pub value: Option<Json>,
    /// Credential Store reference for a `secret`-trait value.
    #[sea_orm(nullable)]
    pub secret_ref: Option<String>,
    /// The owning declaration's classification, copied here so the search
    /// corpus's partial-index predicates can read it.
    pub data_classification: String,
    /// `true` when the value no longer validates against the setting's current
    /// type; excluded from resolution until corrected.
    pub needs_review: bool,
    /// Why the value was flagged; `None` when it is not.
    #[sea_orm(nullable)]
    pub needs_review_detail: Option<String>,
    /// When this scoped value last changed — the value arm of the effective
    /// recency, and the tag a write at this scope must present.
    pub last_change_at: OffsetDateTime,
    /// Row creation time.
    pub created_at: OffsetDateTime,
    /// Last write time.
    pub updated_at: OffsetDateTime,
    /// Subject who set the value.
    pub set_by: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
