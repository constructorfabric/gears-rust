// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-subscriptions:p1
//! The Subscription Manager: who is subscribed to a key, and which of them a
//! change at one scope reaches.
//!
//! A subscription is explicit and exact — `(subscriber, instance_id, key,
//! tenant)`, one per consumer instance — and is registered for any key the
//! consumer may read, whoever owns or contributes it. The manager owns the decisions around a registration, the read of a
//! key's subscriptions under the lock and their qualification by Scope Class,
//! and the unsubscribe; the lock and the transaction around a registration and an
//! unsubscribe belong to the adapter that calls it (`infra::subscriptions`), as the
//! Value Writer's belong to the Write Coordinator.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use settings_service_sdk::SettingKey;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::publisher::{Qualified, StateWrite};
use super::recorder::Transition;
use super::{
    ActivationRow, ActivationState, ActivationStatus, InstanceKey, NotifyMetrics, SubscriberTenant,
    Subscription, tenant_text,
};
use crate::domain::access::AccessRepository;
use crate::domain::category::{DomainVisibility, is_visible};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::resolution::{ScopeTarget, ValueResolver, scope_class};
use crate::domain::value::ValueRepository;

/// Persistence of subscriptions and activation states, one per consumer
/// instance: both are keyed by `(subscriber, instance_id, key, tenant)`
/// ([`InstanceKey`]).
///
/// Plain statements, no decision: the Subscription Manager, the Activation
/// Publisher and the Recorder compose them in their transactions, in the
/// statement order every writer keeps — an instance's activation state before
/// its subscription.
#[async_trait]
pub trait ActivationRepository: Send + Sync {
    /// Insert the subscription of the instance, or refresh it: idempotent on
    /// `uq_sub_subscriber_instance_key_tenant`. A new row carries
    /// `instance_label`, no `silent_since` and `created_at` now; an existing
    /// row is kept with its `instance_label` replaced by the given one and
    /// nothing else touched — its `silent_since` is the business of
    /// [`Self::clear_silent`]. Returns the subscription as it now stands. The
    /// label is stored as given: truncating it to 128 characters is the
    /// caller's, and the column refuses a longer one.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    async fn register<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
        instance_label: Option<&str>,
    ) -> Result<Subscription, DomainError>;

    /// Every subscription of `key`, every instance of every subscriber at every
    /// tenant, through `idx_sub_key`, oldest first.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn of_key<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
    ) -> Result<Vec<Subscription>, DomainError>;

    /// The subscription of exactly this instance, when there is one.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
    ) -> Result<Option<Subscription>, DomainError>;

    /// How many registrations — one per instance — `subscriber` holds for `key`
    /// at `tenant`: what the registration cap counts.
    ///
    /// # Errors
    /// [`DomainError`] when the count fails.
    async fn registrations<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<u64, DomainError>;

    /// The oldest evictable registration of `subscriber` for `key` at
    /// `tenant`, by `created_at` then id: one whose `silent_since` is set, or
    /// whose instance holds no activation state. `None` when none of them is.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn oldest_evictable<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<Subscription>, DomainError>;

    /// The subscriptions of `key` at `tenant` whose `silent_since` is set,
    /// through `idx_sub_silent`, ordered by subscriber, `created_at` and id:
    /// where the activation read finds the instances a change left out.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn silent_subscriptions<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
        tenant: &str,
    ) -> Result<Vec<Subscription>, DomainError>;

    /// Delete the instance's activation state and then its subscription, in
    /// the caller's transaction; nothing is reported for the state, and the
    /// subscriber's other instances are untouched. What an unsubscribe and an
    /// eviction by the registration cap do. `true` when the subscription
    /// existed.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    async fn unsubscribe<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
    ) -> Result<bool, DomainError>;

    /// Delete every activation state and then every subscription of `key` —
    /// every subscriber, every instance, every tenant, any status — in the
    /// caller's transaction. Nothing is reported for the deleted states. What
    /// a purge of the key's declaration does, the tables holding no foreign key
    /// to it. Returns how many subscriptions and how many states were deleted.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    async fn delete_key<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
    ) -> Result<(usize, usize), DomainError>;

    /// Write the activation states of one change: delete every state of the
    /// read set's subscriptions that still exist — the silent ones included —
    /// then, in one insert-select from `subscriptions` restricted to the read
    /// set's ids, write the state of each one the change expects: those whose
    /// `silent_since` is absent, and every one of a subscriber at the key and
    /// tenant when all of its subscriptions there in the read set are silent —
    /// a predicate of that one statement. Each state is `awaiting`, with its
    /// expected value, the change's `change_ref`, `changed_at` the database's
    /// `now()`, `deliveries` `0` and every other field cleared, whatever the
    /// state was before. Returns the pairs of subscriber and tenant that now
    /// hold this change's states, each once.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    async fn write_states<C: DBRunner>(
        &self,
        conn: &C,
        write: &StateWrite,
    ) -> Result<Vec<SubscriberTenant>, DomainError>;

    /// The activation state of exactly this instance, when there is one.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn state<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
    ) -> Result<Option<ActivationState>, DomainError>;

    /// Every activation state of `key` at `tenant`, ordered by subscriber and
    /// instance, in one statement through `idx_activation_key_tenant`.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the read fails.
    async fn states_of<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
        tenant: &str,
    ) -> Result<Vec<ActivationState>, DomainError>;

    /// The activation read's rows of `key` at `tenant`: what each state shows —
    /// status, `change_ref`, `silent_since`, `reported_at`, `detail` — joined
    /// to its subscription for `instance_label` by an outer join (a row is kept
    /// without a label when its subscription is absent), in one statement
    /// through `idx_activation_key_tenant`, ordered by subscriber and instance.
    /// It reads no expected or applied value.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the read fails.
    async fn activation_rows<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
        tenant: &str,
    ) -> Result<Vec<ActivationRow>, DomainError>;

    /// Settle the state a report decided on: set `status`, `applied_value`,
    /// `detail`, `reported_at` now and `updated_at` now — nothing else, so a
    /// silent state keeps its `silent_since` — guarded on the row still naming
    /// the transition's `change_ref` and still being `awaiting` or `silent`.
    /// Returns the stamp written, or `None` when the guard matched no row — a
    /// concurrent report settled it, or a concurrent change replaced its
    /// expectation.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    async fn report<C: DBRunner>(
        &self,
        conn: &C,
        transition: &Transition,
    ) -> Result<Option<time::OffsetDateTime>, DomainError>;

    /// Re-arm the instance's `silent` state of `change_ref` to `awaiting`:
    /// `deliveries` `0`, `silent_since` and `last_notified_at` cleared,
    /// `updated_at` now; guarded on the row still being `silent` and still
    /// naming `change_ref`. `true` when the guard matched.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    async fn rearm<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
        change_ref: Uuid,
    ) -> Result<bool, DomainError>;

    /// Clear the `silent_since` of the instance's subscription, when it is set
    /// and the instance holds no `silent` state for the key and tenant at the
    /// clearing statement's start. A silent subscription's row is taken first,
    /// in a statement of its own, so a call of silence in flight — which holds
    /// that row until it commits, marked or already silent — is waited for
    /// and its `silent` state seen. `true` when a subscription was cleared.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    async fn clear_silent<C: DBRunner>(
        &self,
        conn: &C,
        instance: &InstanceKey,
    ) -> Result<bool, DomainError>;
}

