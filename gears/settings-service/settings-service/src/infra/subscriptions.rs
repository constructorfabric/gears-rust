// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-subscriptions:p1
//! The adapters around the Subscription Manager: the registration read
//! decision over the policy enforcement point, and the service that takes the
//! key's mutation lock around the transaction of a registration and of a
//! retire.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::pep::AccessRequest;
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use settings_service_sdk::SettingKey;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::authz::resource;
use crate::domain::access::AccessRepository;
use crate::domain::activation::{
    ActivationRepository, KeyLocks, ReadGate, Subscription, SubscriptionManager, tenant_text,
};
use crate::domain::category::{DomainVisibility, domain_visibility};
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::value::ValueRepository;
use crate::log_text::LogSafe;

/// The registration read decision, asked of the `PolicyEnforcer` for `read` on
/// the value resource, as every read of a value is.
pub struct PepReadGate {
    enforcer: Arc<PolicyEnforcer>,
}

impl PepReadGate {
    /// Over the gear's enforcer.
    #[must_use]
    pub fn new(enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { enforcer }
    }
}

#[async_trait]
impl ReadGate for PepReadGate {
    async fn read_visibility(
        &self,
        ctx: &SecurityContext,
    ) -> Result<DomainVisibility, DomainError> {
        // `require_constraints(false)` for the reason `api::authz::access_scope`
        // gives: the value resource declares no PEP property to clamp to.
        match self
            .enforcer
            .access_scope_with(
                ctx,
                &resource::VALUE,
                "read",
                None,
                &AccessRequest::new().require_constraints(false),
            )
            .await
        {
            Ok(scope) => Ok(domain_visibility(&scope)),
            // A denial is the decision point's answer and terminal for the key.
            Err(EnforcerError::Denied { .. }) => Err(DomainError::Unauthorized {
                resource: settings_service_sdk::gts::VALUE_SCHEMA,
            }),
            // No decision could be had — the decision point unreachable, or its
            // answer unusable: not a denial, so the caller retries.
            Err(err @ (EnforcerError::EvaluationFailed(_) | EnforcerError::CompileFailed(_))) => {
                Err(DomainError::dependency_unavailable(
                    "authorization service",
                    "decide the registration read",
                    err,
                ))
            }
        }
    }
}

/// The log line of a retire: `info` for one that deleted a subscription, `warn`
/// for one that failed; a key with nothing to retire is the caller's `404` and
/// logs nothing.
fn log_retire(outcome: &Result<(), DomainError>, subscriber: &str, key: &SettingKey, tenant: &str) {
    match outcome {
        Ok(()) => tracing::info!(%subscriber, key = %key, %tenant, "subscription retired"),
        Err(DomainError::NotFound { .. }) => {}
        Err(err) => tracing::warn!(
            key = %key,
            err = %LogSafe(err),
            "a subscription could not be retired"
        ),
    }
}

/// The server half of subscribing and retiring: the manager's decisions, then
/// the key's mutation lock around the transaction.
pub struct SubscriptionService<D, V, A, R> {
    db: Arc<DBProvider<DbError>>,
    manager: Arc<SubscriptionManager<D, V, A, R>>,
    locks: Arc<dyn KeyLocks>,
}

impl<D, V, A, R> SubscriptionService<D, V, A, R>
where
    D: DeclarationRepository + 'static,
    V: ValueRepository + Clone + 'static,
    A: AccessRepository + 'static,
    R: ActivationRepository + 'static,
{
    /// Over this database, manager and lock.
    pub fn new(
        db: Arc<DBProvider<DbError>>,
        manager: Arc<SubscriptionManager<D, V, A, R>>,
        locks: Arc<dyn KeyLocks>,
    ) -> Self {
        Self { db, manager, locks }
    }

    /// The manager, for the adapters that qualify subscriptions.
    #[must_use]
    pub fn manager(&self) -> &Arc<SubscriptionManager<D, V, A, R>> {
        &self.manager
    }

    /// Register the caller's subscription to `key` at `tenant`, idempotent.
    ///
    /// The read decision and the declaration come first, so a refused
    /// registration never contends for the lock; the lock is then taken around
    /// the transaction that inserts the row, so the registration commits before
    /// a value write of the key takes the lock or waits until that write
    /// commits.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] on a denied read decision;
    /// [`DomainError::NotFound`] for an undeclared key, one hidden from the
    /// caller or a tenant outside its subtree; [`DomainError::Unavailable`] when
    /// the decision cannot be had, the lock wait is exhausted or the database
    /// cannot answer — in each of which nothing is registered.
    pub async fn register_subscription(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<Subscription, DomainError> {
        let conn = self.db.conn().map_err(|e| DomainError::Internal {
            diagnostic: e.to_string(),
        })?;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-12
        let admission = self.manager.admit(&conn, ctx, key, tenant).await?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-12
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-14
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-15
        let held = self.locks.acquire(key.as_str(), None).await?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-15
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-14
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-17
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-18
        let manager = Arc::clone(&self.manager);
        let inserting = admission.clone();
        let outcome = self
            .db
            .db()
            .transaction_ref_mapped::<_, Subscription, DomainError>(move |tx| {
                Box::pin(async move { manager.register_in(tx, &inserting).await })
            })
            .await;
        // Committed or not, the lock is released and the release awaited before
        // the registration answers; a failed insert or commit registered nothing.
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-31
        held.release().await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-31
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-18
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-17
        let subscription = outcome?;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-19
        tracing::info!(
            subscriber = %admission.subscriber,
            key = %admission.key,
            tenant = %admission.tenant,
            "subscription registered"
        );
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-19
        Ok(subscription)
    }

    /// Retire the caller's subscription to `key` at `tenant` together with its
    /// activation state, in one transaction under the key's mutation lock;
    /// nothing is reported for the deleted state.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] for a key with no subscription of the caller's
    /// at that tenant — another subscriber's included, of which nothing is
    /// deleted; [`DomainError::Unavailable`] when the lock wait is exhausted,
    /// with nothing deleted and the key still subscribed.
    pub async fn retire_subscription(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-3
        let subscriber = crate::domain::activation::subscriber_of(ctx);
        let tenant = tenant_text(tenant);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-3
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-4
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-5
        let held = self.locks.acquire(key.as_str(), None).await?;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-5
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-4
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-8
        let manager = Arc::clone(&self.manager);
        let (who, what, at) = (subscriber.clone(), key.as_str().to_owned(), tenant.clone());
        let outcome = self
            .db
            .db()
            .transaction_ref_mapped::<_, (), DomainError>(move |tx| {
                Box::pin(async move { manager.retire_in(tx, &who, &what, &at).await })
            })
            .await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-15
        held.release().await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-15
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-16
        log_retire(&outcome, &subscriber, key, &tenant);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-16
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-9
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-14
        // `Ok` is `retired`; a subscription that was not there is `404` and a
        // lock wait that ran out `Unavailable`. The SDK retries none of them.
        outcome
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-14
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-9
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-8
    }
}

#[cfg(test)]
#[path = "subscriptions_tests.rs"]
mod subscriptions_tests;
