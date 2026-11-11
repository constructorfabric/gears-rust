// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-record:p1
//! The plaintext fetch an activation state needs, under the service's own
//! identity.
//!
//! A `revert` or `remove_value` of a `secret`-trait setting leaves an inherited
//! secret in effect, and a batch `set` may adopt a secret staged earlier; in
//! both the gear holds no plaintext at the commit, while the expected value is
//! the hash of the value the consumer will read. The Secret Manager fetches it
//! once per change, under the key's lock and before the transaction.
//!
//! This extends the machine path of `resolve_plaintext` to two in-process uses:
//! each takes the same per-setting authorization decision — for the service's
//! own principal rather than a calling gear's — and writes the same
//! `secret_use` record, and the plaintext never leaves the process.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::activation::{PlaintextFetcher, PlaintextRequest};
use crate::domain::error::DomainError;
use crate::domain::ports::{SecretManager, SecretResolveGate};
use crate::infra::service_identity::service_context;
use crate::infra::storage::audit_store::AuditStore;

/// The fetch over the Secret Manager, the per-setting gate and the sink the
/// `secret_use` record is written through: the audit store in production.
pub struct ServicePlaintext<S = AuditStore> {
    secrets: Arc<dyn SecretManager>,
    gate: Arc<dyn SecretResolveGate>,
    db: Arc<DBProvider<DbError>>,
    sink: S,
}

impl ServicePlaintext {
    /// Over these ports and database, recording through the audit store.
    #[must_use]
    pub fn new(
        secrets: Arc<dyn SecretManager>,
        gate: Arc<dyn SecretResolveGate>,
        db: Arc<DBProvider<DbError>>,
    ) -> Self {
        Self::with_sink(secrets, gate, db, AuditStore)
    }
}

impl<S: AuditSink> ServicePlaintext<S> {
    /// As [`ServicePlaintext::new`], recording through `sink`.
    #[must_use]
    pub fn with_sink(
        secrets: Arc<dyn SecretManager>,
        gate: Arc<dyn SecretResolveGate>,
        db: Arc<DBProvider<DbError>>,
        sink: S,
    ) -> Self {
        Self {
            secrets,
            gate,
            db,
            sink,
        }
    }
}

#[async_trait]
impl<S: AuditSink + 'static> PlaintextFetcher for ServicePlaintext<S> {
    async fn fetch(&self, request: PlaintextRequest<'_>) -> Result<SecretString, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-10
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-12
        // The service's own identity, in the tenant of the change: the decision
        // for that identity first, before the store is asked anything.
        let ctx = service_context(request.audit_tenant)?;
        self.gate.may_resolve(&ctx, request.declaration_id).await?;
        let plaintext = self
            .secrets
            .resolve_plaintext(request.key, request.source_tenant, request.secret_ref)
            .await?;
        // One record per fetch, the value masked, attributed to the service's
        // identity and written on its own connection: it stays when the
        // change's transaction rolls back, since the fetch happened. A record
        // that cannot be written fails the fetch, and with it the change.
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::dependency_unavailable("database", "open a connection", e))?;
        let record = AuditRecord::new(
            request.key,
            Some(request.audit_tenant),
            ctx.subject_id().to_string(),
            ActorTenant::machine(),
            AuditOperation::SecretUse,
            Uuid::new_v4().to_string(),
        )
        .by_module()
        .with_post_image(AuditValue::Masked);
        self.sink
            .append(&conn, &AccessScope::allow_all(), record)
            .await?;
        Ok(plaintext)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-12
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-10
    }
}

#[cfg(test)]
#[path = "service_plaintext_tests.rs"]
mod tests;
