// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
//! Consumer activation, the state half: subscriptions, the key's mutation
//! lock, and the activation state a change records in its own transaction.
//!
//! A consuming gear subscribes per exact key and tenant. Every value change
//! then records, inside its transaction and under the key's mutation lock, one
//! `awaiting` activation state per subscription it affects: the expected value
//! at the subscription's tenant, in the canonical encoding (a salted hash for a
//! `secret`-trait setting), and the change's audit record id as `change_ref`.
//! Delivery and the consumer's report are other halves of the feature.

pub mod canonical;
pub mod notifier;
pub mod publisher;
pub mod recorder;
pub mod subscriptions;

use std::time::Duration;

use async_trait::async_trait;
use secrecy::SecretString;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

pub use canonical::{
    CanonicalError, ExpectedValue, SECRET_HASH_DOMAIN, canonical_json, plaintext_text, secret_hash,
};
pub use notifier::{
    ActivationCounts, ActivationNotifier, BuiltNotification, DueRow, NotificationGateway,
    NotificationPublisher, NotificationStore, NotifiedChange, ScanLock, Tick,
};
pub use publisher::{ActivationInput, ActivationPublisher, Qualified, RecordedChange};
pub use recorder::{ActivationRecorder, BackResponse, ReportResult};
pub use subscriptions::{
    ActivationRepository, Admission, NoActivation, ReadGate, SubscriptionManager,
};

/// The status of one activation state.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStatus {
    /// The consumer has not reported on the current expectation.
    Awaiting,
    /// The consumer reported the expected value.
    Succeeded,
    /// The consumer reported a failure, or a different value.
    Failed,
}

impl ActivationStatus {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Awaiting => "awaiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// From the stored spelling.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "awaiting" => Some(Self::Awaiting),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One consumer's registration for one setting at one tenant.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Subscription id (`UUIDv7`).
    pub id: Uuid,
    /// The consumer identity that registered it.
    pub subscriber: String,
    /// The exact setting key.
    pub key: String,
    /// The tenant, a bare lowercase UUID string.
    pub tenant: String,
    /// When it was registered.
    pub created_at: time::OffsetDateTime,
}

/// What one subscription is expected to apply, and what it reported.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationState {
    /// The consumer identity that owes a report.
    pub subscriber: String,
    /// The setting key.
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
    /// The change that set the current expectation: its audit record's id.
    pub change_ref: Uuid,
    /// The expected value, canonical or hashed.
    pub expected_value: String,
    /// Where the state stands.
    pub status: ActivationStatus,
    /// The reported value.
    pub applied_value: Option<String>,
    /// The consumer's failure detail.
    pub detail: Option<String>,
    /// When the settling report arrived.
    pub reported_at: Option<time::OffsetDateTime>,
    /// When the state was last published in a notification.
    pub last_notified_at: Option<time::OffsetDateTime>,
    /// The last write by a change or a report.
    pub updated_at: time::OffsetDateTime,
}

/// A `(subscriber, tenant)` pair a request wrote states for: the unit one
/// notification is published per.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubscriberTenant {
    /// The consumer identity.
    pub subscriber: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
}

/// The request-scoped accumulator of the pairs a request's changes wrote
/// activation states for, each once, in the order the request first touched
/// it. It travels beside the touched tenants of the cache broadcast and feeds
/// the end-of-request notification publish.
#[domain_model]
#[derive(Debug, Default, Clone)]
pub struct ActivationPairs {
    pairs: Vec<SubscriberTenant>,
}

