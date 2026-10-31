//! The in-process `SettingsReaderClient`.
//!
//! Bound into `ClientHub` at init and resolved by consumers for runtime
//! configuration. It is not gated by tenant access — it resolves configuration
//! for gears, not administrative visibility — and it never substitutes the
//! Schema Default on failure: `NotFound`, `Retired` and `Unavailable` reach the
//! consumer as themselves, so it can tell "give up" from "wait".

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::Value;
use settings_service_sdk::api::{BULK_LIMIT, BulkOutcome, BulkSelector, SettingsReaderClient};
use settings_service_sdk::models::{EffectiveValueResponse, GetEffectiveRequest, TrailEntry};
use settings_service_sdk::{SecretHandle, SettingKey};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::audit::AuditSink;
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::ports::{ReadMetrics, labels};
use crate::domain::resolution::{EffectiveValue, ReaderRead, ScopeTarget, ValueResolver};
use crate::domain::secrets::{SecretResolver, issue_handle};
use crate::domain::value::ValueRepository;

/// The SDK trait over the resolver, the database and the machine-only
/// plaintext path.
pub struct ReaderClient<D, V, A, S> {
    db: Arc<DBProvider<DbError>>,
    resolver: Arc<ValueResolver<D, V, A>>,
    secrets: Arc<SecretResolver<D, V, A, S>>,
    metrics: Arc<dyn ReadMetrics>,
}

impl<D, V, A, S> ReaderClient<D, V, A, S> {
    /// Serve the contract over this database, resolver and secret path,
    /// recording the reads on `metrics`.
    pub fn new(
        db: Arc<DBProvider<DbError>>,
        resolver: Arc<ValueResolver<D, V, A>>,
        secrets: Arc<SecretResolver<D, V, A, S>>,
        metrics: Arc<dyn ReadMetrics>,
    ) -> Self {
        Self {
            db,
            resolver,
            secrets,
            metrics,
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

/// A connection that cannot be had is a dependency that cannot answer.
fn conn_failure(err: &DbError) -> DomainError {
    DomainError::dependency_unavailable("database", "open a connection", err)
}

fn conn_error(err: &DbError) -> CanonicalError {
    CanonicalError::from(conn_failure(err))
}

/// The `resolve_failure` reason of a failed read: only a dependency that
/// cannot answer or the service's own failure count; a refusal about the
/// request or the setting (not found, retired, validation) is neither.
fn failure_reason(err: &DomainError) -> Option<&'static str> {
    match err {
        DomainError::Unavailable { .. } => Some(labels::UNAVAILABLE),
        DomainError::Internal { .. } => Some(labels::INTERNAL),
        _ => None,
    }
}

impl<D, V, A, S> ReaderClient<D, V, A, S> {
    /// Count a failure of the read as the request's or the key's own.
    fn record_failure(&self, err: &DomainError) {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-14
        if let Some(reason) = failure_reason(err) {
            self.metrics.resolve_failure(reason);
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-14
    }

    /// Count the failure of a bulk read as a whole: one count for the request,
    /// none for a refusal of the request itself.
    fn record_request_failure(&self, err: &DomainError) {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-7
        if let Some(reason) = failure_reason(err) {
            self.metrics.resolve_failure(reason);
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-7
    }

    /// Project a resolved read and, once it projects, record it — one lookup
    /// and one duration. A projection failure is the service's own and counts
    /// only the `internal` failure: no lookup and no duration.
    fn deliver(
        &self,
        scope: String,
        read: &ReaderRead,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        let response = project(scope, &read.value).inspect_err(|_| {
            // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-14
            self.metrics.resolve_failure(labels::INTERNAL);
            // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-14
        })?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-13
        let class = read.value.scope_class;
        let result = if read.source == labels::CACHE {
            labels::HIT
        } else {
            labels::MISS
        };
        self.metrics.lookup(result, class);
        self.metrics
            .read(read.source, class, read.elapsed.as_secs_f64());
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-13
        Ok(response)
    }
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
        let conn = self.db.conn().map_err(|e| {
            let err = conn_failure(&e);
            self.record_failure(&err);
            CanonicalError::from(err)
        })?;
        let read = self
            .resolver
            .resolve_for_reader(&conn, &req.key, target)
            .await
            .inspect_err(|err| self.record_failure(err))?;
        self.deliver(req.scope, &read)
    }

    async fn get_effective_bulk(
        &self,
        _ctx: &SecurityContext,
        selector: BulkSelector,
        scope: String,
    ) -> Result<Vec<BulkOutcome>, CanonicalError> {
        // Every failure before the first key is the request's, answered once
        // at the top: no key could be attempted, and copying the refusal onto
        // each key would leave nowhere to put it when there is no key — a
        // category that cannot be enumerated — and would read as that many
        // independent failures when there is.
        let (keys, field) = match &selector {
            BulkSelector::Keys(keys) => (keys.clone(), "keys"),
            BulkSelector::Category(category) => (
                self.keys_in_category(category)
                    .await
                    .inspect_err(|err| self.record_request_failure(err))?,
                "category",
            ),
        };
        // Bounded, and never partial: past the bound nothing is resolved, so
        // no caller takes a truncated set for the whole list or category.
        if keys.len() > BULK_LIMIT {
            return Err(CanonicalError::from(DomainError::Validation {
                field: field.to_owned(),
                code: crate::field::BULK_TOO_LARGE,
                message: format!(
                    "a bulk read resolves at most {BULK_LIMIT} settings; ask for fewer keys or a \
                     smaller category"
                ),
            }));
        }
        let target = ScopeTarget::parse(&scope)?;
        let conn = self.db.conn().map_err(|e| {
            let err = conn_failure(&e);
            self.record_request_failure(&err);
            CanonicalError::from(err)
        })?;
        let reads = self
            .resolver
            .resolve_bulk_for_reader(&conn, &keys, target)
            .await
            .inspect_err(|err| self.record_request_failure(err))?;
        Ok(reads
            .into_iter()
            .map(|(key, outcome)| BulkOutcome {
                key,
                result: match outcome {
                    Ok(read) => self.deliver(scope.clone(), &read),
                    Err(err) => {
                        self.record_failure(&err);
                        Err(CanonicalError::from(err))
                    }
                },
            })
            .collect())
    }

    async fn resolve_secret(
        &self,
        ctx: &SecurityContext,
        handle: SecretHandle,
    ) -> Result<SecretString, CanonicalError> {
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
    async fn keys_in_category(&self, category: &str) -> Result<Vec<SettingKey>, DomainError> {
        let category_id = Uuid::parse_str(category).map_err(|_| DomainError::Validation {
            field: "category".to_owned(),
            code: crate::field::VALIDATION,
            message: format!("`{category}` is not a category id"),
        })?;
        let conn = self.db.conn().map_err(|e| conn_failure(&e))?;
        let declarations = self
            .resolver
            .declarations_in_category(&conn, &AccessScope::allow_all(), category_id)
            .await?;
        // A stored key that does not parse is a broken row, not a setting to
        // leave out: the category is refused whole rather than shortened by
        // one, since no `SettingKey` exists to hang a per-key outcome on.
        declarations
            .iter()
            .map(|d| {
                SettingKey::parse(&d.key).map_err(|err| DomainError::Internal {
                    diagnostic: format!(
                        "declaration {} carries a key that does not parse: {err}",
                        d.id
                    ),
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "reader_client_tests.rs"]
mod reader_client_tests;
