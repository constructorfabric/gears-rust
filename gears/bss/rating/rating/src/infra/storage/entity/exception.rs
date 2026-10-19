//! `bss_rating__exception` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-exception`).
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__exception")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "exception_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub exception_id: Uuid,
    pub tenant_id: Uuid,
    /// `usage_record | child | fact | inbox | source_loss`.
    pub subject_kind: String,
    pub subject_ref: String,
    pub reason_code: String,
    pub detail: String,
    pub first_seen_at: TimeDateTimeWithTimeZone,
    pub last_attempt_at: TimeDateTimeWithTimeZone,
    pub attempts: i32,
    /// `NULL` while open; one open row per `(tenant_id, subject_kind, subject_ref, reason_code)`.
    pub resolved_at: Option<TimeDateTimeWithTimeZone>,
    pub resolution: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
