//! The `subscriptions` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One consumer's registration for one setting at one tenant.
///
/// `tenant` is a bare lowercase UUID string and the table has no tenant
/// column in the scoping sense: a registration is the consumer's own and is
/// read by key under the key's mutation lock, never by caller scope. The unique
/// `uq_sub_subscriber_key_tenant` makes a registration idempotent.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "subscriptions")]
#[secure(unrestricted)]
pub struct Model {
    /// Subscription id (`UUIDv7`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The consumer identity that registered it.
    pub subscriber: String,
    /// The exact setting key.
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
    /// When it was registered.
    pub created_at: OffsetDateTime,
}

/// Relations of the `subscription` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
