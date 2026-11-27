use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One subject's profile schema as the host set it. Keyed and scoped like the subject's settings: the subject id is
/// both the resource id and the owner.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-settings-storage:p1
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "construct__subject_schemas")]
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
    /// The schema document as JSON.
    pub schema_json: String,
    /// How many names the person has made the process keep, over the life of the schema, removals not refunded: a
    /// name is charged to whoever's change first brings it in before the process keeps it, and each is kept for the
    /// life of the process. A name another person's change already made the process keep costs nothing.
    pub names_charged: i64,
    /// The version: how many times the schema was changed, an erasure counting as one. A row a charge stored before
    /// any schema was set is version 0 and holds the default schema.
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