/// The repository of a writer that records no activation state: it holds
/// nothing and writes nothing. Never reached — a commit with no activation
/// input never calls it — it only gives that commit a repository type.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoActivation;

#[async_trait]
impl ActivationRepository for NoActivation {
    async fn register<C: DBRunner>(
        &self,
        _conn: &C,
        _instance: &InstanceKey,
        _instance_label: Option<&str>,
    ) -> Result<Subscription, DomainError> {
        Err(DomainError::Unavailable {
            detail: "activation is not bound".to_owned(),
        })
    }

    async fn of_key<C: DBRunner>(
        &self,
        _conn: &C,
        _key: &str,
    ) -> Result<Vec<Subscription>, DomainError> {
        Ok(Vec::new())
    }

    async fn find<C: DBRunner>(
        &self,
        _conn: &C,
        _instance: &InstanceKey,
    ) -> Result<Option<Subscription>, DomainError> {
        Ok(None)
    }

    async fn registrations<C: DBRunner>(
        &self,
        _conn: &C,
        _subscriber: &str,
        _key: &str,
        _tenant: &str,
    ) -> Result<u64, DomainError> {
        Ok(0)
    }

    async fn oldest_evictable<C: DBRunner>(
        &self,
        _conn: &C,
        _subscriber: &str,
        _key: &str,
        _tenant: &str,
    ) -> Result<Option<Subscription>, DomainError> {
        Ok(None)
    }

