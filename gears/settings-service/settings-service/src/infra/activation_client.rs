// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-sdk:p1
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-observability-sdk:p1
//! The in-process `SettingsActivationClient`.
//!
//! Bound into the `ClientHub` beside the reader client. `subscribe` hands back
//! its handle at once, every key `pending`, and a background task per handle
//! walks the keys: the registration read decision and the registration under
//! the key's mutation lock (`register_subscription`), then the fresh read that
//! the consumer materializes from. Whatever cannot be done yet — a key not
//! declared until a later boot of its owner, a lock wait that ran out, an
//! authorization service that cannot decide, a database error, a read that
//! failed after the commit — leaves the key `pending` and is retried with
//! backoff while the handle lives, with a warning each minute.
//!
//! Receiving is the gear's one anonymous consumer group per process
//! ([`super::activation_group`]); this module owns what the consumer sees: the
//! handle, its per-key states, `retire()` and `report_outcome`.
//!
//! What a handle's handler is handed — the bundle a pending registration
//! delivers and the bundles the group re-reads for a notification — runs on the
//! handle's own task, one bundle at a time, so a slow handler delays neither
//! another handle nor the group's rejoin, and one handle's bundles keep their
//! order.
//!
//! Nothing here logs or labels a setting value, a hash or a secret: a state
//! names a key and a reason, and an outcome prints as `<redacted>`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::FutureExt as _;
use settings_service_sdk::api::SettingsReaderClient;
use settings_service_sdk::models::{EffectiveValueResponse, GetEffectiveRequest};
use settings_service_sdk::{
    ActivationOutcome, BundledKey, ChangeBundle, KeyState, PendingReason, RefusedReason,
    ReportResult, RetireResult, SettingChangeHandler, SettingKey, SettingsActivationClient,
    SettingsError, SubscriptionControl, SubscriptionHandle,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use tracing::{debug, warn};
use uuid::Uuid;

use super::activation_group::{AttachRefused, GroupRegistry, Reactor};
use super::event_broker::{Backoff, or_cancelled};
use super::mutation_lock::LOCK_EXHAUSTED_DETAIL;
use crate::domain::activation::{BackResponse, NotifyMetrics, subscriber_of, tenant_text};
use crate::domain::error::DomainError;
use crate::field;

/// The warning a key that stays `pending` is logged with: once each minute.
const WARN_EVERY: Duration = Duration::from_mins(1);

/// Why a registration did not commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// The registration read decision is a denial: terminal.
    Denied,
    /// The key is not declared, hidden from the subscriber or the tenant lies
    /// outside its subtree: the three answer alike; retried.
    NotFound,
    /// The decision cannot be had, the key's lock wait ran out or the database
    /// failed: nothing is registered; retried. Carries a short non-sensitive
    /// reason code for the once-a-minute warning: `unavailable:<dependency>`,
    /// `lock_exhausted` or `internal`.
    Unavailable(String),
}

/// Why a retire did not delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireFailure {
    /// The caller has no subscription to the key at the tenant.
    NotFound,
    /// The lock wait ran out, or the service could not answer.
    Unavailable,
}

/// The settings service as the client reaches it: the Subscription Manager's
/// `register_subscription` and `retire_subscription`, and the Activation
/// Outcome Recorder's `receive_response`. In process these are direct calls; a
/// remote binding implements the same three.
#[async_trait]
pub trait ActivationBackend: Send + Sync {
    /// Register the caller's subscription to `key` at `tenant`, idempotent.
    async fn register(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<(), RegisterError>;

    /// Delete the caller's subscription to `key` at `tenant` with its state.
    async fn retire(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<(), RetireFailure>;

    /// Record a report.
    async fn report(
        &self,
        ctx: &SecurityContext,
        response: BackResponse,
    ) -> Result<crate::domain::activation::ReportResult, DomainError>;
}

/// The waits of the client. Production values come from [`Self::standard`];
/// tests pass milliseconds.
#[derive(Debug, Clone)]
pub struct ClientTiming {
    /// The schedule of a `pending` key's retries and of the group's broker
    /// waits: 1 s doubling to a 30 s cap with equal jitter. Not configurable.
    pub backoff: Backoff,
    /// How often a key that stays `pending` is logged at `warn`.
    pub warn_every: Duration,
    /// How long leaving and deleting the group may take in all.
    pub cleanup_deadline: Duration,
    /// How often a quiet stream is interrupted to look the broker up again.
    pub liveness: Duration,
    /// How long a stream stays open before it counts as healthy.
    pub healthy: Duration,
}

impl ClientTiming {
    /// The production schedule.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            backoff: Backoff::standard(),
            warn_every: WARN_EVERY,
            cleanup_deadline: super::cache_invalidation::CLEANUP_DEADLINE,
            liveness: super::cache_invalidation::LIVENESS_PERIOD,
            healthy: super::cache_invalidation::HEALTHY_STREAM,
        }
    }
}

