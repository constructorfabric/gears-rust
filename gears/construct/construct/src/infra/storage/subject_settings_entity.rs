use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One subject's settings. The subject id is the resource id the policy
/// service constrains on.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-settings-storage:p1
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "construct__subject_settings")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "subject_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub subject_id: Uuid,
    pub personalization_enabled: bool,
    pub erasure_in_progress: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
