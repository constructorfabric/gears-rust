//! Repository contracts (`DESIGN.md:595`'s `EventRepo`, `TopicRepo`,
//! `SubscriptionRepo`, `CursorRepo`). Signatures only - no implementation
//! (backed by `infra::storage` for persisted entities, `domain::cluster`
//! for ephemeral cache-backed ones per the Domain Model's
//! persisted-vs-ephemeral split).

use async_trait::async_trait;
use gts::GtsInstanceId;
use types_registry_sdk::GtsInstance;

use crate::domain::error::DomainError;
use crate::domain::model::{Cursor, Event, Subscription};

/// A topic is an instance of the topic base type, so what a lookup yields is that
/// instance: its description and retention are properties of the document, and how
/// many partitions the broker gives it is the broker's own configuration. The
/// shape the REST API reports is a projection of the instance, applied where a
/// topic crosses the API boundary and nowhere inside the domain.
#[async_trait]
pub trait TopicRepo: Send + Sync {
    async fn get(&self, id: &GtsInstanceId) -> Result<Option<GtsInstance>, DomainError>;
}

#[async_trait]
pub trait EventRepo: Send + Sync {
    async fn append(
        &self,
        topic: &GtsInstance,
        partition: i32,
        events: &[Event],
    ) -> Result<(), DomainError>;

    async fn query(
        &self,
        topic: &GtsInstance,
        partition: i32,
        offset: i64,
        limit: i32,
    ) -> Result<Vec<Event>, DomainError>;
}

#[async_trait]
pub trait SubscriptionRepo: Send + Sync {
    async fn get(&self, id: uuid::Uuid) -> Result<Option<Subscription>, DomainError>;
    async fn put(&self, subscription: &Subscription) -> Result<(), DomainError>;
    async fn delete(&self, id: uuid::Uuid) -> Result<(), DomainError>;
}

#[async_trait]
pub trait CursorRepo: Send + Sync {
    async fn get(
        &self,
        consumer_group: &str,
        topic: &GtsInstanceId,
        partition: i32,
    ) -> Result<Option<Cursor>, DomainError>;

    async fn put(&self, cursor: &Cursor) -> Result<(), DomainError>;
}
