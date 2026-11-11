// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-sdk:p1
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
//! # The shape of it
//!
//! 1. [`SettingsActivationClient::subscribe`] registers the gear for exact keys
//!    at **one tenant** and returns a [`SubscriptionHandle`] at once. Every
//!    other outcome is a per-key [`KeyState`] on the handle: `registered` with
//!    the fresh value to materialize from, `pending` (the SDK retries it while
//!    the handle lives), or `refused` (terminal).
//! 2. When a change affects a subscribed key at that tenant, the handler's
//!    [`SettingChangeHandler::on_change`] receives a [`ChangeBundle`]: the keys
//!    that changed, each **re-read fresh** by the SDK, with the `change_ref`
//!    that names the change.
//! 3. The handler applies the values its own way and returns one
//!    [`ActivationOutcome`] per key; the SDK reports them. A consumer whose
//!    application completes later calls [`SettingsActivationClient::report_outcome`]
//!    itself, naming the same `change_ref`.
//!
//! # The obligation is mutual
//!
//! A consumer that subscribes takes on a duty to answer **on every delivery that
//! carries a `change_ref`, a repeat of one it already handled included**. Delivery
//! is at-least-once and repeats until the consumer reports, so reacting must be
//! idempotent — re-reading the effective value and converging to it already is —
//! and a repeat is never suppressed: suppressing it withholds its report and
//! leaves the setting unconfirmed. The bundle that a *pending registration*
//! delivers carries no `change_ref` and owes no report.
//!
//! # What never travels
//!
//! A notification carries identifiers only. The consumer re-reads under its own
//! identity; a `secret`-trait value reads as a [`crate::SecretHandle`], and its
//! plaintext is reported only as a salted hash that the SDK computes
//! ([`ActivationOutcome::SecretSuccess`]).
//!
//! The `detail` of a failed outcome is written into an append-only audit table:
//! it must be free of setting values and secrets.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::SettingKey;
use crate::canonical::{EncodingError, canonical_json, secret_hash};
use crate::models::EffectiveValueResponse;

/// One key of a [`ChangeNotification`], with the change that owes a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifiedKey {
    /// The setting key.
    pub key: String,
    /// The change the consumer reports on: the id of the change's audit record.
    pub change_ref: Uuid,
}

/// The body of the broker event that signals a change to a subscriber: the
/// `data` of a `change_notification`.
///
/// **Identifiers only.** The tenant, the subscriber and the keys with their
/// `change_ref`; there is no field for a value, an expected value, a hash, a
/// secret or an actor, and none may be added: the consumer re-reads under its
/// own identity and reports what it applied. Unknown attributes are ignored on
/// receipt, so an additive attribute is harmless to an older consumer.
///
/// The field names are the wire's — snake case, as the event type's schema
/// registers them — and are shared by the publishing service and the consuming
/// SDK through this one definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeNotification {
    /// The subscription tenant, a bare lowercase UUID string; the root tenant's
    /// id for a platform-wide subscription.
    pub tenant: String,
    /// The subscriber the notification is for: the broker delivers by topic,
    /// type and tenant only, so a consumer drops what names another.
    pub subscriber: String,
    /// The subscriber's `awaiting` keys at that tenant, in key order.
    pub changes: Vec<NotifiedKey>,
    /// The subject's type, for a subject-scoped change. Absent in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    /// The subject's runtime id, for a subject-scoped change. Absent in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
}

/// One setting of a [`ChangeBundle`]: its fresh value, and the change to report on.
#[derive(Debug, Clone)]
pub struct BundledKey {
    /// The setting.
    pub key: SettingKey,
    /// The change this delivery reports on; absent for the bundle a pending
    /// registration delivers, which owes no report.
    pub change_ref: Option<Uuid>,
    /// The effective value at the bundle's tenant, **re-read fresh** by the SDK
    /// after the notification arrived — a [`crate::SecretHandle`] for a
    /// `secret`-trait setting.
    pub value: EffectiveValueResponse,
}

/// What a handler receives: the settings whose effective value changed at one
/// tenant, with their fresh values.
///
/// A notified key whose fresh re-read failed — or answered `Retired` or
/// `NotFound` — is **left out**: it reports nothing and is delivered again.
#[derive(Debug, Clone)]
pub struct ChangeBundle {
    /// The tenant the values are for, a bare lowercase UUID string.
    pub tenant: String,
    /// The changed settings.
    pub changes: Vec<BundledKey>,
}

