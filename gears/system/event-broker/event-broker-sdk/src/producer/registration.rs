//! Durable managed producer registrations with UTC bindings on every SQL backend.

use chrono::Utc;
#[cfg(feature = "outbox")]
use sea_orm::ActiveValue;
use sea_orm::entity::prelude::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
#[cfg(feature = "outbox")]
use toolkit_db::secure::secure_insert;
use toolkit_db::secure::{
    AccessScope, ScopableEntity, ScopeError, SecureEntityExt, SecureUpdateExt,
};

use crate::api::ProducerMode;
use crate::error::EventBrokerError;
use crate::ids::ProducerId;

use super::types::ManagedDeduplication;

/// Longest registration key and queue name a row holds (`VARCHAR(255)`).
pub(crate) const MAX_NAME_LEN: usize = 255;

/// One producer id per `(registration key, queue, outbox partition)`: each id
/// carries exactly one sequence space, that partition's `OutboxMessage.seq`
/// (ADR-0004 "Producer Partitions").
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "event_broker_producer_registrations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub registration_key: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub queue: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub outbox_partition: i64,
    pub producer_id: uuid::Uuid,
    pub mode: String,
    pub client_agent: String,
    pub generation: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl ScopableEntity for Entity {
    const IS_UNRESTRICTED: bool = true;

    fn tenant_col() -> Option<Self::Column> {
        None
    }

    fn resource_col() -> Option<Self::Column> {
        None
    }

    fn owner_col() -> Option<Self::Column> {
        None
    }

    fn type_col() -> Option<Self::Column> {
        None
    }

    fn resolve_property(_property: &str) -> Option<Self::Column> {
        None
    }
    fn scope_columns() -> Vec<Self::Column> {
        Vec::new()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProducerRegistration {
    pub(crate) slot: RegistrationSlot,
    pub(crate) producer_id: ProducerId,
    pub(crate) mode: ProducerMode,
    pub(crate) client_agent: String,
    pub(crate) generation: i64,
}

/// Which outbox partition of which queue of which registration key a producer
/// id serves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RegistrationSlot {
    pub(crate) key: String,
    pub(crate) queue: String,
    /// The outbox partition as toolkit-db delivers it (`OutboxMessage.partition_id`):
    /// the partition row's id, stable and unique per partition.
    pub(crate) partition: i64,
}

impl RegistrationSlot {
    fn filter(&self) -> sea_orm::Condition {
        sea_orm::Condition::all()
            .add(Column::RegistrationKey.eq(self.key.as_str()))
            .add(Column::Queue.eq(self.queue.as_str()))
            .add(Column::OutboxPartition.eq(self.partition))
    }
}

/// A new registration's identity, before it is stored.
#[cfg(feature = "outbox")]
pub(crate) struct NewRegistration<'a> {
    pub(crate) slot: &'a RegistrationSlot,
    pub(crate) producer_id: ProducerId,
    pub(crate) mode: ProducerMode,
    pub(crate) client_agent: &'a str,
}

#[derive(Clone)]
pub(crate) struct ProducerRegistrationStore {
    db: toolkit_db::Db,
}

impl ProducerRegistrationStore {
    pub(crate) fn new(db: toolkit_db::Db) -> Self {
        Self { db }
    }

    #[cfg(feature = "outbox")]
    pub(crate) async fn load(
        &self,
        slot: &RegistrationSlot,
    ) -> Result<Option<ProducerRegistration>, EventBrokerError> {
        let conn = self.db.conn().map_err(map_db_err)?;
        let row = Entity::find()
            .filter(slot.filter())
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await
            .map_err(map_scope_err)?;
        row.map(ProducerRegistration::try_from).transpose()
    }

    /// Every partition's registration stored under `key`, across queues.
    pub(crate) async fn load_all(
        &self,
        key: &str,
    ) -> Result<Vec<ProducerRegistration>, EventBrokerError> {
        let conn = self.db.conn().map_err(map_db_err)?;
        Entity::find()
            .filter(Column::RegistrationKey.eq(key))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await
            .map_err(map_scope_err)?
            .into_iter()
            .map(ProducerRegistration::try_from)
            .collect()
    }

