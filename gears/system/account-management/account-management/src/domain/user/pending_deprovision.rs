//! Storage port for user deletions that have started but not finished.
//!
//! `UserService::delete_user` must tell other gears' principal-lifecycle
//! hooks that a user is gone — but only for a user that belonged to the
//! tenant named in the request. An `IdP` cannot always tell the two cases
//! apart afterwards: once the user is deleted, "already gone" and "never in
//! this tenant" look the same (in a dedicated realm both are a plain 404).
//! So `delete_user` decides before it deletes: it confirms the user is in
//! the tenant, records the deletion here, and only then asks the `IdP` to
//! delete. A retried DELETE that finds the user already gone finishes the
//! deletion (cleanup and hooks again) only while the record exists; with no
//! record it does nothing.

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The record of in-flight user deletions, keyed by `(tenant_id, user_id)`.
#[async_trait]
pub trait PendingDeprovisionRepo: Send + Sync {
    /// Record that deleting `user_id` from `tenant_id` has started.
    /// Idempotent: recording a deletion that is already recorded succeeds.
    ///
    /// # Errors
    ///
    /// [`DomainError`] when the store cannot be reached.
    async fn record(&self, tenant_id: Uuid, user_id: Uuid) -> Result<(), DomainError>;

    /// Whether a deletion of `user_id` from `tenant_id` is recorded.
    ///
    /// # Errors
    ///
    /// [`DomainError`] when the store cannot be reached.
    async fn is_recorded(&self, tenant_id: Uuid, user_id: Uuid) -> Result<bool, DomainError>;

    /// Forget the deletion of `user_id` from `tenant_id`, once it has
    /// finished. Idempotent: clearing an absent record succeeds.
    ///
    /// # Errors
    ///
    /// [`DomainError`] when the store cannot be reached.
    async fn clear(&self, tenant_id: Uuid, user_id: Uuid) -> Result<(), DomainError>;
}