/// What a consumer did with one changed setting.
///
/// The applied value is encoded by the SDK before it is reported — RFC 8785
/// canonical JSON, or for a `secret`-trait setting the salted hash of the
/// plaintext, computed in the consumer's process — so the service compares it
/// with what it recorded when the change committed, byte for byte. A success
/// carrying a different value is recorded as a failure.
pub enum ActivationOutcome {
    /// The consumer applied this value.
    Success {
        /// The value actually applied, echoed so the service can verify it. For
        /// a number: an integer within ±2^53 or a decimal binary64 holds exactly.
        applied_value: serde_json::Value,
    },
    /// The consumer applied this secret. The SDK hashes it; the plaintext is
    /// never reported, logged or stored.
    SecretSuccess {
        /// The plaintext the consumer applied: what `resolve_secret` returned
        /// for the bundle's handle, the declaration's empty default included.
        plaintext: SecretString,
    },
    /// The consumer could not apply the value.
    Failed {
        /// Why — free of setting values and secrets, which the audit table
        /// keeps forever and cannot redact. Cut to 1 KiB by the service.
        detail: String,
    },
}

/// Never prints a value, a plaintext or a detail: an outcome is the one place a
/// log line could pick one up by accident.
impl fmt::Debug for ActivationOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Success { .. } => "ActivationOutcome::Success { <redacted> }",
            Self::SecretSuccess { .. } => "ActivationOutcome::SecretSuccess { <redacted> }",
            Self::Failed { .. } => "ActivationOutcome::Failed { <redacted> }",
        })
    }
}

/// Whether a report says the consumer applied the value or failed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    /// The consumer applied a value.
    Success,
    /// The consumer could not.
    Failed,
}

impl ReportStatus {
    /// The wire spelling: `success` or `failed`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }
}

/// An [`ActivationOutcome`] as a report carries it: the status, the encoded
/// applied value and the detail.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedOutcome {
    /// Applied or failed.
    pub status: ReportStatus,
    /// The applied value in the encoding of the expected value; absent on a
    /// failure. Never logged.
    pub applied_value: Option<String>,
    /// The failure detail; absent on a success.
    pub detail: Option<String>,
}

impl fmt::Debug for EncodedOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodedOutcome")
            .field("status", &self.status)
            .field(
                "applied_value",
                &self.applied_value.as_ref().map(|_| "<redacted>"),
            )
            .field("detail", &self.detail.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ActivationOutcome {
    /// Encode this outcome for `key` at `tenant`, as a report carries it.
    ///
    /// # Errors
    /// [`EncodingError`] for a `tenant` that is not a UUID, or an applied value
    /// that is not canonical — a number outside the binary64-lossless domain.
    pub fn encode(&self, key: &SettingKey, tenant: &str) -> Result<EncodedOutcome, EncodingError> {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-7
        // The same encoder and the same secret hash the service applies to the
        // expected value, one implementation: agreement is by construction, and
        // the shared corpus in the tests pins it.
        let salt = || {
            Uuid::parse_str(tenant)
                .map(|id| id.as_hyphenated().to_string())
                .map_err(|_| EncodingError::new(EncodingError::TENANT, "the tenant is not a UUID"))
        };
        match self {
            Self::Success { applied_value } => Ok(EncodedOutcome {
                status: ReportStatus::Success,
                applied_value: Some(canonical_json(applied_value)?),
                detail: None,
            }),
            Self::SecretSuccess { plaintext } => Ok(EncodedOutcome {
                status: ReportStatus::Success,
                applied_value: Some(secret_hash(key.as_str(), &salt()?, plaintext)?),
                detail: None,
            }),
            Self::Failed { detail } => Ok(EncodedOutcome {
                status: ReportStatus::Failed,
                applied_value: None,
                detail: Some(detail.clone()),
            }),
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-7
    }
}

/// What `report_outcome` answers. Not an error either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportResult {
    /// The report settled the setting's activation: it is `succeeded` or
    /// `failed` and an `activation` audit record was written.
    Applied,
    /// The report changed nothing: no subscription or state, one already
    /// settled, a stale `change_ref`, or a state settled or replaced
    /// concurrently. Neither the SDK nor the consumer retries it.
    Ignored,
    /// The service holds no subscription of this process's instance for the key
    /// at the tenant — forgotten, evicted by the registration cap, deleted out
    /// from under the process, or its declaration purged. Nothing was written.
    /// The report is not retried; the SDK registers the key again while a live
    /// handle of the gear holds it, and the next change is expected from the
    /// re-registered instance.
    NotSubscribed,
}

/// Why a key is still `pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReason {
    /// The first pass over the handle's keys has not reached this one yet: the
    /// state `subscribe` returns every key in.
    Registering,
    /// The key is refused `NotFound` before registering: undeclared (its owner
    /// may not have booted yet), hidden from the subscriber, or a tenant outside
    /// the subscriber's subtree — all answer alike. Nothing is registered.
    NotFound,
    /// The registration could not be had now: the key's mutation lock was not
    /// acquired within its budget, the authorization service could not decide,
    /// or the database failed before the commit. Nothing is registered.
    Unavailable,
    /// The registration committed and is kept; the fresh read after it failed.
    FreshReadFailed,
    /// The registration committed and is kept; the broker refused the gear's
    /// consumer group or its join for a missing `define` or `consume` grant, so
    /// delivery waits until an operator grants it.
    BrokerGrantMissing,
    /// A report was answered `not_subscribed`: the service no longer holds this
    /// process's subscription to the key. The SDK registers it again with
    /// backoff while a live handle holds the key; the fresh read and a bundle
    /// without `change_ref` follow. A notification carrying the key meanwhile
    /// is still re-read and handed to the handler, without `change_ref`.
    NotRegistered,
}

