// @cpt-dod:cpt-cf-settings-service-dod-module-contributions-operations:p1
//! The in-process `SettingsContributionClient`.
//!
//! Bound into `ClientHub` at init and resolved by a contributing gear from its
//! own init. Each declaration is reconciled in its own transaction: a refused
//! item writes nothing and is reported per key, a failed one rolls back, and
//! the others land regardless.

use std::sync::Arc;

use async_trait::async_trait;
use settings_service_sdk::SettingKey;
use settings_service_sdk::api::SettingsContributionClient;
use settings_service_sdk::models::{ContributedDeclaration, ContributionError, ReconcileResult};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;

use crate::domain::category::CategoryRepository;
use crate::domain::coherence::CoherencePublisher;
use crate::domain::contribution::{ContributionService, ItemError, Outcome};
use crate::domain::declaration::DeclarationRepository;
use uuid::Uuid;

use crate::audit::AuditSink;
use crate::domain::error::DomainError;
use crate::domain::ports::{ChangePublisher, ValueEvent};
use crate::domain::resolution::EffectiveCache;
use crate::domain::value::ValueRepository;

/// The SDK trait over the reconciler and the database.
pub struct ContributionClient<D, Cat, V, S> {
    db: Arc<DBProvider<DbError>>,
    service: Arc<ContributionService<D, Cat, V, S>>,
    cache: Arc<EffectiveCache>,
    publisher: Arc<dyn ChangePublisher>,
    coherence: Option<Arc<CoherencePublisher>>,
}

impl<D, Cat, V, S> ContributionClient<D, Cat, V, S> {
    /// Serve the contract over this database and reconciler, evicting the
    /// effective-value cache for every declaration it changes.
    pub fn new(
        db: Arc<DBProvider<DbError>>,
        service: Arc<ContributionService<D, Cat, V, S>>,
        cache: Arc<EffectiveCache>,
        publisher: Arc<dyn ChangePublisher>,
    ) -> Self {
        Self {
            db,
            service,
            cache,
            publisher,
            coherence: None,
        }
    }

    /// Publish `cache_invalidate` at the end of every pass through `hook`.
    /// The root tenant comes from the hook's client cache: the Reconciler
    /// resolves no tenant, and a pass before the id is known is skipped.
    #[must_use]
    pub fn with_coherence(mut self, hook: Arc<CoherencePublisher>) -> Self {
        self.coherence = Some(hook);
        self
    }

    /// The end of a pass: one broadcast naming every key it changed, under one
    /// correlation id minted for the pass. Nothing when it changed none.
    async fn publish_pass(&self, changed: Vec<String>, pass_id: Uuid) {
        if changed.is_empty() {
            return;
        }
        if let Some(hook) = &self.coherence {
            hook.publish_declaration_changes(changed, pass_id).await;
        }
    }

    async fn publish_registered(&self, key: &str, owner_module: &str) {
        self.publisher
            .publish(ValueEvent::DeclarationRegistered {
                key: key.to_owned(),
                actor: owner_module.to_owned(),
            })
            .await;
    }
}