/// What the handles and the group share: the service, the reader, the metrics,
/// the hub the broker is looked up in and the waits.
pub struct ConsumerDeps {
    /// The service's registration, retire and report.
    pub backend: Arc<dyn ActivationBackend>,
    /// The reader every fresh read goes through.
    pub reader: Arc<dyn SettingsReaderClient>,
    /// `settings_notify_publish_to_deliver_seconds{event="change_notification"}`.
    pub metrics: Arc<dyn NotifyMetrics>,
    /// Where the Event Broker is looked up, on every wait.
    pub hub: Arc<ClientHub>,
    /// The waits.
    pub timing: ClientTiming,
}

impl ConsumerDeps {
    /// Read `key` at `tenant` through the fresh read: `get_effective` with the
    /// freshness option, one store read, under `ctx`.
    pub(crate) async fn read_fresh(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-fresh-read:p1:inst-ca-fresh-5
        // `get_effective`, once per key: the bulk read has no freshness option
        // and is never used for a fresh read.
        self.reader
            .get_effective(
                ctx,
                GetEffectiveRequest::new(key.clone(), format!("/tenants/{tenant}")).fresh(),
            )
            .await
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-fresh-read:p1:inst-ca-fresh-5
    }

    /// Report one outcome: encode its applied value as the service encodes the
    /// expected value, then hand it to the Recorder in process. Never retried.
    pub(crate) async fn report(
        &self,
        ctx: &SecurityContext,
        change_ref: Uuid,
        tenant: &str,
        key: &SettingKey,
        outcome: &ActivationOutcome,
    ) -> Result<ReportResult, CanonicalError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-2
        // On both entry paths — the handler's returned outcomes and a direct
        // `report_outcome` — before the Recorder is called.
        let encoded = outcome.encode(key, tenant).map_err(|e| {
            CanonicalError::from(DomainError::Validation {
                field: e.field().to_owned(),
                code: field::VALIDATION,
                message: e.to_string(),
            })
        })?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-2
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-3
        // A direct call in process; a remote binding reaches `receive_response`
        // through its own transport. Never a REST route or a broker event.
        let answer = self
            .backend
            .report(
                ctx,
                BackResponse {
                    change_ref: change_ref.to_string(),
                    tenant: tenant.to_owned(),
                    key: key.as_str().to_owned(),
                    status: encoded.status.as_str().to_owned(),
                    applied_value: encoded.applied_value,
                    detail: encoded.detail,
                },
            )
            .await
            .map_err(CanonicalError::from)?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-3
        Ok(match answer {
            crate::domain::activation::ReportResult::Applied => ReportResult::Applied,
            crate::domain::activation::ReportResult::Ignored => ReportResult::Ignored,
        })
    }
}

/// One key on a handle.
struct KeyEntry {
    state: KeyState,
    /// The registration committed and is kept: a retry resumes at the read.
    committed: bool,
    /// How many times the walker stepped this key.
    attempts: u32,
    /// Since when the key has been `pending`; absent while it is not.
    pending_since: Option<Instant>,
    /// When the key was last logged at `warn`.
    last_warned: Option<Instant>,
    /// The reason code of the failure that left the key `pending`, when the
    /// backend gave one: named in the warning, never a value.
    code: Option<String>,
    /// What the key was `registered` with, held while a missing broker grant
    /// moves it to `pending`, restored when the join succeeds.
    held: Option<HeldValue>,
}

/// What a key was `registered` with, set aside while the broker grant is missing.
struct HeldValue(Option<EffectiveValueResponse>);

impl KeyEntry {
    fn new(state: KeyState, now: Instant) -> Self {
        let pending_since = matches!(state, KeyState::Pending { .. }).then_some(now);
        Self {
            state,
            committed: false,
            attempts: 0,
            pending_since,
            last_warned: None,
            code: None,
            held: None,
        }
    }

