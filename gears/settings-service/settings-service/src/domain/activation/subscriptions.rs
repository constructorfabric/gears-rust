// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-subscriptions:p1
//! The Subscription Manager: who is subscribed to a key, and which of them a
//! change at one scope reaches.
//!
//! A subscription is explicit and exact — `(subscriber, key, tenant)` — and is
//! registered for any key the consumer may read, whoever owns or contributes
//! it. The manager owns the decisions around a registration, the read of a
//! key's subscriptions under the lock and their qualification by Scope Class,
//! and the retire; the lock and the transaction around a registration and a
//! retire belong to the adapter that calls it (`infra::subscriptions`), as the
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
use super::{ActivationState, SubscriberTenant, Subscription, tenant_text};
use crate::domain::access::AccessRepository;
use crate::domain::category::{DomainVisibility, is_visible};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::resolution::{ScopeTarget, ValueResolver, scope_class};
use crate::domain::value::ValueRepository;

/// Persistence of subscriptions and activation states.
#[async_trait]
pub trait ActivationRepository: Send + Sync {
    /// Insert the subscription of `(subscriber, key, tenant)`, idempotent on
    /// `uq_sub_subscriber_key_tenant`: an existing row is kept and returned.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    async fn register<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Subscription, DomainError>;

    /// Every subscription of `key`, through `idx_sub_key`.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn of_key<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
    ) -> Result<Vec<Subscription>, DomainError>;

    /// The subscription of exactly this triple, when there is one.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<Subscription>, DomainError>;

    /// Delete the subscription of this triple and its activation state, in the
    /// caller's transaction. `true` when the subscription existed.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    async fn retire<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<bool, DomainError>;

    /// Write the activation states of one change: for each affected
    /// subscription of the read set whose row still exists, `awaiting` with
    /// its expected value and the change's `change_ref`, the report fields and
    /// `last_notified_at` cleared, whatever the state was before. Returns the
    /// pairs of subscriber and tenant that now hold this change's state.
    ///
    /// # Errors
    /// [`DomainError`] when a statement fails.
    async fn write_states<C: DBRunner>(
        &self,
        conn: &C,
        write: &StateWrite,
    ) -> Result<Vec<SubscriberTenant>, DomainError>;

    /// The activation state of exactly this triple, when there is one.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn state<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<ActivationState>, DomainError>;
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
        _subscriber: &str,
        _key: &str,
        _tenant: &str,
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
        _subscriber: &str,
        _key: &str,
        _tenant: &str,
    ) -> Result<Option<Subscription>, DomainError> {
        Ok(None)
    }

    async fn retire<C: DBRunner>(
        &self,
        _conn: &C,
        _subscriber: &str,
        _key: &str,
        _tenant: &str,
    ) -> Result<bool, DomainError> {
        Ok(false)
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
        _subscriber: &str,
        _key: &str,
        _tenant: &str,
    ) -> Result<Option<ActivationState>, DomainError> {
        Ok(None)
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

/// A registration that passed its decisions and is ready for the lock: the
/// subscriber, the key and the tenant as the tables hold them.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// The subscriber: the subject of the caller's security context.
    pub subscriber: String,
    /// The exact setting key.
    pub key: String,
    /// The subscription's tenant, a bare lowercase UUID string.
    pub tenant: String,
}

/// The manager.
#[domain_model]
pub struct SubscriptionManager<D, V, A, R> {
    resolver: Arc<ValueResolver<D, V, A>>,
    repo: R,
    gate: Arc<dyn ReadGate>,
}

fn not_found(resource: &'static str) -> DomainError {
    DomainError::NotFound { resource }
}

impl<D, V, A, R> SubscriptionManager<D, V, A, R>
where
    D: DeclarationRepository,
    V: ValueRepository + Clone,
    A: AccessRepository,
    R: ActivationRepository,
{
    /// A manager over the resolver, the repository and the read decision.
    pub fn new(resolver: Arc<ValueResolver<D, V, A>>, repo: R, gate: Arc<dyn ReadGate>) -> Self {
        Self {
            resolver,
            repo,
            gate,
        }
    }

    /// The repository, for the adapters that read states directly.
    #[must_use]
    pub fn repo(&self) -> &R {
        &self.repo
    }

    /// The decisions of a registration, taken before any lock so that a refused
    /// registration never contends for one: the read decision, then the
    /// declaration — absent, hidden from the caller's administrative domains,
    /// hidden at the tenant, or the tenant outside the caller's subtree is
    /// `NotFound`. A retired key is admitted: its declaration still exists, and
    /// a reactivation re-arms the next change.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] on a denied read decision;
    /// [`DomainError::Unavailable`] when no decision or no read can be had;
    /// [`DomainError::NotFound`] as above.
    pub async fn admit<C: DBRunner>(
        &self,
        conn: &C,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<Admission, DomainError> {
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
        // The tenant as a bare lowercase UUID string, the subscriber from the
        // caller's security context and never from an argument.
        Ok(Admission {
            subscriber: super::subscriber_of(ctx),
            key: key.as_str().to_owned(),
            tenant: tenant_text(tenant),
        })
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-3
    }

    /// Insert the subscription in the caller's transaction, idempotent: an
    /// existing row is kept.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    pub async fn register_in<C: DBRunner>(
        &self,
        tx: &C,
        admission: &Admission,
    ) -> Result<Subscription, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-16
        self.repo
            .register(tx, &admission.subscriber, &admission.key, &admission.tenant)
            .await
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-16
    }

    /// Delete the subscription and its activation state in the caller's
    /// transaction; nothing is reported for them.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] for a key with no subscription of this
    /// subscriber at the tenant — another subscriber's included, of which
    /// nothing is deleted; [`DomainError`] when a statement fails.
    pub async fn retire_in<C: DBRunner>(
        &self,
        tx: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-6
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-7
        // The subscription of the caller's own triple and nothing else: the
        // statements are keyed on the subscriber, so another subscriber's row
        // is neither found nor touched.
        if self.repo.retire(tx, subscriber, key, tenant).await? {
            Ok(())
        } else {
            Err(not_found("subscription"))
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-7
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-6
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
