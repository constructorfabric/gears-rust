// Created: 2026-09-06 by Constructor Tech
//! The in-process `SettingsReaderClient`.
//!
//! Bound into `ClientHub` at init and resolved by consumers for runtime
//! configuration. It is not gated by tenant access — it resolves configuration
//! for gears, not administrative visibility — and it never substitutes the
//! Schema Default on failure: `NotFound`, `Retired` and `Unavailable` reach the
//! consumer as themselves, so it can tell "give up" from "wait".

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use settings_service_sdk::api::{BulkOutcome, BulkSelector, SettingsReaderClient};
use settings_service_sdk::models::{EffectiveValueResponse, GetEffectiveRequest, TrailEntry};
use settings_service_sdk::{SecretHandle, SettingKey};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::audit::AuditSink;
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, ScopeTarget, ValueResolver};
use crate::domain::secrets::{SecretResolver, issue_handle};
use crate::domain::value::ValueRepository;

/// The SDK trait over the resolver, the database and the machine-only
/// plaintext path.
pub struct ReaderClient<D, V, A, S> {
    db: Arc<DBProvider<DbError>>,
    resolver: Arc<ValueResolver<D, V, A>>,
    secrets: Arc<SecretResolver<D, V, A, S>>,
}

impl<D, V, A, S> ReaderClient<D, V, A, S> {
    /// Serve the contract over this database, resolver and secret path.
    pub fn new(
        db: Arc<DBProvider<DbError>>,
        resolver: Arc<ValueResolver<D, V, A>>,
        secrets: Arc<SecretResolver<D, V, A, S>>,
    ) -> Self {
        Self {
            db,
            resolver,
            secrets,
        }
    }
}

/// The consumer projection: no setter identity and no timestamps on the trail,
/// so an ancestor's administrator is not exposed to a subordinate tenant.
fn project(
    scope: String,
    value: &EffectiveValue,
) -> Result<EffectiveValueResponse, CanonicalError> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-7
    let inheritance_trail = value
        .trail
        .iter()
        .map(|e| TrailEntry {
            scope: e.scope.clone(),
            has_override: e.has_override,
            provided_value: e.provided_value,
        })
        .collect();
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-7
    let key = SettingKey::parse(&value.key).map_err(|e| {
        CanonicalError::from(DomainError::Internal {
            diagnostic: format!("stored key `{}` does not parse: {e}", value.key),
        })
    })?;
    // @cpt-begin:cpt-cf-settings-service-flow-secret-values-admin-read:p1:inst-sv-aread-3
    // A secret-classified value goes out as the handle for the requested scope,
    // configured or not: the shape does not disclose which, and the reference
    // the resolver holds never leaves.
    let projected = if value.data_classification == "secret" {
        Value::String(issue_handle(&value.key, &scope).as_token().to_owned())
    } else {
        value.value.clone()
    };
    // @cpt-end:cpt-cf-settings-service-flow-secret-values-admin-read:p1:inst-sv-aread-3
    Ok(EffectiveValueResponse {
        key,
        scope,
        value: projected,
        source: value.source,
        source_scope: value.source_scope.clone(),
        traits: value.traits.clone(),
        inheritance_trail,
    })
}

fn conn_error(err: &DbError) -> CanonicalError {
    CanonicalError::from(DomainError::Internal {
        diagnostic: err.to_string(),
    })
}

#[async_trait]
impl<D, V, A, S> SettingsReaderClient for ReaderClient<D, V, A, S>
where
    D: DeclarationRepository + 'static,
    V: ValueRepository + 'static,
    A: crate::domain::access::AccessRepository + 'static,
    S: AuditSink + 'static,
{
    async fn get_effective(
        &self,
        _ctx: &SecurityContext,
        req: GetEffectiveRequest,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        let target = ScopeTarget::parse(&req.scope)?;
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        let resolved = self.resolver.resolve(&conn, &req.key, target).await?;
        project(req.scope, &resolved)
    }

    async fn get_effective_bulk(
        &self,
        _ctx: &SecurityContext,
        selector: BulkSelector,
        scope: String,
    ) -> Vec<BulkOutcome> {
        let keys: Vec<SettingKey> = match &selector {
            BulkSelector::Keys(keys) => keys.clone(),
            BulkSelector::Category(category) => match self.keys_in_category(category).await {
                Ok(keys) => keys,
                // No key is known, so no per-key outcome can carry the failure;
                // an empty batch is the only honest answer for a category.
                Err(_) => return Vec::new(),
            },
        };
        let target = match ScopeTarget::parse(&scope) {
            Ok(target) => target,
            Err(err) => {
                let canonical = CanonicalError::from(err);
                return keys
                    .into_iter()
                    .map(|key| BulkOutcome {
                        key,
                        result: Err(canonical.clone()),
                    })
                    .collect();
            }
        };
        let conn = match self.db.conn() {
            Ok(conn) => conn,
            Err(err) => {
                let canonical = conn_error(&err);
                return keys
                    .into_iter()
                    .map(|key| BulkOutcome {
                        key,
                        result: Err(canonical.clone()),
                    })
                    .collect();
            }
        };
        self.resolver
            .resolve_bulk(&conn, &keys, target)
            .await
            .into_iter()
            .map(|(key, outcome)| BulkOutcome {
                key,
                result: outcome
                    .map_err(CanonicalError::from)
                    .and_then(|v| project(scope.clone(), &v)),
            })
            .collect()
    }

    async fn resolve_secret(
        &self,
        ctx: &SecurityContext,
        handle: SecretHandle,
    ) -> Result<String, CanonicalError> {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-1
        // The one plaintext path. The domain decides; this adapter only
        // supplies the connection and projects the outcome.
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        self.secrets
            .resolve(&conn, ctx, &handle)
            .await
            .map_err(CanonicalError::from)
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-1
    }
}

impl<D, V, A, S> ReaderClient<D, V, A, S>
where
    D: DeclarationRepository,
    V: ValueRepository,
    A: crate::domain::access::AccessRepository,
    S: AuditSink,
{
    async fn keys_in_category(&self, category: &str) -> Result<Vec<SettingKey>, CanonicalError> {
        let category_id = Uuid::parse_str(category).map_err(|_| {
            CanonicalError::from(DomainError::Validation {
                field: "category".to_owned(),
                code: crate::field::VALIDATION,
                message: format!("`{category}` is not a category id"),
            })
        })?;
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        let declarations = self
            .resolver
            .declarations_in_category(&conn, &AccessScope::allow_all(), category_id)
            .await?;
        Ok(declarations
            .iter()
            .filter_map(|d| SettingKey::parse(&d.key).ok())
            .collect())
    }
}

#[cfg(test)]
#[path = "reader_client_tests.rs"]
mod reader_client_tests;
