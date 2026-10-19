//! The operation idempotency store (DESIGN §3.3, §3.7 `bss_rating__operation`; feature 01 §3.2).
//!
//! A command that carries an `Idempotency-Key` (re-rate creation, policy, acceptance and hold
//! creation) settles the key in the transaction of its effect: the key scope is
//! `(tenant_id, actor, key)`, kept 7 days. A retry with the same body gets the original receipt;
//! the same key with another body is refused `409 AlreadyExists`.

use std::sync::Arc;

use async_trait::async_trait;

use serde_json::Value;
use time::{Duration, OffsetDateTime};
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::authz::Operation;
use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::ports::Clock;

/// How long a key is honoured.
pub const OPERATION_KEY_RETENTION: Duration = Duration::days(7);

/// The longest `Idempotency-Key` accepted.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;

/// A client's `Idempotency-Key`: 1–255 visible ASCII characters.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validates a key.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] for an empty, oversized or non-visible-ASCII key.
    pub fn parse(key: &str) -> Result<Self, DomainError> {
        if key.is_empty()
            || key.len() > MAX_IDEMPOTENCY_KEY_LEN
            || !key.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(DomainError::InvalidArgument {
                field: "Idempotency-Key",
                detail: "an idempotency key is 1-255 visible ASCII characters".to_owned(),
                reason: bss_rating_sdk::errors::reason::IDEMPOTENCY_KEY_REQUIRED,
            });
        }
        Ok(Self(key.to_owned()))
    }

    /// The key as the client sent it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The scope of a key: one tenant, one actor.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperationKey {
    /// The tenant the command acts in.
    pub tenant_id: Uuid,
    /// The subject that sent it.
    pub actor: Uuid,
    /// The client's key.
    pub idempotency_key: IdempotencyKey,
}

/// A settled operation.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOperation {
    /// Its key.
    pub key: OperationKey,
    /// The operation it created.
    pub operation_id: Uuid,
    /// The digest of the request it was settled with.
    pub request_digest: Sha256Hex,
    /// The receipt the original request was answered with.
    pub response: Value,
    /// When it was settled.
    pub created_at: OffsetDateTime,
}

/// What a command does with its key.
#[domain_model]
#[derive(Debug, PartialEq)]
pub enum Settlement {
    /// No live operation holds the key: run the command and record it with
    /// [`OperationKeys::record`], which consumes the pending key.
    Fresh(PendingOperation),
    /// A retry of the same request: answer with the stored receipt, change nothing.
    Replay(StoredOperation),
}

/// A key no live operation holds, bound to the digest of the request that settled it. Only
/// [`OperationKeys::settle`] makes one, only [`OperationKeys::record`] consumes it, and it cannot
/// be cloned, so a command cannot record under another digest or record one key twice.
///
/// That the command records it at all is the command's to keep: the type cannot force it, as a
/// `Settlement::Fresh(_)` arm may drop it unused. A key dropped unrecorded stays unsettled, so a
/// retry runs the command again rather than replaying a receipt that was never stored.
#[domain_model]
#[must_use = "a fresh key is recorded with the command's receipt in the command's transaction"]
#[derive(Debug, PartialEq, Eq)]
pub struct PendingOperation {
    key: OperationKey,
    request_digest: Sha256Hex,
}

impl PendingOperation {
    /// The key.
    #[must_use]
    pub fn key(&self) -> &OperationKey {
        &self.key
    }
}

/// Settles and records operation keys inside the command's transaction.
#[domain_model]
#[derive(Clone)]
pub struct OperationKeys {
    repo: Arc<dyn OperationRepo>,
    clock: Arc<dyn Clock>,
}

impl OperationKeys {
    /// A store over `repo` stamping instants with `clock`.
    #[must_use]
    pub fn new(repo: Arc<dyn OperationRepo>, clock: Arc<dyn Clock>) -> Self {
        Self { repo, clock }
    }

    /// Looks the key up in the command's transaction; `operation` is the command, whose resource
    /// a refusal names.
    ///
    /// # Errors
    ///
    /// [`DomainError::IdempotencyKeyReused`] when a live operation holds the key with another
    /// request digest; a storage failure.
    pub async fn settle(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        operation: Operation,
        key: &OperationKey,
        request_digest: &Sha256Hex,
    ) -> Result<Settlement, DomainError> {
        let fresh = || {
            Settlement::Fresh(PendingOperation {
                key: key.clone(),
                request_digest: request_digest.clone(),
            })
        };
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-lookup
        let Some(stored) = self.repo.find(tx, scope, key).await? else {
            return Ok(fresh());
        };
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-lookup
        if stored.created_at + OPERATION_KEY_RETENTION <= self.clock.now() {
            // An expired key no longer binds; the command starts over under it.
            self.repo.delete(tx, scope, key).await?;
            return Ok(fresh());
        }
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-replay
        if stored.request_digest == *request_digest {
            return Ok(Settlement::Replay(stored));
        }
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-replay
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-conflict
        Err(DomainError::IdempotencyKeyReused {
            key: key.idempotency_key.as_str().to_owned(),
            operation,
        })
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-conflict
    }

    /// Records the key [`Self::settle`] found fresh with the command's receipt, in the
    /// transaction of the command's effect.
    ///
    /// # Errors
    ///
    /// [`DomainError::Concurrent`] when a concurrent request with the key committed first (the
    /// caller retries and replays it); a storage failure.
    pub async fn record(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        pending: PendingOperation,
        operation_id: Uuid,
        response: Value,
    ) -> Result<StoredOperation, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-insert
        let stored = StoredOperation {
            key: pending.key,
            operation_id,
            request_digest: pending.request_digest,
            response,
            created_at: self.clock.now(),
        };
        self.repo.insert(tx, scope, &stored).await?;
        Ok(stored)
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-operation-key:p1:inst-ok-insert
    }
}

/// `bss_rating__operation`: settled `Idempotency-Key`s.
#[async_trait]
pub trait OperationRepo: Send + Sync {
    /// The stored operation of `key`, live or expired.
    async fn find(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &OperationKey,
    ) -> Result<Option<StoredOperation>, DomainError>;

    /// Records a settled operation; [`DomainError::Concurrent`] when recorded concurrently.
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        stored: &StoredOperation,
    ) -> Result<(), DomainError>;

    /// Deletes the (expired) operation of `key`.
    async fn delete(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &OperationKey,
    ) -> Result<(), DomainError>;
}

#[cfg(test)]
#[path = "operation_tests.rs"]
mod tests;