/// Why a key is `refused`, terminally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedReason {
    /// The subscriber may not read the key at that tenant (a `403` decision).
    NotReadable,
    /// A 65th tenant interest of the gear in the process: the broker's limit,
    /// checked before registering, so nothing is registered.
    InterestLimit,
}

/// One key's state on a [`SubscriptionHandle`].
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "a handle holds a handful of these and a caller matches on `Some(value)` directly; boxing the one large variant would only tax every consumer"
)]
pub enum KeyState {
    /// Registered. The consumer materializes from `value`, read fresh after the
    /// registration committed — a [`crate::SecretHandle`] for a `secret`-trait
    /// setting; `None` when the key is retired (a reactivation re-arms the next
    /// change). Delivery needs a broker: without one the key stays `Registered`
    /// and the SDK retries joining while the service's delivery loop catches up.
    Registered {
        /// The fresh effective value at the handle's tenant.
        value: Option<EffectiveValueResponse>,
    },
    /// Not registered yet, or registered but not yet deliverable; the SDK
    /// retries with backoff while the handle lives and logs a warning each
    /// minute the key stays here.
    Pending {
        /// What the key waits for.
        reason: PendingReason,
    },
    /// Refused, terminally.
    Refused {
        /// Why.
        reason: RefusedReason,
    },
}

/// What `unsubscribe()` answers for one key. The SDK retries none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsubscribeResult {
    /// The key is released from the handle. The subscription and its activation
    /// state are deleted — nothing is reported for the state — unless another
    /// live handle of the gear in this process still holds the key at the
    /// handle's tenant, in which case they stay for it and nothing is deleted.
    Unsubscribed,
    /// The caller has no subscription to the key at the handle's tenant —
    /// another subscriber's included, of which nothing is deleted.
    NotFound,
    /// The key's mutation lock was not acquired within its budget, or the
    /// service could not answer: nothing is deleted, the key stays subscribed on
    /// the handle with its state and retries, and the caller may call `unsubscribe()`
    /// again.
    Unavailable,
}

/// The longest `instance_label` the SDK sends, in characters.
pub const MAX_INSTANCE_LABEL_CHARS: usize = 128;

/// Which process a registration, a report or an unsubscribe comes from.
///
/// One `instance_id` — a `UUIDv7` — is generated per process at first use, and the
/// host name is read once as the informational `instance_label`; every
/// subscribing gear of the process shares them ([`Self::process`]). A client
/// binding sends the id with every registration, report and unsubscribe and the
/// label with every registration. **The consumer passes and sees neither**:
/// `subscribe`, `report_outcome` and `unsubscribe()` keep their signatures.
///
/// [`Self::new`] exists for a binding's tests, which emulate several instances
/// of one gear in one process; production code uses [`Self::process`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceIdentity {
    instance_id: Uuid,
    instance_label: Option<String>,
}

