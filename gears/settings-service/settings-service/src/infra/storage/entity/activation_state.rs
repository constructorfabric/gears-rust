//! The `activation_state` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// What one instance's subscription is expected to apply, and what it
/// reported.
///
/// At most one row per `(subscriber, instance_id, key, tenant)`
/// (`uq_activation_state`); the table declares no primary key, so the entity
/// names the four columns of that identity as one. `change_ref` names the
/// change's audit record by value: there is no foreign key, since audit
/// records leave by retention while a state lives until its instance reports,
/// a later change replaces it, or its subscription goes.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "activation_state")]
#[secure(unrestricted)]
pub struct Model {
    /// The consumer identity that owes a report.
    #[sea_orm(primary_key, auto_increment = false)]
    pub subscriber: String,
    /// The consumer instance that owes the report.
    #[sea_orm(primary_key, auto_increment = false)]
    pub instance_id: Uuid,
    /// The setting key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant: String,
    /// The change that set the current expectation.
    pub change_ref: Uuid,
    /// When that change wrote the row: the database's `now()`, set by the
    /// change's insert-select and touched by nothing else.
    pub changed_at: OffsetDateTime,
    /// The expected value in canonical encoding, or a hash for a secret.
    pub expected_value: String,
    /// `awaiting`, `succeeded`, `failed` or `silent`.
    pub status: String,
    /// Publications of the current `change_ref` the broker accepted.
    pub deliveries: i32,
    /// When the delivery loop called the row silent.
    #[sea_orm(nullable)]
    pub silent_since: Option<OffsetDateTime>,
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
    /// The last write by a change, a report, a call of silence or a re-arm.
    pub updated_at: OffsetDateTime,
}

/// Relations of the `activation_state` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
