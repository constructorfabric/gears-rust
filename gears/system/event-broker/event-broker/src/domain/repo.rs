//! Repository contracts (`DESIGN.md:595`'s `CursorRepo`, plus the routing
//! markers). Subscriptions have none: they are held in memory by the
//! `ConsumerGroupCoordinator` of the instance that owns their group.
//! Signatures only - no implementation (backed by `infra::storage::Storage`
//! for persisted entities, `domain::cluster` for ephemeral cache-backed ones
//! per the Domain Model's persisted-vs-ephemeral split). Topics and events
//! have no repo contract here (eb-single-process-implementation D1/D3):
//! topics go through `SpecificationManager` exclusively, and events go
//! straight to the resolved `event_broker_sdk::EventBrokerBackend`
//! (`domain::backend::BackendResolver`) - no repo-level indirection for
//! either.
//!
//! Method names are deliberately distinct across these traits
//! (`find_cursor`, `find_consumer_group`, not a shared
//! `get`) - `Storage` (`infra/storage/storage.rs`) implements all of them on
//! one struct, and identically-named methods across traits in scope on the
//! same receiver are ambiguous at the call site (would force
//! `Trait::method(&*repo, ..)` UFCS everywhere instead of `repo.method(..)`).

use async_trait::async_trait;
use toolkit_gts::GtsInstanceId;

use crate::domain::error::DomainError;
use crate::domain::model::{ConsumerGroup, Cursor};

/// The cluster-visible trace of a subscription: markers a dispatcher reads to
/// route a request to the instance that holds it. Never the subscription
/// itself - that lives in the owning instance's `ConsumerGroupCoordinator`.
///
/// Two hops, because a request names either one: a subscription id resolves
/// to its group, and a group resolves to the instance that owns it
/// (`DESIGN.md`'s `evbk.group.endpoint:{consumer_group}`).
#[async_trait]
pub trait RoutingMarkers: Send + Sync {
    async fn mark_subscription(
        &self,
        subscription_id: uuid::Uuid,
        group: &GtsInstanceId,
    ) -> Result<(), DomainError>;
    async fn unmark_subscription(&self, subscription_id: uuid::Uuid) -> Result<(), DomainError>;
    /// Records this instance as the group's owner.
    async fn mark_group(&self, group: &GtsInstanceId) -> Result<(), DomainError>;
    async fn unmark_group(&self, group: &GtsInstanceId) -> Result<(), DomainError>;
}

#[async_trait]
pub trait CursorRepo: Send + Sync {
    async fn find_cursor(
        &self,
        consumer_group: &GtsInstanceId,
        topic: &GtsInstanceId,
        partition: i32,
    ) -> Result<Option<Cursor>, DomainError>;

    async fn put_cursor(&self, cursor: &Cursor) -> Result<(), DomainError>;
}

#[async_trait]
pub trait ConsumerGroupRepo: Send + Sync {
    async fn create_consumer_group(
        &self,
        group: ConsumerGroup,
    ) -> Result<ConsumerGroup, DomainError>;
    async fn find_consumer_group(
        &self,
        id: &GtsInstanceId,
    ) -> Result<Option<ConsumerGroup>, DomainError>;
    #[toolkit_macros::temporary(
        tracking = "gears-rust#4347",
        reason = "no limit/cursor/filter params - callers must fetch every \
                  registered consumer group into memory and paginate/filter \
                  client-side; needs pagination pushdown once a real backend \
                  replaces the in-memory store"
    )]
    async fn list_consumer_groups(&self) -> Result<Vec<ConsumerGroup>, DomainError>;
    async fn delete_consumer_group(&self, id: &GtsInstanceId) -> Result<(), DomainError>;
}