impl InstanceIdentity {
    /// The identity of this process: generated on the first call, the same
    /// ever after.
    #[must_use]
    pub fn process() -> &'static Self {
        static PROCESS: OnceLock<InstanceIdentity> = OnceLock::new();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-34
        // Once, at first use: the id a `UUIDv7`, the host name read once and cut
        // to 128 characters, absent when unreadable.
        PROCESS.get_or_init(|| {
            let host = hostname::get()
                .ok()
                .and_then(|name| name.into_string().ok());
            Self::generate(host.as_deref())
        })
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-34
    }

    /// A fresh identity: a new `UUIDv7` and `host` as the label.
    #[must_use]
    pub fn generate(host: Option<&str>) -> Self {
        Self::new(Uuid::now_v7(), host)
    }

    /// An identity with an explicit id, for a binding's tests that emulate
    /// several instances. The label is trimmed, cut to
    /// [`MAX_INSTANCE_LABEL_CHARS`] characters, and absent when empty.
    #[must_use]
    pub fn new(instance_id: Uuid, label: Option<&str>) -> Self {
        let instance_label = label
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(|label| label.chars().take(MAX_INSTANCE_LABEL_CHARS).collect());
        Self {
            instance_id,
            instance_label,
        }
    }

    /// The process's instance id.
    #[must_use]
    pub const fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    /// The host name read once, if it was readable: informational only.
    #[must_use]
    pub fn instance_label(&self) -> Option<&str> {
        self.instance_label.as_deref()
    }
}

/// What backs a [`SubscriptionHandle`]: implemented by the binding of the
/// client (in process, or remote), never by a consumer.
#[async_trait]
pub trait SubscriptionControl: Send + Sync {
    /// Every key still on the handle, with its state.
    fn states(&self) -> BTreeMap<SettingKey, KeyState>;

    /// A counter that changes whenever a key's state does.
    fn watch(&self) -> watch::Receiver<u64>;

    /// Unsubscribe the handle's keys. See [`SubscriptionHandle::unsubscribe`].
    async fn unsubscribe(
        &self,
        ctx: &SecurityContext,
    ) -> Result<BTreeMap<SettingKey, UnsubscribeResult>, CanonicalError>;

    /// The handle was dropped: stop its retries. Unsubscribes nothing. Idempotent.
    fn close(&self);
}

/// A live subscription: the caller's view of its keys.
///
/// Live from the moment `subscribe` returns it until it is dropped or unsubscribed.
/// **Dropping it stops its retries and unsubscribes nothing** — the subscriptions
/// stay registered, and a `subscribe` the caller repeats returns a new handle
/// over them; only [`Self::unsubscribe`] deletes.
///
/// One process may hold several handles of the same gear over overlapping keys;
/// they share the subscription of a key at a tenant, which is the gear's, not the
/// handle's. A handle's `unsubscribe` releases its own keys and deletes a
/// subscription only when no other live handle of the gear in this process holds
/// that key at that tenant.
pub struct SubscriptionHandle {
    control: Arc<dyn SubscriptionControl>,
    changes: watch::Receiver<u64>,
}

impl SubscriptionHandle {
    /// A handle over `control`.
    #[must_use]
    pub fn new(control: Arc<dyn SubscriptionControl>) -> Self {
        let changes = control.watch();
        Self { control, changes }
    }

    /// Every key still on the handle with its state, as of now. A key that
    /// `unsubscribe()` answered `Unsubscribed` or `NotFound` is gone from the handle.
    #[must_use]
    pub fn states(&self) -> BTreeMap<SettingKey, KeyState> {
        self.control.states()
    }

    /// Wait until some key's state changed since the last call, then return
    /// `true`; `false` once the handle can change no more — unsubscribed (every key
    /// answered `Unsubscribed` or `NotFound`), closed, or its client stopped — so
    /// `while handle.changed().await { .. }` ends with the handle.
    pub async fn changed(&mut self) -> bool {
        self.changes.changed().await.is_ok()
    }

    /// Release the handle's keys and delete their subscriptions, each with its
    /// activation state, under the identity of `ctx`. Explicit: never done on drop.
    ///
    /// A key's subscription and state are deleted only when no other live handle
    /// of the gear in this process holds the key at the handle's tenant; while one
    /// does, the key is released from this handle alone, answered `Unsubscribed`,
    /// and nothing is deleted. Across processes the subscription is the gear's:
    /// an unsubscribe in one process removes it for every process.
    ///
    /// Per key: [`UnsubscribeResult::Unsubscribed`], [`UnsubscribeResult::NotFound`] or
    /// [`UnsubscribeResult::Unavailable`]. A key answered `Unsubscribed` or `NotFound`
    /// leaves the handle; when every key has, the handle is unsubscribed. The SDK
    /// retries none of them.
    ///
    /// # Errors
    /// A [`CanonicalError`] only when no key could be attempted.
    pub async fn unsubscribe(
        &self,
        ctx: &SecurityContext,
    ) -> Result<BTreeMap<SettingKey, UnsubscribeResult>, CanonicalError> {
        self.control.unsubscribe(ctx).await
    }
}

