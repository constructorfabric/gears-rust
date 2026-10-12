//! Tenant-scoped storage for `pricing_plan`.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_plan")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    /// The projection a revision's apply writes.
    pub published_rev: Option<i32>,
    pub version: i64,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    /// The draft or pending revision, when the plan holds one (D-484). Paired with `work_state`.
    pub work_revision_id: Option<Uuid>,
    /// `draft` or `pending`, and only then.
    pub work_state: Option<String>,
    /// The scheduled revision waiting for its date. Paired with `scheduled_from`.
    pub scheduled_revision_id: Option<Uuid>,
    /// That revision's `available_from`.
    pub scheduled_from: Option<TimeDate>,
    /// The stored published revision, not the one a due schedule reads as.
    pub published_revision_id: Option<Uuid>,
    /// The book of `work ?? scheduled ?? published` (D-484).
    pub current_book_id: Option<Uuid>,
    /// That book's currency. Book currency does not change.
    pub current_currency: Option<String>,
    /// The latest `updated_at` of the plan and its revisions (D-484). Not the If-Match clock.
    pub last_activity_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