impl ActivationPairs {
    /// An empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember the pairs a committed change wrote, keeping each once.
    pub fn remember(&mut self, written: impl IntoIterator<Item = SubscriberTenant>) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-22
        for pair in written {
            if !self.pairs.contains(&pair) {
                self.pairs.push(pair);
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-22
    }

    /// The pairs, in first-touch order.
    #[must_use]
    pub fn pairs(&self) -> &[SubscriberTenant] {
        &self.pairs
    }

    /// Whether the request wrote no state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// The subscriber of a call: the subject of the caller's security context,
/// never an argument the caller passes.
#[must_use]
pub fn subscriber_of(ctx: &SecurityContext) -> String {
    ctx.subject_id().to_string()
}

/// A tenant id as the tables hold it: a bare lowercase UUID string.
#[must_use]
pub fn tenant_text(tenant: Uuid) -> String {
    tenant.as_hyphenated().to_string()
}

/// Counters of the key's mutation lock: `settings_mutation_lock_total{result}`
/// and `settings_mutation_lock_wait_seconds`.
pub trait LockMetrics: Send + Sync {
    /// An acquisition that succeeded after `wait`.
    fn acquired(&self, wait: Duration);
    /// A wait that ran out its budget after `wait`.
    fn exhausted(&self, wait: Duration);
    /// An unlock that failed, a lock session that reconnected while the lock
    /// was held, or an acquisition the lock backend could not attempt.
    fn error(&self);
}

/// Counts nothing; the test binding.
#[domain_model]
pub struct NoLockMetrics;

impl LockMetrics for NoLockMetrics {
    fn acquired(&self, _wait: Duration) {}
    fn exhausted(&self, _wait: Duration) {}
    fn error(&self) {}
}

/// The activation instruments beyond the lock's: the delivery, the reports and
/// the two gauges the delivery loop refreshes. No label carries a key, a
/// tenant, a value or a hash.
pub trait NotifyMetrics: Send + Sync {
    /// `settings_notify_publish_to_deliver_seconds{event}`: from an event's
    /// `occurred_at` to its receipt, `event` being `cache_invalidate` or
    /// `change_notification`.
    fn publish_to_deliver(&self, event: &'static str, elapsed: Duration);
    /// `settings_notify_response_latency_seconds`: from the state's
    /// `updated_at` to the report that settled it.
    fn response_latency(&self, elapsed: Duration);
    /// `settings_notify_redeliveries_total`: one per notification the delivery
    /// loop's scan published and the broker accepted.
    fn redelivery(&self);
    /// `settings_activation_awaiting`: the `awaiting` states.
    fn awaiting(&self, count: u64);
    /// `settings_notify_subscriptions`: the registered subscriptions.
    fn subscriptions(&self, count: u64);
    /// `settings_notify_reactions_total{outcome}`: the status a report settled.
    fn reaction(&self, outcome: &'static str);
    /// `settings_notify_reports_total{result}`: `applied` or `ignored`.
    fn report(&self, result: &'static str);
}

/// Counts nothing; the test binding.
#[domain_model]
pub struct NoNotifyMetrics;

impl NotifyMetrics for NoNotifyMetrics {
    fn publish_to_deliver(&self, _event: &'static str, _elapsed: Duration) {}
    fn response_latency(&self, _elapsed: Duration) {}
    fn redelivery(&self) {}
    fn awaiting(&self, _count: u64) {}
    fn subscriptions(&self, _count: u64) {}
    fn reaction(&self, _outcome: &'static str) {}
    fn report(&self, _result: &'static str) {}
}

/// A lock held on one key.
#[async_trait]
pub trait HeldKey: Send {
    /// Release the lock and await the release. A failed release is logged and
    /// counted by the holder, never raised: the mutation it surrounded already
    /// has its outcome.
    async fn release(self: Box<Self>);
}

/// The named advisory lock of a setting key.
///
/// One key has one lock across every replica and two keys never share one; the
/// waiter polls on its own schedule, within a budget, and a wait that runs out
/// fails the mutation as `Unavailable` with nothing done.
#[async_trait]
pub trait KeyLocks: Send + Sync {
    /// Take the lock of `key`, waiting at most the lock's budget, further
    /// capped by `cap` (a batch's remaining request deadline).
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the budget is exhausted, or the lock
    /// backend cannot be reached.
    async fn acquire(
        &self,
        key: &str,
        cap: Option<Duration>,
    ) -> Result<Box<dyn HeldKey>, DomainError>;
}

/// What a plaintext fetch for an activation state is asked for.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct PlaintextRequest<'a> {
    /// The declaration whose per-setting decision gates the fetch.
    pub declaration_id: Uuid,
    /// The setting key.
    pub key: &'a str,
    /// The tenant of the change, which the `secret_use` record is about.
    pub audit_tenant: Uuid,
    /// The tenant whose Credential Store entry holds the plaintext: the
    /// change's own for a staged secret, an ancestor's for an inherited one.
    pub source_tenant: Uuid,
    /// The store reference to read.
    pub secret_ref: &'a str,
}

/// The in-process use of the Secret Manager's `resolve_plaintext` an
/// activation state needs: an inherited fallback's plaintext after a `revert`
/// or `remove_value`, a staged secret's after a batch `set` adopts it.
///
/// Under the service's own identity: the per-setting authorization decision for
/// that identity first, then the store read, then one `secret_use` record
/// attributed to it, written on its own so that it stays when the change later
/// rolls back. The plaintext comes back in the redacting, zeroizing wrapper
/// and never leaves the process.
#[async_trait]
pub trait PlaintextFetcher: Send + Sync {
    /// Fetch the plaintext once.
    ///
    /// # Errors
    /// [`DomainError`] when the decision is a denial or cannot be obtained, the
    /// store read fails, or the `secret_use` record cannot be written; the
    /// change is refused with nothing of it stored.
    async fn fetch(&self, request: PlaintextRequest<'_>) -> Result<SecretString, DomainError>;
}

#[cfg(test)]
pub mod testing;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
