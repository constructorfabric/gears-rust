use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// The identity of one received record, kept as its `record_key`
/// ([`crate::domain::record_intake::record_key`]) with the connector and the
/// subject. The provenance and the version are not stored, since a provenance
/// can hold record content.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-storage:p1
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "construct__record_ids")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub record_key: String,
    pub connector: String,
    pub subject_id: Option<Uuid>,
    pub received_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
