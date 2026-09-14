//! `SeaORM` entity for `bss.pricing_plan_descriptor_set` — the billing
//! descriptor set of **one plan revision** (`design/02-plan-definition.md` §6,
//! D-48 as revised by D-110), keyed `(plan_id, plan_revision)`.
//!
//! Genuinely 1:1 per revision, so the key carries no discriminator — the one
//! structural difference from `pricing_plan_phase` and
//! `pricing_plan_addon_rule`, whose keys need one because a revision holds many
//! of each. `plan_revision` is here for the reason it is on those two: the set
//! versions with the revision (D-83).
//!
//! **Three columns, not five.** `billingTiming` and `taxCategory` ride
//! `pricing_price` — the second because `tax_category_ref` is per row and a
//! per-plan column cannot mirror a per-row source of truth (D-110). Adding
//! either back here would be a second, disagreeing home for a value that already
//! has one.
//!
//! There is no `lifecycle_state` here. A descriptor row is frozen when **its**
//! revision publishes, so the parent `pricing_plan` row is the referent and the
//! table's append-only triggers read it.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_plan_descriptor_set")]
#[secure(tenant_col = "tenant_id", resource_col = "plan_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_id: Uuid,
    /// The revision this copy belongs to — the row it is frozen with.
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_revision: i64,
    /// Copied from the parent revision by the repository, never taken from a
    /// request: the foreign key covers `(plan_id, plan_revision)` alone, so a row
    /// claiming a tenant its parent revision does not belong to is refused by the
    /// append-only trigger's parent-tenant arm rather than by the key.
    pub tenant_id: Uuid,
    /// The invoice line template Billing renders from. Nullable, because a
    /// draft may be incomplete — `DESCRIPTOR_INCOMPLETE` is what reports a
    /// missing element, at publish and by name.
    pub invoice_line_template: Option<String>,
    /// The general-ledger code the posting lands on. Nullable, same reason.
    pub gl_code: Option<String>,
    /// How the plan's charges are composed into invoice lines. Nullable, same
    /// reason.
    pub itemization_rule: Option<String>,
    /// P5's config-extensible required-field registry: a JSON object of extra
    /// descriptor names and their values (`jsonb` on Postgres, `text` on
    /// `SQLite`). It is what lets a deployment require a fourth descriptor
    /// without a migration.
    pub additional_fields: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