    async fn silent_subscriptions<C: DBRunner>(
        &self,
        _conn: &C,
        _key: &str,
        _tenant: &str,
    ) -> Result<Vec<Subscription>, DomainError> {
        Ok(Vec::new())
    }

    async fn unsubscribe<C: DBRunner>(
        &self,
        _conn: &C,
        _instance: &InstanceKey,
    ) -> Result<bool, DomainError> {
        Ok(false)
    }

    async fn delete_key<C: DBRunner>(
        &self,
        _conn: &C,
        _key: &str,
    ) -> Result<(usize, usize), DomainError> {
        Ok((0, 0))
    }

    async fn write_states<C: DBRunner>(
        &self,
        _conn: &C,
        _write: &StateWrite,
    ) -> Result<Vec<SubscriberTenant>, DomainError> {
        Ok(Vec::new())
    }

    async fn state<C: DBRunner>(
        &self,
        _conn: &C,
        _instance: &InstanceKey,
    ) -> Result<Option<ActivationState>, DomainError> {
        Ok(None)
    }

    async fn states_of<C: DBRunner>(
        &self,
        _conn: &C,
        _key: &str,
        _tenant: &str,
    ) -> Result<Vec<ActivationState>, DomainError> {
        Ok(Vec::new())
    }

    async fn activation_rows<C: DBRunner>(
        &self,
        _conn: &C,
        _key: &str,
        _tenant: &str,
    ) -> Result<Vec<ActivationRow>, DomainError> {
        Ok(Vec::new())
    }

    async fn report<C: DBRunner>(
        &self,
        _conn: &C,
        _transition: &Transition,
    ) -> Result<Option<time::OffsetDateTime>, DomainError> {
        Ok(None)
    }

    async fn rearm<C: DBRunner>(
        &self,
        _conn: &C,
        _instance: &InstanceKey,
        _change_ref: Uuid,
    ) -> Result<bool, DomainError> {
        Ok(false)
    }

    async fn clear_silent<C: DBRunner>(
        &self,
        _conn: &C,
        _instance: &InstanceKey,
    ) -> Result<bool, DomainError> {
        Ok(false)
    }
}

/// The registration read decision: `read` on the key at the subscription's
/// tenant, asked of the policy decision point before any lock is taken.
#[async_trait]
pub trait ReadGate: Send + Sync {
    /// The domains the caller may read, when the decision is an allow.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] when the decision is a denial;
    /// [`DomainError::Unavailable`] when no decision can be obtained, which the
    /// caller retries, a denial being terminal.
    async fn read_visibility(&self, ctx: &SecurityContext)
    -> Result<DomainVisibility, DomainError>;
}

/// How many registrations — one per consumer instance — a subscriber holds
/// for one key at one tenant: a constant, not configuration. A registration of
/// a new instance past it evicts the oldest evictable one, or is refused.
pub const MAX_INSTANCES_PER_SUBSCRIPTION: u64 = 64;

/// The longest `instance_label` stored, in characters.
pub const INSTANCE_LABEL_MAX_CHARS: usize = 128;

/// The `instance_id` of a consumer call — a registration, a report or an
/// unsubscribe. The SDK client always sets it, so a call without one came past
/// the client and is refused before anything is decided or locked.
///
/// # Errors
/// [`DomainError::Validation`] with [`crate::field::INSTANCE_ID_REQUIRED`]
/// when the call carries none.
pub fn require_instance_id(instance_id: Option<Uuid>) -> Result<Uuid, DomainError> {
    instance_id.ok_or_else(|| DomainError::Validation {
        field: "instance_id".to_owned(),
        code: crate::field::INSTANCE_ID_REQUIRED,
        message: "the call carries no instance_id".to_owned(),
    })
}

