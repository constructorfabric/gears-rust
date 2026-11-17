use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One subject's settings. The subject id is both the resource id and the
/// owner the policy service constrains on: a subject owns its settings.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-settings-storage:p1
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "construct__subject_settings")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "subject_id",
    owner_col = "subject_id",
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub subject_id: Uuid,
    pub personalization_enabled: bool,
    pub erasure_in_progress: bool,
    /// The generation of the subject's profile root; an erasure moves it on.
    pub profile_generation: i64,
    /// When the erasure under way was marked; none without one.
    pub erasure_marked_at: Option<OffsetDateTime>,
    /// When the leader's job last tried the erasure under way; none when it
    /// has not.
    pub erasure_attempted_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