/// A refusal carried out of a transaction that committed nothing.
type Refusal = (&'static str, String);

fn refusal_of(key: &SettingKey, (code, message): Refusal) -> ContributionError {
    ContributionError {
        key: key.to_string(),
        code: code.to_owned(),
        message,
    }
}

#[async_trait]
impl<D, Cat, V, S> SettingsContributionClient for ContributionClient<D, Cat, V, S>
where
    D: DeclarationRepository + 'static,
    Cat: CategoryRepository + 'static,
    V: ValueRepository + 'static,
    S: AuditSink + 'static,
{
    async fn register_declarations(
        &self,
        _ctx: &SecurityContext,
        owner_module: String,
        declarations: Vec<ContributedDeclaration>,
    ) -> Result<ReconcileResult, CanonicalError> {
        // One id correlates every record this reconcile writes; there is no
        // HTTP request to take one from.
        let pass_id = Uuid::new_v4();
        let request_id = pass_id.to_string();
        let mut changed: Vec<String> = Vec::new();
        let mut result = ReconcileResult::default();
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-2
        for contributed in declarations {
            let service = Arc::clone(&self.service);
            let owner = owner_module.clone();
            let rid = request_id.clone();
            let key = contributed.key.clone();
            // Refusals come back as `Ok(Err(_))` so the transaction commits
            // what it did not write; an infrastructure failure is `Err` and
            // rolls back. The future owns its inputs: it must outlive any
            // transaction lifetime the database picks.
            let outcome: Result<Result<Outcome, Refusal>, DomainError> = self
                .db
                .db()
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        match service.reconcile_one(tx, &owner, &rid, &contributed).await {
                            Ok(outcome) => Ok(Ok(outcome)),
                            Err(ItemError::Refused { code, message }) => Ok(Err((code, message))),
                            Err(ItemError::Failed(err)) => Err(err),
                        }
                    })
                })
                .await;
            // A changed declaration changes what every scope resolves to —
            // its default, its traits, its very existence — so the key is
            // evicted whole and re-resolves lazily.
            if matches!(&outcome, Ok(Ok(o)) if *o != Outcome::Unchanged) {
                self.cache.invalidate_key(key.as_str());
            }
            // What peers must evict: a revive, an upgrade (both keys) and an
            // in-place reclassification; a new key has nothing cached and a
            // metadata-only edit changes nothing a cached value carries.
            match &outcome {
                Ok(Ok(Outcome::Reactivated | Outcome::Updated { reclassified: true })) => {
                    changed.push(key.to_string());
                }
                Ok(Ok(Outcome::Upgraded { retired })) => {
                    changed.push(key.to_string());
                    changed.push(retired.clone());
                }
                _ => {}
            }
            // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-upgrade:p1:inst-mc-up-5
            // An upgrade changes two keys: the successor, evicted above, and the
            // predecessor it retired, which must stop resolving as active.
            if let Ok(Ok(Outcome::Upgraded { retired })) = &outcome {
                self.cache.invalidate_key(retired);
            }
            // @cpt-end:cpt-cf-settings-service-algo-module-contributions-upgrade:p1:inst-mc-up-5
            // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-5
            // The declaration events, published once the reconcile is
            // durable: a consumer learns a setting appeared, moved to a new
            // major, came back, or had its metadata rewritten. No audit record
            // is written on this path — a declaration has no scope to write one
            // against — so these events are the whole of the trail, and the
            // metadata one matters most: it is the only way a contributed
            // declaration's gates can change at all.
            match &outcome {
                Ok(Ok(Outcome::Registered)) => {
                    self.publish_registered(key.as_str(), &owner_module).await;
                }
                Ok(Ok(Outcome::Upgraded { retired })) => {
                    self.publish_registered(key.as_str(), &owner_module).await;
                    self.publisher
                        .publish(ValueEvent::DeclarationRetired {
                            key: retired.clone(),
                            actor: owner_module.clone(),
                        })
                        .await;
                }
                Ok(Ok(Outcome::Reactivated)) => {
                    self.publisher
                        .publish(ValueEvent::DeclarationReactivated {
                            key: key.to_string(),
                            actor: owner_module.clone(),
                        })
                        .await;
                }
                Ok(Ok(Outcome::Updated { .. })) => {
                    self.publisher
                        .publish(ValueEvent::DeclarationUpdated {
                            key: key.to_string(),
                            actor: owner_module.clone(),
                        })
                        .await;
                }
                _ => {}
            }
            // @cpt-end:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-5
            match outcome {
                // An upgrade is a registration of the successor as far as the
                // counts go; what it retired is the predecessor, not one of the
                // keys the caller asked to retire.
                Ok(Ok(Outcome::Registered | Outcome::Upgraded { .. })) => result.registered += 1,
                Ok(Ok(Outcome::Updated { .. })) => result.updated += 1,
                Ok(Ok(Outcome::Reactivated)) => result.reactivated += 1,
                Ok(Ok(Outcome::Unchanged)) => {}
                Ok(Err(refusal)) => result.errors.push(refusal_of(&key, refusal)),
                Err(err) => {
                    // What committed before the failure is still announced.
                    self.publish_pass(changed, pass_id).await;
                    return Err(CanonicalError::from(err));
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-2
        // The pass's end: one broadcast for every key it changed.
        self.publish_pass(changed, pass_id).await;
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-6
        Ok(result)
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-6
    }

    // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-1
    async fn retire_declarations(
        &self,
        _ctx: &SecurityContext,
        owner_module: String,
        keys: Vec<SettingKey>,
    ) -> Result<ReconcileResult, CanonicalError> {
        // One id correlates every record this retire writes.
        let pass_id = Uuid::new_v4();
        let request_id = pass_id.to_string();
        let mut changed: Vec<String> = Vec::new();
        let mut result = ReconcileResult::default();
        for key in keys {
            let service = Arc::clone(&self.service);
            let owner = owner_module.clone();
            let rid = request_id.clone();
            let moved = key.clone();
            let outcome: Result<Result<bool, Refusal>, DomainError> = self
                .db
                .db()
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        match service.retire_one(tx, &owner, &rid, &moved).await {
                            Ok(retired) => Ok(Ok(retired)),
                            Err(ItemError::Refused { code, message }) => Ok(Err((code, message))),
                            Err(ItemError::Failed(err)) => Err(err),
                        }
                    })
                })
                .await;
            // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-5
            if matches!(outcome, Ok(Ok(true))) {
                self.cache.invalidate_key(key.as_str());
                changed.push(key.to_string());
                self.publisher
                    .publish(ValueEvent::DeclarationRetired {
                        key: key.to_string(),
                        actor: owner_module.clone(),
                    })
                    .await;
            }
            // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-5
            match outcome {
                Ok(Ok(true)) => result.retired += 1,
                Ok(Ok(false)) => {}
                Ok(Err(refusal)) => result.errors.push(refusal_of(&key, refusal)),
                Err(err) => {
                    self.publish_pass(changed, pass_id).await;
                    return Err(CanonicalError::from(err));
                }
            }
        }
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-6
        // The pass's end: one broadcast for every key it retired.
        self.publish_pass(changed, pass_id).await;
        Ok(result)
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-6
    }
    // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-1
}
