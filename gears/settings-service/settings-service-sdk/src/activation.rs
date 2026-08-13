// Created: 2026-08-13 by Constructor Tech
//! The consumer activation contract.
//!
//! The reader trait in [`crate::api`] is **pull**: a consumer calls it when it
//! needs a value. That covers a setting read per request, and not much else.
//!
//! This module covers the other case: a consumer that has already
//! *materialized* a setting — a connection pool sized by it, a socket bound
//! from it, a config file rendered from it — and will never re-read it unless
//! something says it changed.
//!
//! # The obligation is mutual
//!
//! Delivery is **acknowledged**. A notification is redelivered until the
//! consumer accounts for every key in it, and the originating apply does not
//! settle until that account arrives — an administrator watching an apply is
//! waiting on these outcomes. So a consumer that subscribes takes on a duty to
//! answer, and [`SettingChangeHandler::on_change`] returns the outcomes rather
//! than offering a separate call it could forget to make.
//!
//! Delivery is also **at-least-once**: the same `(apply_id, key)` may arrive
//! more than once, so reacting must be idempotent. Re-reading the effective
//! value and converging to it already is.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::SettingKey;

/// A change signal for settings this consumer subscribed to.
///
/// Carries **identifiers only** — never a value, never a secret. The consumer
/// re-reads under its own identity, which is what keeps plaintext out of the
/// signal stream rather than relying on the stream being private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingChangeNotification {
    /// The apply this notification belongs to. Every outcome reported for it is
    /// accounted against this apply.
    pub apply_id: String,

    /// The tenant whose values changed; absent means platform-wide.
    ///
    /// The notification carries the tenant precisely because a subscription
    /// does not: a consumer watching a key is notified of that key's change in
    /// any tenant, and reads which one from here.
    pub tenant: Option<String>,

    /// Only this subscriber's **own** subscribed keys that changed — never the
    /// full apply, so a subscriber cannot learn what changed elsewhere.
    pub changed_keys: Vec<SettingKey>,
}

/// What a consumer did with one changed setting.
// `rename_all` renames the *variants*; the fields inside them need
// `rename_all_fields`, or this enum would be the one model emitting snake_case.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "status"
)]
pub enum ActivationOutcome {
    /// The consumer applied the new value.
    Success {
        /// The setting that was applied.
        key: SettingKey,
        /// The value actually applied, echoed so the service can verify it
        /// against the value snapshotted at apply time — a success carrying a
        /// value that does not match is treated as a failure.
        ///
        /// For a secret-valued setting this is a **hash**: the plaintext never
        /// enters the signal stream, in either direction.
        applied_value: serde_json::Value,
    },

    /// The consumer could not apply the new value.
    Failed {
        /// The setting that failed.
        key: SettingKey,
        /// Why it failed.
        detail: String,
    },
}

impl ActivationOutcome {
    /// The setting this outcome accounts for, whichever way it went.
    ///
    /// The service accounts per key, so an outcome that could not be attributed
    /// would leave its await-record open indefinitely.
    #[must_use]
    pub const fn key(&self) -> &SettingKey {
        match self {
            Self::Success { key, .. } | Self::Failed { key, .. } => key,
        }
    }
}

/// What a consumer runs when a setting it subscribed to changes.
#[async_trait]
pub trait SettingChangeHandler: Send + Sync {
    /// React to a batch of changed settings and account for each one.
    ///
    /// The consumer re-reads the changed keys — the notification carries no
    /// values — applies them its own way, and returns one outcome per key it
    /// was notified about. Returning fewer leaves the missing keys unconfirmed
    /// and the notification eligible for redelivery.
    async fn on_change(&self, notification: SettingChangeNotification) -> Vec<ActivationOutcome>;
}

/// A live subscription.
///
/// Held by the consumer for as long as it wants delivery. Dropping it does not
/// retract the durable subscription — that survives a restart by design, and
/// re-subscribing re-publishes anything missed while the consumer was down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionHandle {
    /// Server-assigned identifier for this subscription.
    pub id: String,
}

/// The consumer-facing activation contract.
#[async_trait]
pub trait SettingsActivationClient: Send + Sync {
    /// Subscribe to change notifications for **exact** setting keys.
    ///
    /// Exact keys only — no namespace or prefix subscription. Any key the
    /// consumer can read may be subscribed; this is not tied to which gear
    /// declared the setting.
    ///
    /// No scope is supplied: a subscription has no tenant dimension, so a
    /// subscriber to a key is notified of that key's change in **any** tenant,
    /// and reads which one from the notification.
    async fn subscribe(
        &self,
        ctx: &SecurityContext,
        keys: Vec<SettingKey>,
        handler: std::sync::Arc<dyn SettingChangeHandler>,
    ) -> Result<SubscriptionHandle, CanonicalError>;

    /// Report the outcome of reacting to one changed setting.
    ///
    /// Normally unnecessary — [`SettingChangeHandler::on_change`] returns
    /// outcomes and the SDK emits them. This exists for a consumer whose
    /// re-application completes after the handler returns, such as one that
    /// hands the change to a worker and learns the result later.
    async fn report_outcome(
        &self,
        ctx: &SecurityContext,
        apply_id: String,
        tenant: Option<String>,
        outcome: ActivationOutcome,
    ) -> Result<(), CanonicalError>;
}

#[cfg(test)]
#[path = "activation_tests.rs"]
mod activation_tests;
