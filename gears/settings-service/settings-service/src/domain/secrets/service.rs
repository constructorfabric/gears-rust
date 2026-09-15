// Created: 2026-09-07 by Constructor Tech
//! The machine-only plaintext path.
//!
//! Reached through the SDK reader and nowhere else. The caller is authorized
//! for the one setting the handle names before the store is asked anything,
//! the plaintext is fetched for the row that won the resolution, and one
//! `secret_use` record is stored per resolution with the value masked. Nothing
//! here is cached: the effective cache keeps holding the reference only.

use std::sync::Arc;

use settings_service_sdk::gts::VALUE_SCHEMA;
use settings_service_sdk::{SecretHandle, SettingKey};
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::access::AccessRepository;
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::ports::{SecretManager, SecretResolveGate};
use crate::domain::resolution::{ScopeTarget, ValueResolver};
use crate::domain::secrets::decode_handle;
use crate::domain::value::ValueRepository;
use crate::field;

/// Resolves a [`SecretHandle`] to plaintext for an authorized machine caller.
// @cpt-dod:cpt-cf-settings-service-dod-secret-values-machine-path:p1
pub struct SecretResolver<D, V, A, S> {
    resolver: Arc<ValueResolver<D, V, A>>,
    secrets: Arc<dyn SecretManager>,
    gate: Arc<dyn SecretResolveGate>,
    sink: S,
}

impl<D, V, A, S> SecretResolver<D, V, A, S>
where
    D: DeclarationRepository,
    V: ValueRepository,
    A: AccessRepository,
    S: AuditSink,
{
    /// Assemble the path over the resolver, the Secret Manager, the
    /// per-setting gate and the audit sink.
    pub fn new(
        resolver: Arc<ValueResolver<D, V, A>>,
        secrets: Arc<dyn SecretManager>,
        gate: Arc<dyn SecretResolveGate>,
        sink: S,
    ) -> Self {
        Self {
            resolver,
            secrets,
            gate,
            sink,
        }
    }

    /// The plaintext behind `handle`, for `ctx`.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for a malformed handle or one naming a
    /// setting that is not secret-classified; [`DomainError::NotFound`] on the
    /// declaration when none exists and on the value when no credential is
    /// configured; [`DomainError::Retired`]; [`DomainError::Unauthorized`] when
    /// the caller may not read that setting; [`DomainError::Unavailable`] when
    /// the store or the audit store cannot answer.
    pub async fn resolve<C: DBRunner>(
        &self,
        conn: &C,
        ctx: &SecurityContext,
        handle: &SecretHandle,
    ) -> Result<String, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-2
        let claims = decode_handle(handle)?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-2
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-3
        // @cpt-begin:cpt-cf-settings-service-algo-secret-values-handle:p1:inst-sv-handle-3
        // Resolved again, now: the handle asked for a scope, and whatever wins
        // there at this moment is what the caller gets.
        let key = SettingKey::parse(&claims.key).map_err(|_| DomainError::Validation {
            field: "handle".to_owned(),
            code: field::SECRET_HANDLE_MALFORMED,
            message: "the secret handle names no valid setting key".to_owned(),
        })?;
        let target = ScopeTarget::parse(&claims.scope)?;
        let effective = self.resolver.resolve(conn, &key, target).await?;
        if effective.data_classification != "secret" {
            return Err(DomainError::Validation {
                field: "handle".to_owned(),
                code: field::NOT_A_SECRET,
                message: "the handle names a setting that is not secret-classified".to_owned(),
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-secret-values-handle:p1:inst-sv-handle-3
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-3
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-4
        // Before the store is asked anything, and whether or not a credential
        // exists: a caller without the grant learns nothing either way.
        self.gate.may_resolve(ctx, effective.declaration_id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-4
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-5
        // The placeholder default is not a credential. `NotFound` on the value
        // is what the SDK projects to `SecretNotConfigured`.
        let Some(secret_ref) = effective
            .secret_backed
            .then(|| effective.value.as_str())
            .flatten()
        else {
            return Err(DomainError::NotFound {
                resource: VALUE_SCHEMA,
            });
        };
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-5
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-6
        // The entry lives in the tenant whose row won, which for an inherited
        // value is an ancestor of the requested scope.
        let root = self.resolver.root_tenant().await?;
        let source_tenant = match &effective.source_scope {
            Some(scope) => ScopeTarget::parse(scope)?.tenant_id(root),
            None => effective.tenant_id,
        };
        let plaintext = self
            .secrets
            .resolve_plaintext(&effective.key, source_tenant, secret_ref)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-6
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-7
        // One record per resolution, the value masked, before any plaintext
        // leaves: a resolution that cannot be recorded does not happen.
        let record = AuditRecord::new(
            effective.key.as_str(),
            Some(effective.tenant_id),
            ctx.subject_id().to_string(),
            AuditOperation::SecretUse,
            Uuid::new_v4().to_string(),
        )
        .by_module()
        .with_post_image(AuditValue::Masked);
        self.sink
            .append(conn, &AccessScope::allow_all(), record)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-7
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-8
        Ok(plaintext)
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-resolve:p1:inst-sv-resolve-8
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
