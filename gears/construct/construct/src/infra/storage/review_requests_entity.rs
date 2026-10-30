use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One review request, open or closed. `open_fact_id` holds the fact's id
/// while the request is open and nothing once it closes, so the unique key on
/// the tenant and it allows one open request per fact and any number of
/// closed ones. `value` is the flagged value as JSON text, kept only while the
/// request is open. `claim_id` and `claimed_at` name the resolution that holds
/// an open request, and `pending` is what that resolution is about to write,
/// as JSON text, so a resolution that ends without closing the request is
/// closed as it later. The subject is the resource and its owner, as for the
/// subject's facts, so the platform's scope on those reaches its requests.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-storage:p1
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "construct__review_requests")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "subject_id",
    owner_col = "subject_id",
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub fact_id: String,
    pub entity_kind: String,
    pub property: String,
    pub value: Option<String>,
    pub comment: Option<String>,
    pub state: String,
    pub open_fact_id: Option<String>,
    pub outcome: Option<String>,
    pub reason: Option<String>,
    pub claim_id: Option<Uuid>,
    pub claimed_at: Option<OffsetDateTime>,
    pub pending: Option<String>,
    pub resolved_by: Option<Uuid>,
    pub corrected_fact_id: Option<String>,
    pub created_at: OffsetDateTime,
    pub resolved_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
