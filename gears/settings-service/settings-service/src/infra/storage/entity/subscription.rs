//! The `subscriptions` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One consumer instance's registration for one setting at one tenant.
///
/// `tenant` is a bare lowercase UUID string and the table has no tenant
/// column in the scoping sense: a registration is the consumer's own and is
/// read by key under the key's mutation lock, never by caller scope. The unique
/// `uq_sub_subscriber_instance_key_tenant` makes a repeat registration of an
/// instance idempotent.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "subscriptions")]
#[secure(unrestricted)]
pub struct Model {
    /// Subscription id (`UUIDv7`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The consumer identity that registered it.
    pub subscriber: String,
    /// The consumer instance — one process of the subscriber.
    pub instance_id: Uuid,
    /// The instance's informational label, at most 128 characters.
    #[sea_orm(nullable)]
    pub instance_label: Option<String>,
    /// The exact setting key.
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
    /// When the instance was first called silent for the key and tenant;
    /// absent while it is not silent.
    #[sea_orm(nullable)]
    pub silent_since: Option<OffsetDateTime>,
    /// When it was registered.
    pub created_at: OffsetDateTime,
}

/// Relations of the `subscription` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