/// An instance label as stored: the call's, cut to
/// [`INSTANCE_LABEL_MAX_CHARS`] characters.
#[must_use]
pub fn instance_label(label: Option<&str>) -> Option<String> {
    label.map(|label| label.chars().take(INSTANCE_LABEL_MAX_CHARS).collect())
}

/// A registration that passed its decisions and is ready for the lock: the
/// subscriber, the instance, the key and the tenant as the tables hold them.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// The admitted declaration, read again under the shared row lock when the
    /// subscription is inserted.
    pub declaration_id: Uuid,
    /// The subscriber: the subject of the caller's security context.
    pub subscriber: String,
    /// The registering consumer instance, from the call.
    pub instance_id: Uuid,
    /// The instance's informational label from the call, at most
    /// [`INSTANCE_LABEL_MAX_CHARS`] characters.
    pub instance_label: Option<String>,
    /// The exact setting key.
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
}

impl Admission {
    /// The identity of the subscription the registration inserts or refreshes.
    #[must_use]
    pub fn instance(&self) -> InstanceKey {
        InstanceKey {
            subscriber: self.subscriber.clone(),
            instance_id: self.instance_id,
            key: self.key.clone(),
            tenant: self.tenant.clone(),
        }
    }
}

/// What a registering transaction did.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// The instance's subscription as it now stands, and the registration the
    /// cap evicted to make room for it, if any.
    Registered {
        /// The inserted or refreshed subscription.
        subscription: Box<Subscription>,
        /// The evicted registration: its states and its subscription were
        /// deleted first, in the same transaction.
        evicted: Option<InstanceKey>,
    },
    /// The cap was reached and no registration was evictable: nothing was
    /// written.
    Refused,
}

/// The manager.
#[domain_model]
pub struct SubscriptionManager<D, V, A, R> {
    resolver: Arc<ValueResolver<D, V, A>>,
    repo: R,
    gate: Arc<dyn ReadGate>,
    metrics: Arc<dyn NotifyMetrics>,
}

fn not_found(resource: &'static str) -> DomainError {
    DomainError::NotFound { resource }
}

/// The identity of the caller's own subscription an unsubscribe names: the
/// subscriber from the security context, the instance from the call.
///
/// # Errors
/// [`DomainError::Unauthorized`] when the caller's subject is of an interactive
/// subject type; [`DomainError::Validation`] when the call carries no
/// `instance_id`.
pub fn unsubscribing(
    ctx: &SecurityContext,
    key: &SettingKey,
    tenant: Uuid,
    instance_id: Option<Uuid>,
) -> Result<InstanceKey, DomainError> {
    // A person holds no subscription: refused before the lock is taken.
    let subscriber = super::calling_subscriber(ctx)?;
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-18
    let instance_id = require_instance_id(instance_id)?;
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-18
    Ok(InstanceKey {
        subscriber,
        instance_id,
        key: key.as_str().to_owned(),
        tenant: tenant_text(tenant),
    })
}

