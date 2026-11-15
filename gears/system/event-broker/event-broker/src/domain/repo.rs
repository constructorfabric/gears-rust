//! Repository contracts (the `CursorRepo` of `DESIGN.md` "Module Structure", plus the routing
//! markers). Subscriptions have none: they are held in memory by the
//! `ConsumerGroupCoordinator` of the instance that owns their group.
//! Signatures only - no implementation (backed by `infra::storage::Storage`
//! for persisted entities, `domain::cluster` for ephemeral cache-backed ones
//! per the Domain Model's persisted-vs-ephemeral split). Topics and events
//! have no repo contract here: topics go through `SpecificationManager`
//! exclusively, and events go
//! straight to the resolved `event_broker_sdk::backend::Backend`
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
use event_broker_sdk::ConsumerGroupId;
use toolkit_gts::GtsInstanceId;

use crate::domain::error::DomainError;
use crate::domain::model::{ConsumerGroup, Cursor};

/// The cluster-visible trace of a subscription: markers a dispatcher is to read
/// to route a request to the instance that holds it (the read is not yet
/// implemented, tracked in #4438). Never the subscription itself - that lives
/// in the owning instance's `ConsumerGroupCoordinator`.
///
/// Two hops, because a request names either one: a subscription id resolves
/// to its group, and a group resolves to the instance that owns it
/// (`docs/DESIGN.md` "Subscription Resolution Cache").
#[async_trait]
pub trait RoutingMarkers: Send + Sync {
    async fn mark_subscription(
        &self,
        subscription_id: uuid::Uuid,
        group: &GtsInstanceId,
    ) -> Result<(), DomainError>;
    async fn unmark_subscription(&self, subscription_id: uuid::Uuid) -> Result<(), DomainError>;
    /// Records this instance as the group's owner, keyed by the group's UUID.
    async fn mark_group(&self, group: ConsumerGroupId) -> Result<(), DomainError>;
    async fn unmark_group(&self, group: ConsumerGroupId) -> Result<(), DomainError>;
}

#[async_trait]
pub trait CursorRepo: Send + Sync {
    async fn find_cursor(
        &self,
        consumer_group: &GtsInstanceId,
        topic: &GtsInstanceId,
        partition: i32,
    ) -> Result<Option<Cursor>, DomainError>;

    /// Writes every cursor in `cursors`, creating or replacing each one's
    /// position, or none of them: a failure, or the call being dropped, after
    /// some of them were written leaves no cursor changed.
    ///
    /// # Errors
    ///
    /// `NotFound` with `ConsumerGroupNotFound` when a cursor's consumer group
    /// is not registered, before anything is written; `StorageUnavailable`
    /// when the database cannot complete the write; `Forbidden` with
    /// `ScopeDenied` only on a broken invariant (the write's tenant scope
    /// refusing the cursor's own tenant).
    async fn put_cursors(&self, cursors: &[Cursor]) -> Result<(), DomainError>;
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
