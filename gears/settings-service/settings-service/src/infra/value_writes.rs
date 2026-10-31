// @cpt-dod:cpt-cf-settings-service-dod-value-writes-batch:p1
//! The write coordinator: one transaction per change, commit, evict, publish.
//!
//! The domain writer decides and commits inside a transaction it is handed;
//! this adapter owns the transaction and what follows it, and shapes the
//! request-level operations — a batch, a clone, the read-only report — out of
//! the writer's steps.

use std::sync::Arc;
use std::time::{Duration, Instant};

use secrecy::SecretString;
use serde_json::Value;
use settings_service_sdk::{EffectiveSource, SettingKey};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::activation::{
    ActivationInput, ActivationPairs, ActivationPublisher, KeyLocks, PlaintextFetcher,
    PlaintextRequest, SubscriberTenant, SubscriptionManager, plaintext_text,
};
use crate::domain::category::DomainVisibility;
use crate::domain::coherence::{ChangeOutcome, CoherencePublisher};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, ScopeTarget};
use crate::domain::secrets::pending::{
    self as pending, Claim, PendingSecret, PendingSecretRepository,
};
use crate::domain::value::ValueRepository;
use crate::domain::writes::service::{ImpactPage, ImpactReport, StepUpPolicy, ValidationReport};
use crate::domain::writes::{
    Change, Committed, Gated, StagePrecondition, Staged, ValueWriter, WriteActor,
};
use crate::field;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::pending_secret_repo::PendingSecretRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::log_text::LogSafe;

/// The writer over the concrete repositories and the audit store.
pub type ConcreteWriter =
    ValueWriter<DeclarationRepo, ValueRepo, AccessRepo, AuditStore, PendingSecretRepo>;

/// The Subscription Manager over the concrete repositories.
pub type ConcreteManager =
    SubscriptionManager<DeclarationRepo, ValueRepo, AccessRepo, ActivationRepo>;

/// The Activation Publisher over the concrete repositories.
pub type ConcretePublisher =
    ActivationPublisher<DeclarationRepo, ValueRepo, AccessRepo, ActivationRepo>;

/// The request deadline a batch's lock waits are capped by: the API gateway's,
/// thirty seconds by default. The gateway's own setting is another gear's
/// configuration, which the gear cannot read, so the figure is fixed here and
/// follows `mutation_lock_wait_seconds` in `config.rs`; a platform that
/// raises the gateway's deadline gains nothing until this constant does.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(30);

/// A test-only pause point: awaited with the key once a change has taken its
/// lock and found its subscriptions, and before its transaction opens, so a
/// test holds a write deterministically in flight between the candidate step
/// and the commit.
#[cfg(any(test, feature = "integration"))]
pub type PausePoint = Arc<
    dyn Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

/// What the write path needs to record activation state: the key's mutation
/// lock, the subscriptions of a key, the publisher that writes the states and
/// the plaintext fetch a secret's expected value needs.
#[derive(Clone)]
pub struct ActivationWiring {
    /// Qualifies a key's subscriptions under the lock.
    pub manager: Arc<ConcreteManager>,
    /// Writes the states in the change's transaction.
    pub publisher: Arc<ConcretePublisher>,
    /// The key's mutation lock.
    pub locks: Arc<dyn KeyLocks>,
    /// The plaintext fetch under the service's identity.
    pub plaintext: Arc<dyn PlaintextFetcher>,
}

/// What a change carries into the part of its commit that runs under the key's
/// lock.
struct CommitParams<'a> {
    /// The tag the caller presented.
    if_match: Option<&'a str>,
    /// The change set the request belongs to.
    change_set_id: Uuid,
    /// Where the plaintext of a secret comes from.
    plan: PlaintextPlan,
}

/// Where the plaintext a secret's expected value is hashed from comes from.
enum PlaintextPlan {
    /// The setting is not a secret: no plaintext.
    None,
    /// An inline secret `set`: the value in the request.
    Inline(Arc<SecretString>),
    /// A batch `set` adopting a staged secret: fetched by its reference.
    Adopted(String),
    /// A `revert` or `remove_value`: the fallback left in effect.
    Fallback,
}