    /// Move the key to `state`: a `pending` key keeps the time it has been
    /// pending since, any other state clears it.
    fn move_to(&mut self, state: KeyState, code: Option<String>) {
        if matches!(state, KeyState::Pending { .. }) {
            self.pending_since.get_or_insert_with(Instant::now);
        } else {
            self.pending_since = None;
            self.last_warned = None;
        }
        self.state = state;
        self.code = code;
    }

    /// Whether the walker has work for this key: `pending` for a reason the
    /// walker retries (a missing grant is the group's to retry).
    const fn walkable(&self) -> bool {
        matches!(
            self.state,
            KeyState::Pending { reason }
                if !matches!(reason, PendingReason::BrokerGrantMissing)
        )
    }
}

// @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-15
/// What one step of one key came to: the key's state and whether the SDK
/// retries it.
enum Step {
    /// Nothing more to do now: `registered`, or `refused`.
    Settled,
    /// A retry will resume where this stopped.
    Retry,
    /// Registered on a retry: the value the handler is handed, owing no report.
    Delivered(Box<BundledKey>),
}
// @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-15

/// A subscription handle's state: its keys, their retries and the handler.
pub(crate) struct HandleCore {
    id: Uuid,
    subscriber: String,
    ctx: SecurityContext,
    tenant: Uuid,
    handler: Arc<dyn SettingChangeHandler>,
    deps: Arc<ConsumerDeps>,
    groups: Arc<GroupRegistry>,
    entries: Mutex<BTreeMap<SettingKey, KeyEntry>>,
    /// The change counter the handle's `changed()` waits on. Dropped when the
    /// handle closes, which ends every `changed()` with `false`.
    version: Mutex<Option<watch::Sender<u64>>>,
    /// Stops the walker, the delivery task and the warning ticker when the
    /// handle is dropped or retired, or the client stops: a child of the
    /// registry's stop token.
    stop: CancellationToken,
    closed: AtomicBool,
    /// Whether the broker grants the group: a registered key is `pending` while
    /// not. Read and written only with `entries` locked, so a key moving to
    /// `registered` and the grant changing are never interleaved: whichever
    /// takes the lock second sees the other's effect.
    granted: AtomicBool,
    /// The bundles awaiting the handler, in order.
    /// Unbounded: it grows by one bundle per redelivery while a handler is stuck.
    queue: UnboundedSender<ChangeBundle>,
    /// Serialises a walker step with `retire()`, so a retire never races the
    /// registration of the key it deletes.
    op: tokio::sync::Mutex<()>,
}

