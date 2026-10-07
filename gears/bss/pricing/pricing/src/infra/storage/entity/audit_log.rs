//! Scoped persistence model, following Products.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_audit")]
#[secure(tenant_col = "tenant_id", resource_col = "audit_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_id: Uuid,
    pub tenant_id: Uuid,
    /// The acting principal from `SecurityContext::subject_id()`; no actor table.
    pub actor_ref: Uuid,
    /// The audit action token (`design/01-foundation.md` §4.4). No
    /// vocabulary `CHECK` yet — an owed debt the migration's own doc names.
    pub action: String,
    /// The kind of thing `subject_id` names. Same owed debt as `action`.
    pub subject_kind: String,
    /// The subject's id. Nullable in the DDL; the one writer,
    /// `write_eventless_act_audit`, sets it on every row.
    pub subject_id: Option<Uuid>,
    /// The subject's revision at the time of the act. Nullable in the DDL;
    /// `support::audit`, through which every pricing row is written, always
    /// supplies it.
    pub subject_revision: Option<i64>,
    /// Carried in the DDL and always `NULL`: no door writes a refusal row
    /// (D-433).
    pub error_code: Option<String>,
    /// Carried in the DDL and always `NULL` (D-433).
    pub attempted_key: Option<String>,
    /// A free-text reason, where the door supplies one.
    pub reason: Option<String>,
    /// The request's edge correlation (D-431), as `text` (D-433). Never
    /// `NULL`: a rereserve op's rows carry the id the op minted.
    pub correlation_id: Option<String>,
    /// The commit instant.
    pub written_at: TimeDateTimeWithTimeZone,
    /// Carried in the DDL; no writer sets it, so it is always `NULL`.
    pub session_id: Option<Uuid>,
    /// Carried in the DDL (D-433); no writer sets it, so it is always `NULL`.
    pub ceremony_ref: Option<Uuid>,
    /// `unsealed | sealed`. Written `unsealed` at INSERT, always; this gear
    /// never advances it.
    pub seal_state: String,
    /// Reserved for the platform sealing capability. `NULL` until sealed.
    pub chain_id: Option<Uuid>,
    /// Reserved for the platform sealing capability. `NULL` until sealed.
    pub seq: Option<i64>,
    /// Reserved for the platform sealing capability. `NULL` on the segment
    /// head and until sealed.
    pub prev_hash: Option<Vec<u8>>,
    /// Reserved for the platform sealing capability. `NULL` until sealed.
    pub row_hash: Option<Vec<u8>>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