impl PlaintextPlan {
    fn of(declaration: &Declaration, change: &Change) -> Self {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-6
        // From the request for an inline secret `set`, from a fetch for an
        // inherited fallback or an adopted staged entry, or the default's value
        // when the fallback is not a stored secret.
        if !declaration.has_secret_trait {
            return Self::None;
        }
        match change {
            Change::Set(value) => Self::Inline(Arc::new(plaintext_text(value))),
            Change::AdoptSecret { secret_ref, .. } => Self::Adopted(secret_ref.clone()),
            Change::Revert | Change::Remove => Self::Fallback,
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-6
    }
}

/// The most changes one batch may carry.
pub const BATCH_LIMIT: usize = 500;

/// The refusal a batch past [`BATCH_LIMIT`] is answered with — by the handler
/// on the deserialized list, and by the coordinator as the backstop.
#[must_use]
pub fn batch_too_large() -> DomainError {
    DomainError::Validation {
        field: "changes".to_owned(),
        code: field::VALIDATION,
        message: format!("a batch carries at most {BATCH_LIMIT} changes"),
    }
}

/// How many expired stages one sweep pass releases at most.
pub const SWEEP_LIMIT: u64 = 200;

/// The batch operation that stores a value — the default, so a client that
/// never heard of the field keeps working unchanged.
pub const OP_SET: &str = "set";

/// The batch operation that clears the scope's own override.
pub const OP_REVERT: &str = "revert";

/// One change of a batch.
///
/// The operation stays the word the request used until the entry is evaluated.
/// A batch entry stands or falls alone, so an unrecognised operation — or a
/// value that contradicts the one named — has to refuse that entry with
/// `invalid` and leave the rest of the batch to commit; parsing it here, where
/// the whole request is still one value, could only refuse all of them.
#[derive(Debug, Clone)]
pub struct BatchChange {
    /// The setting.
    pub key: SettingKey,
    /// The target tenant; absent, the caller's own.
    pub tenant: Option<Uuid>,
    /// [`OP_SET`] or [`OP_REVERT`]; absent is [`OP_SET`].
    pub op: Option<String>,
    /// The value. Carried by a set, absent from a revert.
    pub value: Option<Value>,
    /// The tag the caller last read.
    pub if_match: Option<String>,
}

/// A batch entry the surface refused on its text, before it was a change: a
/// key that is not a setting key, a number a double cannot hold. It carries
/// the key as the caller wrote it and the scope it asked for, so the refusal
/// is reported in its place and published like any other.
#[derive(Debug)]
pub struct RefusedEntry {
    /// The key exactly as submitted, parsed or not.
    pub key: String,
    /// The target tenant; absent, the caller's own.
    pub tenant: Option<Uuid>,
    /// Why the entry was refused.
    pub error: DomainError,
}

/// The change an entry asks for, before a secret's staged entry is resolved.
///
/// `set` and `revert` are the whole vocabulary: `remove` is the administrative
/// deletion of a row and is not offered here, and `AdoptSecret` is not asked
/// for by name — it is what a set of a `pending_id` becomes.
fn requested_change(op: Option<&str>, value: Option<Value>) -> Result<Change, DomainError> {
    let invalid = |field: &str, message: String| DomainError::Validation {
        field: field.to_owned(),
        code: field::VALIDATION,
        message,
    };
    match op.unwrap_or(OP_SET) {
        OP_SET => value
            .map(Change::Set)
            .ok_or_else(|| invalid("value", format!("a `{OP_SET}` change carries a value"))),
        OP_REVERT if value.is_some() => Err(invalid(
            "value",
            format!("a `{OP_REVERT}` change carries no value"),
        )),
        OP_REVERT => Ok(Change::Revert),
        other => Err(invalid(
            "op",
            format!("unknown operation `{other}`; one of `{OP_SET}` or `{OP_REVERT}`"),
        )),
    }
}

/// One batch's answer: a change set id and one outcome per change.
#[derive(Debug)]
pub struct BatchOutcome {
    /// The change set every committed change carries.
    pub change_set_id: Uuid,
    /// In request order.
    pub results: Vec<Result<Committed, DomainError>>,
    /// The subscribers and tenants the committed changes wrote activation
    /// states for, each once, in the order the batch first touched them.
    pub activation: ActivationPairs,
}

/// The coordinator.
pub struct WriteCoordinator {
    db: Arc<DBProvider<DbError>>,
    writer: Arc<ConcreteWriter>,
    pending: PendingSecretRepo,
    declarations: DeclarationRepo,
    coherence: Option<Arc<CoherencePublisher>>,
    activation: Option<ActivationWiring>,
    request_deadline: Duration,
    #[cfg(any(test, feature = "integration"))]
    pause: std::sync::Mutex<Option<PausePoint>>,
}

fn conn_error(err: &DbError) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

impl WriteCoordinator {
    /// Over this database and writer.
    #[must_use]
    pub fn new(db: Arc<DBProvider<DbError>>, writer: Arc<ConcreteWriter>) -> Self {
        Self {
            db,
            writer,
            pending: PendingSecretRepo,
            declarations: DeclarationRepo,
            coherence: None,
            activation: None,
            request_deadline: REQUEST_DEADLINE,
            #[cfg(any(test, feature = "integration"))]
            pause: std::sync::Mutex::new(None),
        }
    }

    /// Take the key's mutation lock around every value change and record its
    /// activation state in the change's transaction. Without this a change
    /// takes no lock and records no state, as on a build before consumer
    /// activation.
    #[must_use]
    pub fn with_activation(mut self, wiring: ActivationWiring) -> Self {
        self.activation = Some(wiring);
        self
    }

    /// The request deadline a batch's lock waits are capped by: a change waits
    /// for its key's lock no longer than what remains of it.
    #[must_use]
    pub fn with_request_deadline(mut self, deadline: Duration) -> Self {
        self.request_deadline = deadline;
        self
    }

    /// Hold every change at `pause` once it has taken its lock and found its
    /// subscriptions, before its transaction opens. For tests only.
    #[cfg(any(test, feature = "integration"))]
    #[must_use]
    pub fn with_pause_point(self, pause: PausePoint) -> Self {
        self.set_pause_point(Some(pause));
        self
    }

    /// Install or clear the test-only pause point of a coordinator that is
    /// already built and shared, as the gear's is.
    ///
    /// # Panics
    /// If the pause point's own mutex is poisoned.
    #[cfg(any(test, feature = "integration"))]
    #[allow(clippy::expect_used)]
    pub fn set_pause_point(&self, pause: Option<PausePoint>) {
        *self.pause.lock().expect("the pause point's mutex") = pause;
    }

