//! The `activation_state` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// What one subscription is expected to apply, and what it reported.
///
/// At most one row per `(subscriber, key, tenant)` (`uq_activation_state`);
/// the table declares no primary key, so the entity names the three columns of
/// that identity as one. `change_ref` names the change's audit record by value:
/// there is no foreign key, since audit records leave by retention while an
/// `awaiting` row has no deadline.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "activation_state")]
#[secure(unrestricted)]
pub struct Model {
    /// The consumer identity that owes a report.
    #[sea_orm(primary_key, auto_increment = false)]
    pub subscriber: String,
    /// The setting key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant: String,
    /// The change that set the current expectation.
    pub change_ref: Uuid,
    /// The expected value in canonical encoding, or a hash for a secret.
    pub expected_value: String,
    /// `awaiting`, `succeeded` or `failed`.
    pub status: String,
    /// The reported value, in the encoding of `expected_value`.
    #[sea_orm(nullable)]
    pub applied_value: Option<String>,
    /// The consumer's failure detail.
    #[sea_orm(nullable)]
    pub detail: Option<String>,
    /// When the report that settled the row was received.
    #[sea_orm(nullable)]
    pub reported_at: Option<OffsetDateTime>,
    /// When the row was last published in a notification.
    #[sea_orm(nullable)]
    pub last_notified_at: Option<OffsetDateTime>,
    /// The last write by a change or a report.
    pub updated_at: OffsetDateTime,
}

/// Relations of the `activation_state` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