impl<D, V, A, R> SubscriptionManager<D, V, A, R>
where
    D: DeclarationRepository,
    V: ValueRepository + Clone,
    A: AccessRepository,
    R: ActivationRepository,
{
    /// A manager over the resolver, the repository, the read decision and the
    /// instruments the registration cap counts on.
    pub fn new(
        resolver: Arc<ValueResolver<D, V, A>>,
        repo: R,
        gate: Arc<dyn ReadGate>,
        metrics: Arc<dyn NotifyMetrics>,
    ) -> Self {
        Self {
            resolver,
            repo,
            gate,
            metrics,
        }
    }

    /// The repository, for the adapters that read states directly.
    #[must_use]
    pub fn repo(&self) -> &R {
        &self.repo
    }

    /// The decisions of a registration, taken before any lock so that a refused
    /// registration never contends for one: the caller's subject — a person is
    /// refused — then the call's `instance_id`, then the read decision, then
    /// the declaration — absent, hidden from the caller's
    /// administrative domains, hidden at the tenant, or the tenant outside the
    /// caller's subtree is `NotFound`. A retired key is admitted: its
    /// declaration still exists, and a reactivation re-arms the next change.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] for a caller whose subject is of an
    /// interactive subject type, before anything else is decided, and on a
    /// denied read decision; [`DomainError::Validation`] for a call without an
    /// `instance_id`; [`DomainError::Unavailable`] when no decision or no read
    /// can be had; [`DomainError::NotFound`] as above.
    pub async fn admit<C: DBRunner>(
        &self,
        conn: &C,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
        instance_id: Option<Uuid>,
        label: Option<&str>,
    ) -> Result<Admission, DomainError> {
        // Before anything is decided or read: the subscriber is the subject of
        // the caller's security context, never an argument, and a person is
        // refused — terminal, as a denied read decision is.
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-3
        let subscriber = super::calling_subscriber(ctx)?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-3
        // Before anything is decided: a call without its instance came past the
        // SDK client and is refused with nothing registered and no lock taken.
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-35
        let instance_id = require_instance_id(instance_id)?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-35
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-9
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-10
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-11
        // Before the declaration is looked at and before any lock: a denial is
        // terminal and an undecidable answer a retry, and neither contends.
        let visibility = self.gate.read_visibility(ctx).await?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-11
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-10
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-9
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-13
        let declaration = self
            .resolver
            .find_declaration(conn, key)
            .await?
            .ok_or_else(|| not_found("declaration"))?;
        if !is_visible(&visibility, declaration.domain_affinity.as_deref()) {
            return Err(not_found("declaration"));
        }
        let root = self.resolver.root_tenant().await?;
        let target = ScopeTarget::Tenant(tenant).normalize(root);
        if self
            .resolver
            .effective_access(conn, declaration.id, target)
            .await?
            .is_hidden()
        {
            return Err(not_found("declaration"));
        }
        let caller = ctx.subject_tenant_id();
        if tenant != caller {
            let hierarchy = self.resolver.hierarchy();
            if !hierarchy.is_within_subtree(caller, tenant).await?
                || hierarchy.is_standalone(tenant).await?
            {
                return Err(not_found("declaration"));
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-13
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-3
        // The tenant as a bare lowercase UUID string, the subscriber as taken
        // from the caller's security context above.
        Ok(Admission {
            declaration_id: declaration.id,
            subscriber,
            instance_id,
            instance_label: instance_label(label),
            key: key.as_str().to_owned(),
            tenant: tenant_text(tenant),
        })
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-3
    }

    /// Register the admitted instance in the caller's transaction, idempotent
    /// per instance. The admitted declaration is read again first, under the
    /// shared row lock: a purge holds that row for update, so it either
    /// deleted the declaration before this read — nothing is inserted — or
    /// waits for this transaction and then deletes the row inserted here.
    ///
    /// Then, in the statement order every writer keeps: a new instance that
    /// would hold the subscriber's 65th registration of the key at the tenant
    /// first evicts the oldest evictable one — silent, or holding no state —
    /// its states and then its subscription; a repeat registration evicts
    /// nothing and re-arms its own `silent` state of the current change,
    /// guarded on `silent` and the `change_ref` read here; last the
    /// subscription is inserted, or kept with its label refreshed and its
    /// `silent_since` cleared under the guard that no `silent` state of the
    /// instance remains. A registration creates no activation state.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when the declaration no longer exists;
    /// [`DomainError`] when a statement fails.
    pub async fn register_in<C: DBRunner>(
        &self,
        tx: &C,
        admission: &Admission,
    ) -> Result<Registration, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-32
        self.resolver
            .lock_declaration(tx, admission.declaration_id)
            .await?
            .ok_or_else(|| not_found("declaration"))?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-32
        let instance = admission.instance();
        let existing = self.repo.find(tx, &instance).await?;
        // The cap counts only for an instance not yet registered: a repeat
        // registration evicts nothing. An eviction is the transaction's first
        // write — the evicted instance's states, then its subscription.
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-36
        let mut evicted = None;
        if existing.is_none() {
            let held = self
                .repo
                .registrations(tx, &admission.subscriber, &admission.key, &admission.tenant)
                .await?;
            if held >= MAX_INSTANCES_PER_SUBSCRIPTION {
                let Some(oldest) = self
                    .repo
                    .oldest_evictable(tx, &admission.subscriber, &admission.key, &admission.tenant)
                    .await?
                else {
                    // Every registration is expected by the current change and
                    // not silent: nothing is written.
                    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-37
                    return Ok(Registration::Refused);
                    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-37
                };
                let oldest = oldest.instance_key();
                self.repo.unsubscribe(tx, &oldest).await?;
                evicted = Some(oldest);
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-36
        // The re-arm of the instance's own `silent` state of the current
        // change, guarded on the `change_ref` read here so a state a change
        // replaced meanwhile is left alone. No state is ever created here.
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-38
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-39
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-10
        if existing.is_some()
            && let Some(state) = self.repo.state(tx, &instance).await?
            && state.status == ActivationStatus::Silent
        {
            self.repo.rearm(tx, &instance, state.change_ref).await?;
        }
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-10
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-39
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-38
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-16
        // Last, the subscription itself: inserted, or kept with its label
        // refreshed and its silence ended under the clear's own guard.
        let mut subscription = self
            .repo
            .register(tx, &instance, admission.instance_label.as_deref())
            .await?;
        if subscription.silent_since.is_some() && self.repo.clear_silent(tx, &instance).await? {
            subscription.silent_since = None;
        }
        Ok(Registration::Registered {
            subscription: Box::new(subscription),
            evicted,
        })
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-16
    }

    /// The answer of a registration whose transaction committed: the eviction
    /// it made logged at `warn` and counted `evicted`, or a cap with nothing
    /// evictable counted `refused` and answered `Unavailable`. Called only
    /// after the commit, since a failed commit rolls an eviction back.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] for [`Registration::Refused`].
    pub fn conclude(&self, registration: Registration) -> Result<Subscription, DomainError> {
        match registration {
            Registration::Registered {
                subscription,
                evicted,
            } => {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-36
                if let Some(evicted) = evicted {
                    tracing::warn!(
                        subscriber = %evicted.subscriber,
                        key = %evicted.key,
                        tenant = %evicted.tenant,
                        evicted_instance_id = %evicted.instance_id,
                        "the registration cap evicted the oldest evictable registration"
                    );
                    self.metrics.eviction("evicted");
                }
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-36
                Ok(*subscription)
            }
            Registration::Refused => {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-37
                self.metrics.eviction("refused");
                Err(DomainError::Unavailable {
                    detail: "the registration cap is reached and no registration can be evicted"
                        .to_owned(),
                })
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-37
            }
        }
    }

    /// Delete the instance's subscription and its activation states in the
    /// caller's transaction; nothing is reported for them, and the
    /// subscriber's other instances are untouched.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when the caller's subscriber holds no
    /// subscription under that `instance_id` for the key at the tenant —
    /// another subscriber's included, of which nothing is deleted;
    /// [`DomainError`] when a statement fails.
    pub async fn unsubscribe_in<C: DBRunner>(
        &self,
        tx: &C,
        instance: &InstanceKey,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-6
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-7
        // The subscription of the caller's own instance and nothing else: the
        // statements are keyed on the subscriber and the instance, so another
        // subscriber's row, and a sibling instance's not named, is neither
        // found nor touched.
        if self.repo.unsubscribe(tx, instance).await? {
            Ok(())
        } else {
            Err(not_found("subscription"))
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-7
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-6
    }

    /// The subscriptions of the changed key a change at `written` reaches,
    /// read once under the key's mutation lock: every one for `global`, the
    /// written tenant's for `local`, and for `cascading` those whose tenant is
    /// the written scope or a descendant of it — decided per subscription
    /// tenant from its ancestor chain, with no subtree enumerated, barriers
    /// ignored as runtime resolution ignores them. The chain of each
    /// qualifying `cascading` tenant is kept for the transaction. The distinct
    /// subscription tenants are resolved concurrently, at most
    /// [`CHAIN_CONCURRENCY`] at a time. A subscription whose tenant the Tenant
    /// Resolver reports as not existing cannot qualify and is skipped with a
    /// `warn`.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails or the Tenant Resolver cannot
    /// answer, which fails the write closed.
    pub async fn resolve_subscribers<C: DBRunner>(
        &self,
        conn: &C,
        declaration: &Declaration,
        written: Uuid,
    ) -> Result<Vec<Qualified>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-5
        let subscriptions = self.repo.of_key(conn, &declaration.key).await?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-5
        if subscriptions.is_empty() {
            return Ok(Vec::new());
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-6
        let written_text = tenant_text(written);
        let subscriptions = parsed(subscriptions)?;
        let chains = if declaration.scope_class == scope_class::CASCADING {
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-8
            // One chain per distinct subscription tenant, asked of the Tenant
            // Resolver here, under the lock and before the transaction: the
            // resolver is an out-of-transaction call. A resolver that cannot
            // answer is the write's failure, nothing stored; a tenant it says
            // does not exist is a subscription that cannot qualify.
            self.chains_of(subscriptions.iter().map(|(_, tenant)| *tenant))
                .await?
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-8
        } else {
            HashMap::new()
        };
        let mut qualified = Vec::with_capacity(subscriptions.len());
        for (subscription, tenant) in subscriptions {
            match declaration.scope_class.as_str() {
                scope_class::GLOBAL => qualified.push(Qualified {
                    subscription,
                    tenant,
                    chain: Vec::new(),
                }),
                scope_class::LOCAL => {
                    if subscription.tenant == written_text {
                        qualified.push(Qualified {
                            subscription,
                            tenant,
                            chain: Vec::new(),
                        });
                    }
                }
                scope_class::CASCADING => {
                    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-7
                    let Some(chain) = chains.get(&tenant).and_then(Option::as_ref) else {
                        tracing::warn!(
                            subscriber = %subscription.subscriber,
                            instance_id = %subscription.instance_id,
                            key = %declaration.key,
                            %tenant,
                            "a subscription's tenant does not exist; it is skipped"
                        );
                        continue;
                    };
                    let chain = chain.clone();
                    if chain.contains(&written) {
                        qualified.push(Qualified {
                            subscription,
                            tenant,
                            chain,
                        });
                    }
                    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-7
                }
                other => {
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "declaration `{}` carries the unknown scope class `{other}`",
                            declaration.key
                        ),
                    });
                }
            }
        }
        Ok(qualified)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-6
    }

    /// The chain of every distinct tenant, `None` for one the Tenant Resolver
    /// reports as not existing, resolved concurrently within the bound.
    async fn chains_of(
        &self,
        tenants: impl Iterator<Item = Uuid>,
    ) -> Result<HashMap<Uuid, Option<Vec<Uuid>>>, DomainError> {
        let distinct: Vec<Uuid> = tenants
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let answers: Vec<ChainAnswer> = futures_util::stream::iter(distinct)
            .map(|tenant| async move { (tenant, self.resolver.chain_of(tenant).await) })
            .buffer_unordered(CHAIN_CONCURRENCY)
            .collect()
            .await;
        let mut chains = HashMap::with_capacity(answers.len());
        for (tenant, answer) in answers {
            match answer {
                Ok(chain) => chains.insert(tenant, Some(chain)),
                Err(DomainError::NotFound { resource: "tenant" }) => chains.insert(tenant, None),
                Err(other) => return Err(other),
            };
        }
        Ok(chains)
    }
}

/// A tenant and what the Tenant Resolver answered for its chain.
type ChainAnswer = (Uuid, Result<Vec<Uuid>, DomainError>);

/// How many tenants' chains one change resolves at a time.
pub const CHAIN_CONCURRENCY: usize = 8;

/// The subscriptions with their tenants parsed.
fn parsed(subscriptions: Vec<Subscription>) -> Result<Vec<(Subscription, Uuid)>, DomainError> {
    subscriptions
        .into_iter()
        .map(|subscription| {
            let tenant =
                Uuid::parse_str(&subscription.tenant).map_err(|_| DomainError::Internal {
                    diagnostic: format!(
                        "subscription {} carries a tenant that is not a UUID",
                        subscription.id
                    ),
                })?;
            Ok((subscription, tenant))
        })
        .collect()
}

#[cfg(test)]
#[path = "subscriptions_tests.rs"]
mod subscriptions_tests;