    /// Publish `cache_invalidate` at the end of every request through `hook`.
    /// Without one a request ends silently, as when no broker resolves.
    #[must_use]
    pub fn with_coherence(mut self, hook: Arc<CoherencePublisher>) -> Self {
        self.coherence = Some(hook);
        self
    }

    /// The end of a batch: what its entries committed, published once.
    async fn publish_batch_end(
        &self,
        results: &[Result<Committed, DomainError>],
        change_set_id: Uuid,
        pairs: &ActivationPairs,
    ) {
        let committed: Vec<&Committed> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        self.publish_request_end(&committed, change_set_id, pairs)
            .await;
    }

    /// The end of a request: one hook call over every change it committed.
    /// The response of the request never depends on it. `pairs` are the
    /// subscribers and tenants the request wrote activation states for, which
    /// the notification half of the hook is published for.
    async fn publish_request_end(
        &self,
        committed: &[&Committed],
        change_set_id: Uuid,
        pairs: &ActivationPairs,
    ) {
        if let Some(hook) = &self.coherence {
            hook.publish_value_writes(
                committed
                    .iter()
                    .map(|c| ChangeOutcome::value_write(&c.key, c.tenant_id, &c.scope_class, true)),
                change_set_id,
                pairs,
            )
            .await;
        }
    }

    /// The writer, for the challenge parameters and the resolver.
    #[must_use]
    pub fn writer(&self) -> &Arc<ConcreteWriter> {
        &self.writer
    }

    /// Gate, commit in one transaction, then evict and publish.
    ///
    /// # Errors
    /// The first gate's refusal, or the commit's rejection; a rejection is also
    /// published as a durable failure notification.
    pub async fn change(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        change: Change,
        if_match: Option<&str>,
        operation: &'static str,
    ) -> Result<Committed, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-5
        // One change set per request, carried on the record — minted before
        // the gate, so a refusal there is published under it like a refusal
        // anywhere later.
        let change_set_id = Uuid::new_v4();
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-5
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-2
        let gated = match self
            .writer
            .gate(
                &conn,
                actor,
                key,
                requested,
                StepUpPolicy::Verify,
                operation,
            )
            .await
        {
            Ok(gated) => gated,
            Err(err) => {
                // The target was not resolved: the scope is the one asked
                // for, or the caller's own.
                let tenant_id = requested.unwrap_or_else(|| actor.ctx.subject_tenant_id());
                self.writer
                    .after_rejection(key.as_str(), tenant_id, actor, &err, change_set_id)
                    .await;
                return Err(err);
            }
        };
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-2
        self.commit(actor, gated, change, if_match, change_set_id)
            .await
    }

    /// Commit a gated change in its own transaction and run what follows. A
    /// single-change request ends here, so the cache broadcast follows the
    /// local eviction.
    ///
    /// # Errors
    /// The commit's rejection, after it was published.
    pub async fn commit(
        &self,
        actor: &WriteActor,
        gated: Gated,
        change: Change,
        if_match: Option<&str>,
        change_set_id: Uuid,
    ) -> Result<Committed, DomainError> {
        let mut pairs = ActivationPairs::new();
        let committed = self
            .commit_change(
                actor,
                gated,
                change,
                if_match,
                change_set_id,
                None,
                &mut pairs,
            )
            .await?;
        self.publish_request_end(&[&committed], committed.change_set_id, &pairs)
            .await;
        Ok(committed)
    }

