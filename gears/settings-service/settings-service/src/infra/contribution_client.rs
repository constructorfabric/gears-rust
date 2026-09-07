// Created: 2026-09-06 by Constructor Tech
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
use uuid::Uuid;

use crate::audit::AuditSink;
use crate::domain::category::CategoryRepository;
use crate::domain::contribution::{ContributionService, ItemError, Outcome};
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::resolution::EffectiveCache;
use crate::domain::value::ValueRepository;

/// The SDK trait over the reconciler and the database.
pub struct ContributionClient<D, Cat, V, S> {
    db: Arc<DBProvider<DbError>>,
    service: Arc<ContributionService<D, Cat, V, S>>,
    cache: Arc<EffectiveCache>,
}

impl<D, Cat, V, S> ContributionClient<D, Cat, V, S> {
    /// Serve the contract over this database and reconciler, evicting the
    /// effective-value cache for every declaration it changes.
    pub fn new(
        db: Arc<DBProvider<DbError>>,
        service: Arc<ContributionService<D, Cat, V, S>>,
        cache: Arc<EffectiveCache>,
    ) -> Self {
        Self { db, service, cache }
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
        let request_id = Uuid::new_v4().to_string();
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
            if matches!(outcome, Ok(Ok(o)) if o != Outcome::Unchanged) {
                self.cache.invalidate_key(key.as_str());
            }
            match outcome {
                Ok(Ok(Outcome::Registered)) => result.registered += 1,
                Ok(Ok(Outcome::Updated)) => result.updated += 1,
                Ok(Ok(Outcome::Reactivated)) => result.reactivated += 1,
                Ok(Ok(Outcome::Unchanged)) => {}
                Ok(Err(refusal)) => result.errors.push(refusal_of(&key, refusal)),
                Err(err) => return Err(CanonicalError::from(err)),
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-2
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
        let request_id = Uuid::new_v4().to_string();
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
            if matches!(outcome, Ok(Ok(true))) {
                self.cache.invalidate_key(key.as_str());
            }
            match outcome {
                Ok(Ok(true)) => result.retired += 1,
                Ok(Ok(false)) => {}
                Ok(Err(refusal)) => result.errors.push(refusal_of(&key, refusal)),
                Err(err) => return Err(CanonicalError::from(err)),
            }
        }
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-6
        Ok(result)
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-6
    }
    // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-1
}