impl fmt::Debug for SubscriptionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubscriptionHandle")
            .field("keys", &self.control.states().len())
            .finish_non_exhaustive()
    }
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        self.control.close();
    }
}

/// What a consumer runs when settings it subscribed to change.
#[async_trait]
pub trait SettingChangeHandler: Send + Sync {
    /// React to a bundle of changed settings and account for each one.
    ///
    /// The values are fresh: the SDK re-read each key after the notification
    /// arrived. Apply them the consumer's own way and return one
    /// `(key, outcome)` per key the consumer accounts for **now**; a key left
    /// out is reported later through [`SettingsActivationClient::report_outcome`],
    /// naming the `change_ref` the bundle carried. For a bundle whose keys carry
    /// no `change_ref` — what a pending registration delivers — the outcomes
    /// are not reported: nothing is owed.
    ///
    /// The SDK runs a handle's bundles on a task of the handle's own, one at a
    /// time and in order: a slow handler delays that handle's next bundle and
    /// nothing else, but a reaction that takes long — a rebuild, a restart —
    /// should be handed to the consumer's own worker, with the key left out of
    /// the returned outcomes and reported later through
    /// [`SettingsActivationClient::report_outcome`]. The handler future is
    /// dropped at an await point when the handle is dropped or unsubscribed or the
    /// gear stops, so a long or non-idempotent apply belongs on the consumer's
    /// own worker. A handler that panics costs its bundle only: nothing is
    /// reported for it and a notification bundle is delivered again (a
    /// pending-registration bundle carries no `change_ref` and is not
    /// redelivered).
    async fn on_change(&self, bundle: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)>;
}

/// The consumer-facing activation contract.
#[async_trait]
pub trait SettingsActivationClient: Send + Sync {
    /// Subscribe to **exact** setting keys at **one tenant**.
    ///
    /// Any key the consumer can read may be subscribed; this is not tied to
    /// which gear declared it. The subscriber is the identity of `ctx`, never an
    /// argument. The call is not atomic across keys: each is registered in its
    /// own transaction.
    ///
    /// Returns the handle at once, every key `pending`; the SDK registers the
    /// keys in the background and the handle reports each key's state
    /// ([`SubscriptionHandle::states`], [`SubscriptionHandle::changed`]).
    ///
    /// # Errors
    /// Only for invalid input — empty `keys` or a `tenant` that is not a UUID —
    /// and then nothing is registered and there is no handle; and `Unavailable`
    /// once the gear has stopped, the one other error.
    async fn subscribe(
        &self,
        ctx: &SecurityContext,
        keys: Vec<SettingKey>,
        tenant: String,
        handler: Arc<dyn SettingChangeHandler>,
    ) -> Result<SubscriptionHandle, CanonicalError>;

    /// Report the outcome of reacting to one changed setting.
    ///
    /// Normally unnecessary — [`SettingChangeHandler::on_change`] returns
    /// outcomes and the SDK reports them. This exists for a consumer whose
    /// application completes after the handler returns. `change_ref` is the one
    /// the notification carried. Idempotent per subscriber, key, tenant and
    /// `change_ref`; **the SDK never retries it** — a consumer that got an error
    /// may repeat the call. The answer is `applied`, `ignored` or
    /// `not_subscribed`; on the last, for a key a live handle of the gear holds,
    /// the SDK moves the key to `pending` ([`PendingReason::NotRegistered`]) and
    /// registers it again, and the report itself is not repeated.
    ///
    /// # Errors
    /// Invalid-argument for malformed input (a tenant or `change_ref` that is
    /// not a UUID, a value that is not canonical); an error with nothing changed
    /// when the service could not record the report.
    async fn report_outcome(
        &self,
        ctx: &SecurityContext,
        change_ref: Uuid,
        tenant: String,
        key: SettingKey,
        outcome: ActivationOutcome,
    ) -> Result<ReportResult, CanonicalError>;
}

#[cfg(test)]
#[path = "activation_tests.rs"]
mod activation_tests;