impl HandleCore {
    fn entries(&self) -> MutexGuard<'_, BTreeMap<SettingKey, KeyEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn announce(&self) {
        if let Some(version) = self
            .version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            version.send_modify(|v| *v += 1);
        }
    }

    /// Move a key to `state`, as the per-key state rules say.
    fn set_state(&self, key: &SettingKey, state: KeyState) {
        self.set_state_with(key, state, None);
    }

    /// Move a key to `state`, naming the reason code of the failure behind it.
    fn set_state_with(&self, key: &SettingKey, state: KeyState, code: Option<String>) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-7
        {
            let mut entries = self.entries();
            let Some(entry) = entries.get_mut(key) else {
                return;
            };
            entry.move_to(state, code);
        }
        self.announce();
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-7
    }

    fn pending_keys(&self) -> Vec<SettingKey> {
        self.entries()
            .iter()
            .filter(|(_, e)| e.walkable())
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// The registration read decision, the registration and the fresh read of
    /// one key, and the state each outcome leaves it in.
    async fn step(&self, key: &SettingKey) -> Step {
        let committed = {
            let mut entries = self.entries();
            let Some(entry) = entries.get_mut(key) else {
                return Step::Settled;
            };
            entry.attempts += 1;
            entry.committed
        };
        if !committed {
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-8
            // One key at a time, each under its own lock in its own
            // transaction: the call is not atomic across keys.
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-13
            let registered = self
                .deps
                .backend
                .register(&self.ctx, key, self.tenant)
                .await;
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-13
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-8
            match registered {
                Ok(()) => {
                    if let Some(entry) = self.entries().get_mut(key) {
                        entry.committed = true;
                    }
                }
                Err(failure) => return self.registration_failed(key, failure),
            }
        }
        self.read_registered(key).await
    }

    /// The state a registration that did not commit leaves its key in.
    fn registration_failed(&self, key: &SettingKey, failure: RegisterError) -> Step {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-1
        match failure {
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-11
            // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-3
            RegisterError::Denied => {
                warn!(key = %key, "subscription refused: the subscriber may not read the key at the tenant");
                self.set_state(
                    key,
                    KeyState::Refused {
                        reason: RefusedReason::NotReadable,
                    },
                );
                Step::Settled
            }
            // @cpt-end:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-3
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-11
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-6
            RegisterError::NotFound => {
                self.set_state(
                    key,
                    KeyState::Pending {
                        reason: PendingReason::NotFound,
                    },
                );
                Step::Retry
            }
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-6
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-7
            RegisterError::Unavailable(code) => {
                self.set_state_with(
                    key,
                    KeyState::Pending {
                        reason: PendingReason::Unavailable,
                    },
                    Some(code),
                );
                Step::Retry
            } // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-7
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-1
    }

    /// The fresh read after a registration committed, and the state it leaves.
    async fn read_registered(&self, key: &SettingKey) -> Step {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-20
        let read = self.deps.read_fresh(&self.ctx, key, self.tenant).await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-20
        let attempts = self.entries().get(key).map_or(0, |e| e.attempts);
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-21
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-22
        match read {
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-3
            // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-1
            Ok(value) => {
                let bundled = (attempts > 1).then(|| {
                    Box::new(BundledKey {
                        key: key.clone(),
                        change_ref: None,
                        value: value.clone(),
                    })
                });
                self.registered(key, Some(value));
                bundled.map_or(Step::Settled, Step::Delivered)
            }
            // @cpt-end:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-1
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-3
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-4
            Err(error)
                if matches!(
                    SettingsError::from(error.clone()),
                    SettingsError::Retired { .. }
                ) =>
            {
                // The subscription is kept and the value absent: a reactivation
                // re-arms the next change.
                self.registered(key, None);
                Step::Settled
            }
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-4
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-8
            Err(_) => {
                // The registration is kept; the retry resumes at the read.
                self.set_state(
                    key,
                    KeyState::Pending {
                        reason: PendingReason::FreshReadFailed,
                    },
                );
                Step::Retry
            } // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-8
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-22
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-21
    }

    /// Mark a key `registered` with `value` — or `pending` for the broker grant
    /// while the group is refused one, the value held for when it succeeds.
    fn registered(&self, key: &SettingKey, value: Option<EffectiveValueResponse>) {
        {
            // One critical section with `broker_grant`: the grant is read and
            // the state written under the entries lock, so a grant that changes
            // between the two cannot leave a key `registered` that the group's
            // grant notification already passed over, nor `pending` for a grant
            // that was restored in between.
            let mut entries = self.entries();
            let Some(entry) = entries.get_mut(key) else {
                return;
            };
            if self.granted.load(Ordering::SeqCst) {
                entry.move_to(KeyState::Registered { value }, None);
            } else {
                entry.held = Some(HeldValue(value));
                entry.move_to(
                    KeyState::Pending {
                        reason: PendingReason::BrokerGrantMissing,
                    },
                    None,
                );
            }
        }
        self.announce();
    }

    /// Log, at `warn` once each `warn_every`, the keys that stay `pending`.
    fn warn_pending(&self) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-27
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-14
        let every = self.deps.timing.warn_every;
        let now = Instant::now();
        let mut entries = self.entries();
        for (key, entry) in entries.iter_mut() {
            let (KeyState::Pending { reason }, Some(since)) = (&entry.state, entry.pending_since)
            else {
                continue;
            };
            let from = entry.last_warned.unwrap_or(since);
            if now.duration_since(from) >= every {
                let reason = *reason;
                entry.last_warned = Some(now);
                warn!(
                    key = %key,
                    reason = ?reason,
                    code = entry.code.as_deref().unwrap_or("none"),
                    "subscription still pending; retrying"
                );
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-14
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-27
    }

    /// The warning of a key that stays `pending`, whatever the reason: the
    /// walker only runs for the keys it retries, and a key waiting for the
    /// broker grant is the group's to retry, so a ticker covers them all.
    async fn tick_warnings(self: Arc<Self>) {
        let period = (self.deps.timing.warn_every / 4).max(Duration::from_millis(1));
        while or_cancelled(&self.stop, tokio::time::sleep(period))
            .await
            .is_some()
        {
            self.warn_pending();
        }
    }

    /// The background work of a handle: walk the keys, retry what is pending
    /// with backoff, until nothing is or the handle is dropped or retired.
    async fn walk(self: Arc<Self>) {
        let mut pause = self.deps.timing.backoff.clone();
        loop {
            let mut delivered = Vec::new();
            for key in self.pending_keys() {
                if self.stop.is_cancelled() {
                    return;
                }
                let step = {
                    let _serial = self.op.lock().await;
                    // A retire that ran while this key waited removed it.
                    if !self.entries().contains_key(&key) {
                        continue;
                    }
                    self.step(&key).await
                };
                if let Step::Delivered(bundled) = step {
                    delivered.push(*bundled);
                }
            }
            if !delivered.is_empty() {
                self.deliver_registered(delivered);
            }
            if self.pending_keys().is_empty() {
                return;
            }
            self.warn_pending();
            // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-2
            // The retryable outcome repeats: wait, and try again.
            if or_cancelled(&self.stop, tokio::time::sleep(pause.next()))
                .await
                .is_none()
            {
                return;
            }
            // @cpt-end:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-2
        }
    }

    /// A pending registration committed on a retry: the handler gets the fresh
    /// values as a bundle without `change_ref`, which owes no report. The
    /// bundle goes through the handle's own queue like every other, so the
    /// walk neither waits for the handler nor dies with it.
    fn deliver_registered(&self, delivered: Vec<BundledKey>) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-28
        // The outcomes the handler returns are dropped: nothing is owed, so
        // nothing is reported.
        self.enqueue(ChangeBundle {
            tenant: tenant_text(self.tenant),
            changes: delivered,
        });
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-28
    }

    fn enqueue(&self, bundle: ChangeBundle) {
        // A closed handle's worker is gone: the bundle is moot.
        if self.queue.send(bundle).is_err() {
            debug!("a closed handle takes no bundle");
        }
    }

    /// The handle's delivery task: its bundles one at a time, in the order they
    /// arrived, each handed to the handler on this task and nowhere else.
    async fn deliver_loop(self: Arc<Self>, mut inbox: UnboundedReceiver<ChangeBundle>) {
        loop {
            let next = or_cancelled(&self.stop, inbox.recv()).await.flatten();
            let Some(bundle) = next else {
                return;
            };
            if or_cancelled(&self.stop, self.run_delivery(bundle))
                .await
                .is_none()
            {
                return;
            }
        }
    }

    /// Hand the handler one bundle and report the outcomes it returns, for the
    /// keys that carry a `change_ref`.
    async fn run_delivery(&self, bundle: ChangeBundle) {
        // A handle closed while the bundle waited is not handed it.
        if self.stop.is_cancelled() {
            return;
        }
        let refs: BTreeMap<SettingKey, Uuid> = bundle
            .changes
            .iter()
            .filter_map(|c| c.change_ref.map(|r| (c.key.clone(), r)))
            .collect();
        let tenant = bundle.tenant.clone();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-9
        // The bundle is `data.tenant` and each re-read key with its
        // `change_ref`, and the fresh values — a `SecretHandle` for a
        // `secret`-trait key. A handler that panics costs this delivery, not
        // the handle's task or the group.
        let outcomes = std::panic::AssertUnwindSafe(self.handler.on_change(bundle))
            .catch_unwind()
            .await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-9
        let Ok(outcomes) = outcomes else {
            warn!("a change handler panicked; its bundle reports nothing");
            return;
        };
        if refs.is_empty() {
            return;
        }
        self.report_outcomes(outcomes, &refs, &tenant).await;
    }

    /// Report each outcome the handler returned for a key of its bundle.
    async fn report_outcomes(
        &self,
        outcomes: Vec<(SettingKey, ActivationOutcome)>,
        refs: &BTreeMap<SettingKey, Uuid>,
        tenant: &str,
    ) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-10
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-11
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-12
        // Each outcome the handler returned is reported with the `change_ref`
        // the notification named, its applied value encoded by the SDK. A key it
        // returns none for is reported later through `report_outcome`. A repeat
        // of a notification already handled takes this same path — re-read,
        // re-apply, re-report — and is never suppressed.
        for (key, outcome) in outcomes {
            let Some(change_ref) = refs.get(&key) else {
                debug!(key = %key, "an outcome for a key outside the bundle was not reported");
                continue;
            };
            if let Err(error) = self
                .deps
                .report(&self.ctx, *change_ref, tenant, &key, &outcome)
                .await
            {
                warn!(
                    key = %key,
                    kind = ?SettingsError::from(error),
                    "an outcome could not be reported; the SDK does not retry it"
                );
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-12
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-11
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-10
    }

    /// The handle is no longer live: stop its retries and leave its group.
    fn close_handle(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-29
        // Retries stop; no subscription is touched, and a `subscribe` the
        // caller repeats returns a new handle over the same subscriptions.
        self.stop.cancel();
        // The handle can change no more: every `changed()` ends with `false`.
        self.version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-29
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-30
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-11
        // The gear's last live handle, dropped or retired, takes the process's
        // consumer group with it: left and deleted best-effort.
        self.groups.release(&self.subscriber, self.id);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-11
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-30
    }
}

#[async_trait]
impl SubscriptionControl for HandleCore {
    fn states(&self) -> BTreeMap<SettingKey, KeyState> {
        self.entries()
            .iter()
            .map(|(k, e)| (k.clone(), e.state.clone()))
            .collect()
    }

    fn watch(&self) -> watch::Receiver<u64> {
        self.version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            // A closed handle hands out a receiver that is already ended.
            .map_or_else(|| watch::channel(0).1, watch::Sender::subscribe)
    }

    async fn retire(
        &self,
        ctx: &SecurityContext,
    ) -> Result<BTreeMap<SettingKey, RetireResult>, CanonicalError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-1
        // Explicit: the one call that deletes. Dropping a handle never does.
        let _serial = self.op.lock().await;
        let keys: Vec<SettingKey> = self.entries().keys().cloned().collect();
        let mut answers = BTreeMap::new();
        for key in keys {
            let answer = match self.deps.backend.retire(ctx, &key, self.tenant).await {
                Ok(()) => RetireResult::Retired,
                Err(RetireFailure::NotFound) => RetireResult::NotFound,
                Err(RetireFailure::Unavailable) => RetireResult::Unavailable,
            };
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-2
            // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-6
            // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-7
            // A key answered `retired` or `404` leaves the handle — its retries
            // with it; one answered `Unavailable` keeps its state and retries,
            // and the caller may call `retire()` again.
            if answer != RetireResult::Unavailable {
                self.entries().remove(&key);
            }
            // @cpt-end:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-7
            // @cpt-end:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-6
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-2
            answers.insert(key, answer);
        }
        self.announce();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-10
        // Every key answered `retired` or `404`: the handle is retired and no
        // longer live; otherwise it stays live for the keys answered `Unavailable`.
        if self.entries().is_empty() {
            self.close_handle();
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-10
        Ok(answers)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-1
    }

    fn close(&self) {
        self.close_handle();
    }
}

#[async_trait]
impl Reactor for HandleCore {
    fn id(&self) -> Uuid {
        self.id
    }

    fn tenant(&self) -> Uuid {
        self.tenant
    }

    fn ctx(&self) -> SecurityContext {
        self.ctx.clone()
    }

    fn is_live(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    fn holds(&self) -> Vec<SettingKey> {
        self.entries()
            .iter()
            .filter(|(_, e)| !matches!(e.state, KeyState::Refused { .. }))
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn enqueue(&self, bundle: ChangeBundle) {
        HandleCore::enqueue(self, bundle);
    }

    fn broker_grant(&self, granted: bool) {
        let mut changed = false;
        {
            let mut entries = self.entries();
            // Stored with the entries locked: see `granted`.
            self.granted.store(granted, Ordering::SeqCst);
            for (key, entry) in entries.iter_mut() {
                match (&entry.state, granted) {
                    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-26
                    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-9
                    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-4
                    (KeyState::Registered { value }, false) => {
                        // The subscription is kept; delivery waits for the grant.
                        entry.held = Some(HeldValue(value.clone()));
                        entry.state = KeyState::Pending {
                            reason: PendingReason::BrokerGrantMissing,
                        };
                        entry.pending_since = Some(Instant::now());
                        entry.last_warned = None;
                        entry.code = None;
                        changed = true;
                        debug!(key = %key, "key pending: the broker grant is missing");
                    }
                    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-4
                    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-9
                    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-26
                    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-10
                    // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-5
                    (
                        KeyState::Pending {
                            reason: PendingReason::BrokerGrantMissing,
                        },
                        true,
                    ) => {
                        // The join retried after the grant succeeded: the
                        // subscription was kept, so the key is as it was.
                        let value = entry.held.take().and_then(|held| held.0);
                        entry.state = KeyState::Registered { value };
                        entry.pending_since = None;
                        entry.last_warned = None;
                        changed = true;
                    }
                    // @cpt-end:cpt-cf-settings-service-state-consumer-activation-handle-key:p1:inst-ca-hk-5
                    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-10
                    _ => {}
                }
            }
        }
        if changed {
            self.announce();
        }
    }
}

/// The in-process `SettingsActivationClient`. See the module documentation.
pub struct ActivationClient {
    deps: Arc<ConsumerDeps>,
    groups: Arc<GroupRegistry>,
}

impl ActivationClient {
    /// A client over the service, the reader and the hub the broker is looked
    /// up in.
    #[must_use]
    pub fn new(deps: ConsumerDeps) -> Self {
        let deps = Arc::new(deps);
        let groups = Arc::new(GroupRegistry::new(Arc::clone(&deps)));
        Self { deps, groups }
    }

    /// The groups the client holds, for the gear's stop and for tests.
    #[must_use]
    pub fn groups(&self) -> &Arc<GroupRegistry> {
        &self.groups
    }

    /// Leave and delete the process's consumer groups best-effort, within the
    /// cleanup deadline, and refuse further subscriptions. Called on the gear's
    /// `stop`.
    pub async fn stop(&self) {
        self.groups.stop().await;
    }
}

/// The short, non-sensitive reason code of a failed registration, for the
/// once-a-minute warning: `lock_exhausted`, `unavailable:<dependency>` or
/// `internal`. The dependency is the name the error was built with
/// (`the <dependency> could not <operation>`), reduced to `[a-z0-9_]`; nothing
/// of the dependency's own error text is in it.
fn failure_code(error: &DomainError) -> String {
    match error {
        DomainError::Unavailable { detail } if detail == LOCK_EXHAUSTED_DETAIL => {
            "lock_exhausted".to_owned()
        }
        DomainError::Unavailable { detail } => {
            let dependency = detail
                .strip_prefix("the ")
                .and_then(|rest| rest.split_once(" could not "))
                .map_or("unknown", |(dependency, _)| dependency);
            let slug: String = dependency
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("unavailable:{slug}")
        }
        DomainError::Internal { .. } => "internal".to_owned(),
        _ => "other".to_owned(),
    }
}

/// An invalid-input error for `subscribe`.
fn invalid_input(field_name: &str, message: &str) -> CanonicalError {
    CanonicalError::from(DomainError::Validation {
        field: field_name.to_owned(),
        code: field::VALIDATION,
        message: message.to_owned(),
    })
}

#[async_trait]
impl SettingsActivationClient for ActivationClient {
    async fn subscribe(
        &self,
        ctx: &SecurityContext,
        keys: Vec<SettingKey>,
        tenant: String,
        handler: Arc<dyn SettingChangeHandler>,
    ) -> Result<SubscriptionHandle, CanonicalError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-1
        // The subscriber is the caller's: the subject of `ctx`, never an argument.
        let subscriber = subscriber_of(ctx);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-1
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-2
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-2
        if keys.is_empty() {
            return Err(invalid_input("keys", "`keys` is empty"));
        }
        let Ok(tenant) = Uuid::parse_str(&tenant) else {
            return Err(invalid_input("tenant", "`tenant` is not a UUID"));
        };
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-2
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-2
        let now = Instant::now();
        let entries: BTreeMap<SettingKey, KeyEntry> = keys
            .into_iter()
            .map(|key| {
                (
                    key,
                    KeyEntry::new(
                        KeyState::Pending {
                            reason: PendingReason::Registering,
                        },
                        now,
                    ),
                )
            })
            .collect();
        let (queue, inbox) = unbounded_channel();
        let core = Arc::new(HandleCore {
            id: Uuid::new_v4(),
            subscriber,
            ctx: ctx.clone(),
            tenant,
            handler,
            deps: Arc::clone(&self.deps),
            groups: Arc::clone(&self.groups),
            entries: Mutex::new(entries),
            version: Mutex::new(Some(watch::channel(0).0)),
            stop: self.groups.stop_token().child_token(),
            closed: AtomicBool::new(false),
            granted: AtomicBool::new(true),
            queue,
            op: tokio::sync::Mutex::new(()),
        });
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-4
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-23
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-24
        // The interest is added, and the process's group ensured, before any
        // registration: a 65th tenant interest is refused here, with nothing
        // registered; the group joins with one interest per tenant.
        let weak: std::sync::Weak<dyn Reactor> =
            Arc::downgrade(&core) as std::sync::Weak<dyn Reactor>;
        let attached = self.groups.attach(&core.subscriber, ctx, tenant, &weak);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-24
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-23
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-2
        if let Err(AttachRefused::Stopped) = attached {
            return Err(CanonicalError::from(DomainError::Unavailable {
                detail: "the settings client is stopping".to_owned(),
            }));
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-2
        if let Err(refusal) = attached {
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-12
            warn!(tenant = %tenant, reason = %refusal, "subscription refused: no consumer group can take the tenant");
            let keys: Vec<SettingKey> = core.entries().keys().cloned().collect();
            for key in keys {
                core.set_state(
                    &key,
                    KeyState::Refused {
                        reason: RefusedReason::InterestLimit,
                    },
                );
            }
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-12
            return Ok(SubscriptionHandle::new(
                core as Arc<dyn SubscriptionControl>,
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-4
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-6
        // For as long as the handle is live; every key is `pending` until the
        // first pass says otherwise.
        tokio::spawn(Arc::clone(&core).walk());
        tokio::spawn(Arc::clone(&core).deliver_loop(inbox));
        tokio::spawn(Arc::clone(&core).tick_warnings());
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-6
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-5
        // The handle at once, every key `pending`.
        Ok(SubscriptionHandle::new(
            core as Arc<dyn SubscriptionControl>,
        ))
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-5
    }

    async fn report_outcome(
        &self,
        ctx: &SecurityContext,
        change_ref: Uuid,
        tenant: String,
        key: SettingKey,
        outcome: ActivationOutcome,
    ) -> Result<ReportResult, CanonicalError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-1
        // Directly, here; or through the outcomes a handler returned, which the
        // consumer reports through the same call. Never retried by the SDK.
        self.deps
            .report(ctx, change_ref, &tenant, &key, &outcome)
            .await
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-report:p1:inst-ca-report-1
    }
}

/// The settings service as the in-process client reaches it: the subscription
/// service and the Recorder, over the concrete repositories.
pub struct ServiceBackend<D, V, A, R, AR> {
    subscriptions: Arc<crate::infra::subscriptions::SubscriptionService<D, V, A, R>>,
    recorder: Arc<crate::domain::activation::ActivationRecorder<R, AR>>,
}

impl<D, V, A, R, AR> ServiceBackend<D, V, A, R, AR> {
    /// Over this subscription service and Recorder.
    #[must_use]
    pub fn new(
        subscriptions: Arc<crate::infra::subscriptions::SubscriptionService<D, V, A, R>>,
        recorder: Arc<crate::domain::activation::ActivationRecorder<R, AR>>,
    ) -> Self {
        Self {
            subscriptions,
            recorder,
        }
    }
}

#[async_trait]
impl<D, V, A, R, AR> ActivationBackend for ServiceBackend<D, V, A, R, AR>
where
    D: crate::domain::declaration::DeclarationRepository + 'static,
    V: crate::domain::value::ValueRepository + Clone + 'static,
    A: crate::domain::access::AccessRepository + 'static,
    R: crate::domain::activation::ActivationRepository + 'static,
    AR: crate::audit::AuditSink + 'static,
{
    async fn register(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<(), RegisterError> {
        match self
            .subscriptions
            .register_subscription(ctx, key, tenant)
            .await
        {
            Ok(_) => Ok(()),
            Err(DomainError::Unauthorized { .. }) => Err(RegisterError::Denied),
            Err(DomainError::NotFound { .. }) => Err(RegisterError::NotFound),
            Err(error) => {
                debug!(key = %key, error = %error.wire_message(), "registration did not commit");
                Err(RegisterError::Unavailable(failure_code(&error)))
            }
        }
    }

    async fn retire(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<(), RetireFailure> {
        match self
            .subscriptions
            .retire_subscription(ctx, key, tenant)
            .await
        {
            Ok(()) => Ok(()),
            Err(DomainError::NotFound { .. }) => Err(RetireFailure::NotFound),
            Err(error) => {
                debug!(key = %key, error = %error.wire_message(), "retire did not complete");
                Err(RetireFailure::Unavailable)
            }
        }
    }

    async fn report(
        &self,
        ctx: &SecurityContext,
        response: BackResponse,
    ) -> Result<crate::domain::activation::ReportResult, DomainError> {
        self.recorder.receive_response(ctx, response).await
    }
}

#[cfg(test)]
#[path = "activation_client_tests.rs"]
mod activation_client_tests;
