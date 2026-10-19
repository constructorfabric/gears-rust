//! `bss_rating__operator_audit` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-operator-audit`):
//! insert-only, written in the transaction of the audited action.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__operator_audit")]
#[secure(tenant_col = "tenant_id", resource_col = "audit_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_id: Uuid,
    /// The subject that performed the action.
    pub actor: Uuid,
    /// The REST/SDK operation id (`bss_rating.inbox.release`, …).
    pub operation_id: String,
    pub target_kind: String,
    pub target_id: String,
    pub request_digest: String,
    pub reason: String,
    /// The second operator of a two-person action.
    pub approved_by: Option<Uuid>,
    /// The caller's request id (`x-request-id`), when it carried one.
    pub request_id: Option<String>,
    pub at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