    /// One change of a request: commit, evict, record. The request's own end
    /// publishes the broadcast, once for all of its changes. `cap` is what a
    /// batch has left of its request deadline, which bounds the wait for the
    /// key's mutation lock; `pairs` remembers the subscribers and tenants the
    /// change wrote activation states for.
    #[allow(clippy::too_many_arguments)]
    async fn commit_change(
        &self,
        actor: &WriteActor,
        gated: Gated,
        change: Change,
        if_match: Option<&str>,
        change_set_id: Uuid,
        cap: Option<Duration>,
        pairs: &mut ActivationPairs,
    ) -> Result<Committed, DomainError> {
        let key = gated.declaration.key.clone();
        let tenant_id = gated.tenant_id;
        // Validation and the store leg come first, outside the transaction: the
        // Credential Store cannot join it, and toolkit-db refuses a fresh
        // connection while one is open on this task. The connection here is
        // for the intent row a secret records before its entry exists.
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // What the expected value of a secret is hashed from, decided while the
        // change still holds its value: staging takes it.
        let plan = PlaintextPlan::of(&gated.declaration, &change);
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-1
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-2
        // A change that passed its gates reaches the commit; one refused by
        // validation or by its Credential Store put returns here, without
        // contending for the lock.
        let staged = match self
            .writer
            .stage(
                &conn,
                &gated,
                actor,
                change,
                StagePrecondition::Judge(if_match),
            )
            .await
        {
            Ok(staged) => staged,
            Err(err) => {
                self.writer
                    .after_rejection(&key, tenant_id, actor, &err, change_set_id)
                    .await;
                return Err(err);
            }
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-2
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-1
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-3
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-4
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-2
        // The key's lock, taken only now that the gates and the validation have
        // passed, so a refused change never contended for it. A wait that runs
        // out refuses the change `Unavailable` with nothing stored — the staged
        // entry of an inline secret is released as any refused commit's is —
        // and a batch still attempts its later changes.
        let held = match &self.activation {
            None => None,
            Some(wiring) => match wiring.locks.acquire(&key, cap).await {
                Ok(held) => Some(held),
                Err(err) => {
                    self.writer.discard(&conn, &gated, &staged).await;
                    self.writer
                        .after_rejection(&key, tenant_id, actor, &err, change_set_id)
                        .await;
                    return Err(err);
                }
            },
        };
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-2
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-4
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-3
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-7
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-6
        // The mutation runs under the lock, from its candidate step until its
        // transaction rolled back, or committed and the local cache is evicted;
        // it takes no second key's lock while it holds this one, so the locks
        // cannot deadlock.
        let outcome = self
            .commit_staged(
                actor,
                &gated,
                &staged,
                CommitParams {
                    if_match,
                    change_set_id,
                    plan,
                },
                &conn,
            )
            .await;
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-6
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-7
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-26
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-8
        // Released, and the release awaited, before the change answers: the next
        // mutation of the key acquires it without waiting, after a rollback too.
        if let Some(held) = held {
            held.release().await;
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-8
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-26
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-24
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-10
        // The mutation's own result, the lock released and the release awaited;
        // the guarantee rests on the lock, not on the isolation level.
        match outcome {
            Ok((committed, written)) => {
                // The change event and the release of a superseded secret's
                // entry come after the lock is let go: neither needs it.
                self.writer.publish_and_release(&committed, actor).await;
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-29
                pairs.remember(written);
                Ok(committed)
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-29
            }
            // Nothing of the change and no activation state is stored, and the
            // pairs it would have remembered are never remembered.
            Err(err) => {
                self.writer.discard(&conn, &gated, &staged).await;
                self.writer
                    .after_rejection(&key, tenant_id, actor, &err, change_set_id)
                    .await;
                Err(err)
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-10
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-24
    }

    /// What a change does under its key's lock: find the subscriptions it may
    /// reach, take the plaintext a secret's expectation is hashed from, then
    /// the transaction that writes the value, its record and its activation
    /// state, and the local eviction that follows the commit. The event and the
    /// release of a superseded secret's entry are the caller's, after it has
    /// released the lock.
    async fn commit_staged(
        &self,
        actor: &WriteActor,
        gated: &Gated,
        staged: &Staged,
        params: CommitParams<'_>,
        conn: &toolkit_db::secure::DbConn<'_>,
    ) -> Result<(Committed, Vec<SubscriberTenant>), DomainError> {
        let CommitParams {
            if_match,
            change_set_id,
            plan,
        } = params;
        let (qualified, plaintext) = match &self.activation {
            None => (Vec::new(), None),
            Some(wiring) => {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-5
                let qualified = wiring
                    .manager
                    .resolve_subscribers(conn, &gated.declaration, gated.tenant_id)
                    .await?;
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-5
                let plaintext = if qualified.is_empty() {
                    None
                } else {
                    self.plaintext_for(wiring, conn, gated, plan).await?
                };
                (qualified, plaintext)
            }
        };
        #[cfg(any(test, feature = "integration"))]
        {
            #[allow(clippy::expect_used)]
            let pause = self.pause.lock().expect("the pause point's mutex").clone();
            if let Some(pause) = pause {
                pause(&gated.declaration.key).await;
            }
        }
        let writer = Arc::clone(&self.writer);
        let publisher = self.activation.as_ref().map(|w| Arc::clone(&w.publisher));
        let actor_owned = actor.clone();
        let if_match = if_match.map(str::to_owned);
        let gated_owned = gated.clone();
        let staged_owned = staged.clone();
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-4
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-16
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-23
        let (committed, written) = self
            .db
            .db()
            .transaction_ref_mapped::<_, (Committed, Vec<SubscriberTenant>), DomainError>(
                move |tx| {
                    Box::pin(async move {
                        let input = publisher.as_deref().and_then(|publisher| {
                            (!qualified.is_empty()).then(|| ActivationInput {
                                publisher,
                                qualified: &qualified,
                                plaintext: plaintext.as_deref(),
                            })
                        });
                        writer
                            .commit_in_recording(
                                tx,
                                &gated_owned,
                                &staged_owned,
                                if_match.as_deref(),
                                &actor_owned,
                                change_set_id,
                                input,
                            )
                            .await
                    })
                },
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-23
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-16
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-4
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-3
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-25
        self.writer.evict(&committed);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-25
        Ok((committed, written))
    }

    /// The plaintext the expected value of a secret is hashed from, when the
    /// change qualified subscriptions: the inline value in hand, or one fetch
    /// by reference under the service's identity — a staged entry's, an
    /// inherited fallback's — before the transaction opens. A fallback that is
    /// the declaration's default is the default's value, with no fetch. A fetch
    /// that fails, for whatever reason, refuses the change `Unavailable`.
    async fn plaintext_for(
        &self,
        wiring: &ActivationWiring,
        conn: &toolkit_db::secure::DbConn<'_>,
        gated: &Gated,
        plan: PlaintextPlan,
    ) -> Result<Option<Arc<SecretString>>, DomainError> {
        let declaration = &gated.declaration;
        let refused = |err: &DomainError| {
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-13
            tracing::warn!(
                key = %declaration.key,
                err = %LogSafe(err),
                "the secret behind a change could not be read; the change is refused"
            );
            DomainError::Unavailable {
                detail: "the secret the change depends on could not be read; nothing was changed"
                    .to_owned(),
            }
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-13
        };
        match plan {
            PlaintextPlan::None => Ok(None),
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-14
            // The plaintext lives only in the redacting, zeroizing wrapper
            // until it is hashed, and is never logged.
            PlaintextPlan::Inline(plaintext) => Ok(Some(plaintext)),
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-14
            PlaintextPlan::Adopted(secret_ref) => wiring
                .plaintext
                .fetch(PlaintextRequest {
                    declaration_id: declaration.id,
                    key: &declaration.key,
                    audit_tenant: gated.tenant_id,
                    source_tenant: gated.tenant_id,
                    secret_ref: &secret_ref,
                })
                .await
                .map(|plaintext| Some(Arc::new(plaintext)))
                .map_err(|err| refused(&err)),
            PlaintextPlan::Fallback => {
                // A `revert` or `remove_value` of a scope with no override of
                // its own is the transaction's `NotFound`, which re-reads the
                // row and decides: no fallback is fetched, and no `secret_use`
                // record written, for a change that is doomed.
                if ValueRepo
                    .find_one(
                        conn,
                        &AccessScope::allow_all(),
                        declaration.id,
                        gated.tenant_id,
                    )
                    .await?
                    .is_none()
                {
                    return Ok(None);
                }
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-9
                // The value the scope falls back to once its override is gone,
                // read through the resolver's freshness form outside a
                // transaction — never from the effective-value cache.
                let key =
                    SettingKey::parse(&declaration.key).map_err(|e| DomainError::Internal {
                        diagnostic: format!("stored key `{}` does not parse: {e}", declaration.key),
                    })?;
                let target = ScopeTarget::Tenant(gated.tenant_id).normalize(gated.root);
                let effective = self
                    .writer
                    .resolver()
                    .resolve_fresh(conn, &key, target)
                    .await
                    .map_err(|err| refused(&err))?;
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-9
                match (effective.fallback_source, effective.fallback.as_str()) {
                    (EffectiveSource::Inherited, Some(secret_ref)) => {
                        let source_tenant = match &effective.fallback_scope {
                            Some(scope) => ScopeTarget::parse(scope)?.tenant_id(gated.root),
                            None => gated.root,
                        };
                        wiring
                            .plaintext
                            .fetch(PlaintextRequest {
                                declaration_id: declaration.id,
                                key: &declaration.key,
                                audit_tenant: gated.tenant_id,
                                source_tenant,
                                secret_ref,
                            })
                            .await
                            .map(|plaintext| Some(Arc::new(plaintext)))
                            .map_err(|err| refused(&err))
                    }
                    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-11
                    // The declaration's default: its value, with no fetch.
                    _ => Ok(Some(Arc::new(plaintext_text(&effective.fallback)))),
                    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-11
                }
            }
        }
    }

    /// Several changes, each standing or falling alone; step-up verified once.
    ///
    /// # Errors
    /// [`DomainError::Validation`] over the size limit;
    /// [`DomainError::StepUpRequired`] or [`DomainError::Unauthorized`] when the
    /// request-level step-up fails, nothing evaluated; a database failure.
    pub async fn batch(
        &self,
        actor: &WriteActor,
        changes: Vec<BatchChange>,
    ) -> Result<BatchOutcome, DomainError> {
        self.batch_entries(
            actor,
            changes.into_iter().map(Ok::<_, RefusedEntry>).collect(),
        )
        .await
    }

    /// [`Self::batch`] over entries the surface has already judged. An `Err`
    /// entry is one refused on its text before it was a change — a key that
    /// does not parse, a number a double cannot hold — and it is reported in
    /// its place, published under the change set like any refusal, counts
    /// against the limit, and is never gated or committed. The rest stand or
    /// fall alone as before.
    ///
    /// # Errors
    /// As [`Self::batch`].
    pub async fn batch_entries(
        &self,
        actor: &WriteActor,
        entries: Vec<Result<BatchChange, RefusedEntry>>,
    ) -> Result<BatchOutcome, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-2
        if entries.len() > BATCH_LIMIT {
            return Err(batch_too_large());
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-2
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-3
        // Once for the request: the declarations that can be read decide
        // whether a person must have re-authenticated; a key that cannot is
        // reported in its own entry below. This pass only asks early. It is not
        // what enforces the requirement: a declaration it failed to read — a
        // tenant resolver that blinked, a policy point that did not answer —
        // is missing here but gates normally in the loop, which therefore
        // decides for itself whether step-up still has to be verified.
        let root = self.writer.resolver().root_tenant().await?;
        let mut declarations: Vec<Declaration> = Vec::new();
        for change in entries.iter().filter_map(|entry| entry.as_ref().ok()) {
            if let Ok(declaration) = self
                .writer
                .declaration_for_write(&conn, actor, &change.key, root)
                .await
            {
                declarations.push(declaration);
            }
        }
        let mut verified = if ConcreteWriter::any_requires_step_up(&declarations) {
            if !actor.is_interactive() {
                return Err(DomainError::Unauthorized {
                    resource: settings_service_sdk::gts::VALUE_SCHEMA,
                });
            }
            self.writer.verify_step_up(actor, "batch").await?;
            true
        } else {
            false
        };
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-4
        let change_set_id = Uuid::new_v4();
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-4
        let mut results = Vec::with_capacity(entries.len());
        // The batch's request deadline runs from here: each change waits for its
        // key's lock no longer than what remains of it.
        let started = Instant::now();
        let mut pairs = ActivationPairs::new();
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-5
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-8
        // The changes of one request are evaluated in order, each under its own
        // lock and in its own transaction, so the last change to a key at a
        // tenant wins the row.
        for entry in entries {
            // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-6
            let change = match entry {
                Ok(change) => change,
                // Refused on the surface: published like a refusal at the gate,
                // at the scope asked for or the caller's own, so the events
                // alone still tell the whole outcome of the batch.
                Err(refused) => {
                    results.push(self.refuse_entry(actor, refused, change_set_id).await);
                    continue;
                }
            };
            let BatchChange {
                key,
                tenant,
                op,
                value,
                if_match,
            } = change;
            let gated = self
                .writer
                .gate(
                    &conn,
                    actor,
                    &key,
                    tenant,
                    StepUpPolicy::AlreadyVerified,
                    "batch",
                )
                .await;
            // The gate above trusts the up-front pass (`AlreadyVerified`), so
            // a requirement that pass could not see is verified here, once: a
            // success covers the rest of the request, a refusal is this
            // entry's and a later gated entry asks again.
            let gated = self.verify_late(actor, gated, &mut verified).await;
            let outcome = match gated {
                Ok(gated) => match self
                    .batch_change_for(&conn, &gated, actor, op.as_deref(), value)
                    .await
                {
                    Ok(change) => {
                        let remaining = self.request_deadline.saturating_sub(started.elapsed());
                        self.commit_change(
                            actor,
                            gated,
                            change,
                            if_match.as_deref(),
                            change_set_id,
                            Some(remaining),
                            &mut pairs,
                        )
                        .await
                    }
                    Err(err) => {
                        self.writer
                            .after_rejection(
                                &gated.declaration.key,
                                gated.tenant_id,
                                actor,
                                &err,
                                change_set_id,
                            )
                            .await;
                        Err(err)
                    }
                },
                // Refused at the gate: published like any other refusal, so
                // the events alone tell the whole outcome of the batch. The
                // target was not resolved, so the scope is the one asked for,
                // or the caller's own.
                Err(err) => {
                    let tenant_id = tenant.unwrap_or_else(|| actor.ctx.subject_tenant_id());
                    self.writer
                        .after_rejection(key.as_str(), tenant_id, actor, &err, change_set_id)
                        .await;
                    Err(err)
                }
            };
            // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-6
            // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-7
            results.push(outcome);
            // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-7
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-affected:p1:inst-ca-affected-8
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-5
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-8
        // Eviction and publication ran per committed change, after each
        // commit and before the next; nothing here is observable before it is
        // stored. The cache broadcast is the request's, published once after
        // the last commit: one per tenant the batch touched.
        self.publish_batch_end(&results, change_set_id, &pairs)
            .await;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-8
        Ok(BatchOutcome {
            change_set_id,
            results,
            activation: pairs,
        })
    }

    /// A gate result whose requirement the up-front pass could not see: step-up
    /// is verified here, once. A success covers the rest of the request, a
    /// refusal is this entry's and a later gated entry asks again.
    async fn verify_late(
        &self,
        actor: &WriteActor,
        gated: Result<Gated, DomainError>,
        verified: &mut bool,
    ) -> Result<Gated, DomainError> {
        match gated {
            Ok(gated) if gated.declaration.requires_step_up && !*verified => {
                self.writer.verify_step_up(actor, "batch").await.map(|()| {
                    *verified = true;
                    gated
                })
            }
            other => other,
        }
    }

    /// An entry refused on the surface, published as a refusal and handed back
    /// as its outcome.
    async fn refuse_entry(
        &self,
        actor: &WriteActor,
        refused: RefusedEntry,
        change_set_id: Uuid,
    ) -> Result<Committed, DomainError> {
        let tenant_id = refused
            .tenant
            .unwrap_or_else(|| actor.ctx.subject_tenant_id());
        self.writer
            .after_rejection(
                &refused.key,
                tenant_id,
                actor,
                &refused.error,
                change_set_id,
            )
            .await;
        Err(refused.error)
    }

    /// The change a batch entry carries: the operation it names, and for a set
    /// the value — or, for a secret setting, a `pending_id` standing in for
    /// one, resolved to the entry staged earlier. Any other shape is a value
    /// and validates as one.
    ///
    /// Every refusal here is this entry's alone: the caller records it as a
    /// rejection and goes on to the next change.
    async fn batch_change_for<C: DBRunner>(
        &self,
        conn: &C,
        gated: &Gated,
        actor: &WriteActor,
        op: Option<&str>,
        value: Option<Value>,
    ) -> Result<Change, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-10
        // A revert takes the same route a set does — the gate, the `If-Match`
        // check and the commit are one implementation — and parts from it only
        // in the change it hands on, which the shared commit already knows how
        // to apply and which audit records as a revert.
        let value = match requested_change(op, value)? {
            Change::Set(value) => value,
            // `requested_change` yields a set or a revert and nothing else.
            other => return Ok(other),
        };
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-10
        if gated.declaration.has_secret_trait
            && let Some(pending_id) = pending::pending_id_of(&value)
        {
            return self.claim_pending(conn, gated, actor, pending_id).await;
        }
        Ok(Change::Set(value))
    }

    /// Check a staged secret for a batch change: the row must exist, be
    /// unexpired, and name this change's declaration, tenant and subject.
    /// Nothing is consumed here — the commit consumes the row inside its own
    /// transaction, re-asserting the expiry — so a commit that fails leaves
    /// the token claimable and the entry, which the stage created, in place.
    async fn claim_pending<C: DBRunner>(
        &self,
        conn: &C,
        gated: &Gated,
        actor: &WriteActor,
        pending_id: Uuid,
    ) -> Result<Change, DomainError> {
        let scope = AccessScope::allow_all();
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-7
        let Some(row) = self.pending.find(conn, &scope, pending_id).await? else {
            return Err(pending::invalid_pending());
        };
        let subject = actor.subject();
        pending::check_claim(
            &row,
            &Claim {
                declaration_id: gated.declaration.id,
                tenant_id: gated.tenant_id,
                subject_id: &subject,
                now: OffsetDateTime::now_utc(),
            },
        )?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-7
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-8
        // The reference travels on to the commit with the token's row, which
        // the commit consumes; two batches racing for one token part there,
        // where only one delete can find the row.
        Ok(Change::AdoptSecret {
            secret_ref: row.secret_ref,
            pending_id,
        })
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-8
    }

    /// Stage a `secret`-trait value ahead of step-up: validate it, put the
    /// plaintext in the store under this gear's principal exactly as a set
    /// would, and hand back a token the batch names in place of the value.
    /// No step-up is asked, since nothing live changes here.
    ///
    /// # Errors
    /// The gate's refusals short of step-up; [`DomainError::Validation`] with
    /// `not_a_secret` for a declaration whose values travel inline, or for an
    /// invalid value; [`DomainError::Unavailable`] when the store or the
    /// database cannot answer, in which case nothing is kept anywhere.
    pub async fn stage_secret(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        value: Value,
    ) -> Result<PendingSecret, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // A stage is also the moment to let go of what nobody claimed; a sweep
        // that fails is logged and tried again by the next one.
        // A request is not the lifecycle: nothing here stops it but its end.
        if let Err(err) = self
            .sweep_expired_in(&conn, SWEEP_LIMIT, &CancellationToken::new())
            .await
        {
            tracing::warn!(
                err = %LogSafe(&err),
                "pending-secret sweep failed; expired stages wait for the next pass"
            );
        }
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-1
        // The value write gate with step-up skipped: authorization was decided
        // by the caller, and a service principal is still refused a setting
        // that requires a person.
        let gated = self
            .writer
            .gate(
                &conn,
                actor,
                key,
                requested,
                StepUpPolicy::AlreadyVerified,
                "stage",
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-1
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-2
        if !gated.declaration.has_secret_trait {
            return Err(DomainError::Validation {
                field: "key".to_owned(),
                code: field::NOT_A_SECRET,
                message: format!(
                    "`{key}` is not a secret setting: its values travel inline and need no staging"
                ),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-2
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-3
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-4
        // Exactly the set's own staging: validated against the type, the
        // intent row written, then into the store under a reference unique to
        // this stage. That intent row is the token the caller takes away — a
        // stage leaves nothing a set would not.
        let staged = self
            .writer
            .stage(
                &conn,
                &gated,
                actor,
                Change::Set(value),
                StagePrecondition::None,
            )
            .await?;
        let Staged::Set {
            secret_ref: Some(_),
            pending_id: Some(pending_id),
            ..
        } = &staged
        else {
            return Err(DomainError::Internal {
                diagnostic: "staging a secret produced no store reference".to_owned(),
            });
        };
        let pending_id = *pending_id;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-4
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-3
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-5
        // The stage's record; a record that cannot be written releases the
        // entry and its row, as a refused set does. The row itself was written
        // by the stage, before the entry existed.
        let key_owned = gated.declaration.key.clone();
        let tenant_id = gated.tenant_id;
        let actor_owned = actor.clone();
        let outcome = self
            .db
            .db()
            .transaction_ref_mapped::<_, (), DomainError>(move |tx| {
                Box::pin(async move {
                    let record = AuditRecord::new(
                        key_owned.as_str(),
                        Some(tenant_id),
                        actor_owned.subject(),
                        AuditOperation::Stage,
                        actor_owned.request_id.clone(),
                    )
                    .with_post_image(AuditValue::Masked);
                    AuditStore
                        .append(tx, &AccessScope::allow_all(), record)
                        .await
                })
            })
            .await;
        if let Err(err) = outcome {
            self.writer.discard(&conn, &gated, &staged).await;
            return Err(err);
        }
        self.pending
            .find(&conn, &AccessScope::allow_all(), pending_id)
            .await?
            .ok_or_else(|| DomainError::Internal {
                diagnostic: "the intent row of a stage vanished before its answer".to_owned(),
            })
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-5
    }

    /// Release the stages nobody claimed: each expired row's entry is
    /// released and the row deleted, together. Returns how many rows went.
    /// `limit` is clamped to [`SWEEP_LIMIT`]: the bound holds whoever calls.
    /// `stop` is looked at before each row, so a lifecycle being shut down
    /// waits for at most the release in flight, not the rest of the batch.
    ///
    /// # Errors
    /// [`DomainError`] when the database cannot list or delete; a store that
    /// cannot release is logged per entry, the row kept for the next pass.
    pub async fn sweep_expired(
        &self,
        limit: u64,
        stop: &CancellationToken,
    ) -> Result<usize, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        self.sweep_expired_in(&conn, limit.min(SWEEP_LIMIT), stop)
            .await
    }

    async fn sweep_expired_in<C: DBRunner>(
        &self,
        conn: &C,
        limit: u64,
        stop: &CancellationToken,
    ) -> Result<usize, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
        let scope = AccessScope::allow_all();
        let expired = self
            .pending
            .list_expired(conn, &scope, OffsetDateTime::now_utc(), limit)
            .await?;
        let mut released = 0;
        for row in expired {
            // A release already asked for is let finish: dropped halfway, the
            // store may or may not hold the entry, while an untouched row is
            // simply the next pass's.
            if stop.is_cancelled() {
                break;
            }
            let key = self
                .declarations
                .find(
                    conn,
                    &scope,
                    &DomainVisibility::Unrestricted,
                    row.declaration_id,
                )
                .await?
                .map(|d| d.key)
                .unwrap_or_default();
            // The entry first, the row after: a release that fails leaves the
            // row as the durable handle on the entry, and the next pass tries
            // again. This cannot release what a claim has just adopted — the
            // sweep sees only rows past their expiry, and a claim consumes a
            // row only while its expiry is still ahead.
            if let Err(err) = self
                .writer
                .secrets()
                .delete_secret(&key, row.tenant_id, &row.secret_ref)
                .await
            {
                tracing::warn!(
                    pending_id = %row.id,
                    tenant = %row.tenant_id,
                    err = %LogSafe(&err),
                    "expired staged secret not released from the store; kept for the next pass"
                );
                continue;
            }
            // Gone already: a concurrent pass got there first.
            if self.pending.delete(conn, &scope, row.id).await? {
                released += 1;
            }
        }
        Ok(released)
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
    }

    /// Copy the effective value at `from` as an override at `to`.
    ///
    /// The caller authorized `read` for the source and `write` for the target
    /// before this point.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] when the source is outside the caller's
    /// subtree; the target gates' refusals; [`DomainError::Validation`] with
    /// `secret_not_cloneable` for a secret setting; the commit's rejection.
    pub async fn clone_value(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        from: Option<Uuid>,
        to: Option<Uuid>,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-2
        let (source, _, _) = self.writer.target_for(actor, from).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-2
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-3
        let gated = self
            .writer
            .gate(&conn, actor, key, to, StepUpPolicy::Verify, "clone")
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-4
        // Copying a secret reference would couple the target to the source
        // credential's lifecycle; a secret is set at the target instead.
        if gated.declaration.data_classification == "secret" {
            return Err(DomainError::Validation {
                field: "key".to_owned(),
                code: field::SECRET_NOT_CLONEABLE,
                message: format!("`{key}` is secret-classified and cannot be cloned"),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-4
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-5
        let effective = self.writer.resolver().resolve(&conn, key, source).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-5
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-6
        // The effective value, copied once: no continuing link to the source.
        self.commit(
            actor,
            gated,
            Change::Set(effective.value.clone()),
            if_match,
            Uuid::new_v4(),
        )
        .await
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-6
    }

    /// The read-only report for a candidate value.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] for a target outside the subtree;
    /// not-found or retired for the declaration; a resolver failure.
    pub async fn validate(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        value: &Value,
        page: ImpactPage,
    ) -> Result<ValidationReport, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-3
        let (target, _, root) = self.writer.target_for(actor, requested).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-3
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-4
        let declaration = self
            .writer
            .declaration_for_write(&conn, actor, key, root)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-4
        self.writer
            .validate(&conn, &declaration, target, value, page)
            .await
    }

    /// The bounded impact report for a candidate value, with the declaration's
    /// classification so the caller masks each descendant's current value as a
    /// read would.
    ///
    /// # Errors
    /// As [`Self::validate`].
    pub async fn impact(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        candidate: &Value,
        limit: Option<usize>,
    ) -> Result<ImpactOutcome, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-2
        let (target, _, root) = self.writer.target_for(actor, requested).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-2
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-4
        let declaration = self
            .writer
            .declaration_for_write(&conn, actor, key, root)
            .await?;
        let report = self
            .writer
            .impact(&conn, &declaration, target, candidate, limit)
            .await?;
        Ok(ImpactOutcome {
            report,
            data_classification: declaration.data_classification,
        })
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-4
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-3
    }

    /// The effective value at a target after a change, for the response.
    ///
    /// # Errors
    /// A resolver failure.
    pub async fn effective_after(
        &self,
        key: &SettingKey,
        target: ScopeTarget,
    ) -> Result<Arc<EffectiveValue>, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        self.writer.resolver().resolve(&conn, key, target).await
    }
}

/// An impact report together with the classification that masks it.
#[derive(Debug, Clone)]
pub struct ImpactOutcome {
    /// The bounded walk.
    pub report: ImpactReport,
    /// `public`, `pii` or `secret`, from the declaration.
    pub data_classification: String,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "value_writes_tests.rs"]
mod value_writes_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "value_writes_activation_tests.rs"]
mod value_writes_activation_tests;
