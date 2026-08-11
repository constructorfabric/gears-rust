//! Repository contracts (`DESIGN.md:595`'s `EventRepo`, `TopicRepo`,
//! `SubscriptionRepo`, `CursorRepo`). Signatures only - no implementation
//! (backed by `infra::storage` for persisted entities, `domain::cluster`
//! for ephemeral cache-backed ones per the Domain Model's
//! persisted-vs-ephemeral split).

use async_trait::async_trait;

use crate::domain::error::DomainError;
use crate::domain::model::{Cursor, Event, Subscription, Topic};

#[async_trait]
pub trait TopicRepo: Send + Sync {
    async fn get(&self, id: &str) -> Result<Option<Topic>, DomainError>;
}

#[async_trait]
pub trait EventRepo: Send + Sync {
    async fn append(
        &self,
        topic: &Topic,
        partition: i32,
        events: &[Event],
    ) -> Result<(), DomainError>;

    async fn query(
        &self,
        topic: &Topic,
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
        topic: &str,
        partition: i32,
    ) -> Result<Option<Cursor>, DomainError>;

    async fn put(&self, cursor: &Cursor) -> Result<(), DomainError>;
}
