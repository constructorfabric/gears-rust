//! `policy_engine__assignment` - attaches a bundle to a tenant. There is no
//! version column by design: the active version is resolved at evaluation.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

/// Scoped by the assigning administrator's tenant (`owner_tenant_id`), not by
/// `tenant_id`, which is the tenant the assignment attaches to.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "policy_engine__assignment")]
#[secure(tenant_col = "owner_tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Assignment identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Assigned bundle.
    pub bundle_id: Uuid,
    /// Tenant the assignment attaches to.
    pub tenant_id: Uuid,
    /// Owning tenant of the assigning administrator; the scope column.
    pub owner_tenant_id: Uuid,
    /// Whether denials are enforced or only reported.
    pub enforce: bool,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Last modification time.
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// The assigned bundle.
    #[sea_orm(
        belongs_to = "super::bundle::Entity",
        from = "Column::BundleId",
        to = "super::bundle::Column::Id"
    )]
    Bundle,
}

impl Related<super::bundle::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Bundle.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