    #[cfg(feature = "outbox")]
    pub(crate) async fn insert_new(
        &self,
        new: NewRegistration<'_>,
    ) -> Result<ProducerRegistration, EventBrokerError> {
        let now = Utc::now();
        let registration = ProducerRegistration {
            slot: new.slot.clone(),
            producer_id: new.producer_id,
            mode: new.mode,
            client_agent: new.client_agent.to_owned(),
            generation: 1,
        };
        let am = ActiveModel {
            registration_key: ActiveValue::Set(registration.slot.key.clone()),
            queue: ActiveValue::Set(registration.slot.queue.clone()),
            outbox_partition: ActiveValue::Set(registration.slot.partition),
            producer_id: ActiveValue::Set(registration.producer_id.0),
            mode: ActiveValue::Set(mode_to_str(registration.mode).to_owned()),
            client_agent: ActiveValue::Set(registration.client_agent.clone()),
            generation: ActiveValue::Set(registration.generation),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
        };
        let conn = self.db.conn().map_err(map_db_err)?;
        secure_insert::<Entity>(am, &AccessScope::allow_all(), &conn)
            .await
            .map_err(map_scope_err)?;
        Ok(registration)
    }

    pub(crate) async fn replace_with_new_generation(
        &self,
        current: &ProducerRegistration,
        producer_id: ProducerId,
    ) -> Result<ProducerRegistration, EventBrokerError> {
        let generation = current.generation + 1;
        let now = Utc::now();
        let conn = self.db.conn().map_err(map_db_err)?;
        let result = Entity::update_many()
            .col_expr(
                Column::ProducerId,
                sea_orm::sea_query::Expr::value(producer_id.0),
            )
            .col_expr(
                Column::Generation,
                sea_orm::sea_query::Expr::value(generation),
            )
            .col_expr(Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
            .filter(current.slot.filter())
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .map_err(map_scope_err)?;
        if result.rows_affected != 1 {
            return Err(EventBrokerError::Internal(format!(
                "producer registration '{}' queue '{}' partition {} was not updated",
                current.slot.key, current.slot.queue, current.slot.partition
            )));
        }
        Ok(ProducerRegistration {
            slot: current.slot.clone(),
            producer_id,
            mode: current.mode,
            client_agent: current.client_agent.clone(),
            generation,
        })
    }
}

impl ProducerRegistration {
    pub(crate) fn validate_matches(
        &self,
        managed: &ManagedDeduplication,
        client_agent: &str,
    ) -> Result<(), EventBrokerError> {
        if self.mode != managed.mode {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: format!(
                    "managed producer registration '{}' has mode {}, expected {}",
                    self.slot.key,
                    mode_to_str(self.mode),
                    mode_to_str(managed.mode)
                ),
            });
        }
        if self.client_agent != client_agent {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: format!(
                    "managed producer registration '{}' has client_agent '{}', expected '{}'",
                    self.slot.key, self.client_agent, client_agent
                ),
            });
        }
        Ok(())
    }
}

impl TryFrom<Model> for ProducerRegistration {
    type Error = EventBrokerError;

    fn try_from(model: Model) -> Result<Self, Self::Error> {
        let partition = model.outbox_partition;
        Ok(Self {
            slot: RegistrationSlot {
                key: model.registration_key,
                queue: model.queue,
                partition,
            },
            producer_id: ProducerId(model.producer_id),
            mode: mode_from_str(&model.mode)?,
            client_agent: model.client_agent,
            generation: model.generation,
        })
    }
}

pub(crate) fn mode_to_str(mode: ProducerMode) -> &'static str {
    match mode {
        ProducerMode::Stateless => "stateless",
        ProducerMode::Monotonic => "monotonic",
        ProducerMode::Chained => "chained",
    }
}

fn mode_from_str(mode: &str) -> Result<ProducerMode, EventBrokerError> {
    match mode {
        "stateless" => Ok(ProducerMode::Stateless),
        "monotonic" => Ok(ProducerMode::Monotonic),
        "chained" => Ok(ProducerMode::Chained),
        other => Err(EventBrokerError::Internal(format!(
            "unknown persisted producer mode '{other}'"
        ))),
    }
}

fn map_db_err(err: toolkit_db::DbError) -> EventBrokerError {
    EventBrokerError::Internal(format!("producer registration db access: {err}"))
}

fn map_scope_err(err: ScopeError) -> EventBrokerError {
    EventBrokerError::Internal(format!("producer registration store: {err}"))
}

#[cfg(test)]
#[path = "registration_tests.rs"]
mod tests;
