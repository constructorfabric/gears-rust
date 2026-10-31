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

use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::activation::{PlaintextFetcher, PlaintextRequest};
use crate::domain::error::DomainError;
use crate::domain::ports::{SecretManager, SecretResolveGate};
use crate::infra::service_identity::service_context;
use crate::infra::storage::audit_store::AuditStore;

/// The fetch over the Secret Manager, the per-setting gate and the audit store.
pub struct ServicePlaintext {
    secrets: Arc<dyn SecretManager>,
    gate: Arc<dyn SecretResolveGate>,
    db: Arc<DBProvider<DbError>>,
}

impl ServicePlaintext {
    /// Over these ports and database.
    #[must_use]
    pub fn new(
        secrets: Arc<dyn SecretManager>,
        gate: Arc<dyn SecretResolveGate>,
        db: Arc<DBProvider<DbError>>,
    ) -> Self {
        Self { secrets, gate, db }
    }
}

#[async_trait]
impl PlaintextFetcher for ServicePlaintext {
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
            AuditOperation::SecretUse,
            Uuid::new_v4().to_string(),
        )
        .by_module()
        .with_post_image(AuditValue::Masked);
        AuditStore
            .append(&conn, &AccessScope::allow_all(), record)
            .await?;
        Ok(plaintext)
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-12
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-record:p1:inst-ca-record-10
    }
}
