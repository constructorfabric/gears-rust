// Created: 2026-09-07 by Constructor Tech
//! Ports the write path depends on whose bindings arrive later or live in
//! infrastructure: the Secret Manager, the Change Publisher and the counters.

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Holds `secret`-trait plaintext outside this service and hands back a
/// reference. The real binding is the Credential Store; with nothing bound a
/// write to a secret setting is refused as unavailable rather than stored in
/// plaintext, and no handle resolves.
#[async_trait]
pub trait SecretManager: Send + Sync {
    /// Store `plaintext` for the setting at `tenant`, returning its reference.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when no store is bound or it cannot answer.
    async fn store_secret(
        &self,
        key: &str,
        tenant: Uuid,
        plaintext: &Value,
    ) -> Result<String, DomainError>;

    /// The plaintext behind `secret_ref`, stored for the setting at `tenant`.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] on the value when the store holds no entry,
    /// [`DomainError::Unavailable`] when it cannot answer.
    async fn resolve_plaintext(
        &self,
        key: &str,
        tenant: Uuid,
        secret_ref: &str,
    ) -> Result<String, DomainError>;

    /// Release the entry behind `secret_ref`; an absent entry is already done.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the store cannot answer.
    async fn delete_secret(
        &self,
        key: &str,
        tenant: Uuid,
        secret_ref: &str,
    ) -> Result<(), DomainError>;
}

/// The binding while no Credential Store is available.
pub struct NoSecretManager;

const NO_STORE: &str = "secret values are not supported: no Secret Manager is bound";

#[async_trait]
impl SecretManager for NoSecretManager {
    async fn store_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _plaintext: &Value,
    ) -> Result<String, DomainError> {
        Err(DomainError::Unavailable {
            detail: NO_STORE.to_owned(),
        })
    }

    async fn resolve_plaintext(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<String, DomainError> {
        Err(DomainError::Unavailable {
            detail: NO_STORE.to_owned(),
        })
    }

    async fn delete_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<(), DomainError> {
        Err(DomainError::Unavailable {
            detail: NO_STORE.to_owned(),
        })
    }
}

/// Decides whether a machine caller may resolve one setting's plaintext.
///
/// The decision is per setting: the resource is the value type and the
/// declaration is its id, so a service is granted the secrets it needs one at
/// a time or by a wider grant, never by holding the reader.
#[async_trait]
pub trait SecretResolveGate: Send + Sync {
    /// Refuse or permit `ctx` to resolve the declaration's plaintext.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] on the value when the decision is deny or
    /// cannot be obtained.
    async fn may_resolve(
        &self,
        ctx: &toolkit_security::SecurityContext,
        declaration_id: Uuid,
    ) -> Result<(), DomainError>;
}

/// What the write path publishes after a change is durably committed, or
/// after it was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueEvent {
    /// `event_value_changed`: a value was stored.
    Changed {
        /// The setting key.
        key: String,
        /// The scope, as a tenant id.
        tenant_id: Uuid,
        /// Who set it.
        actor: String,
        /// The change set the write belonged to.
        change_set_id: Uuid,
    },
    /// `event_value_change_failed`: a change was rejected, as a durable
    /// notification rather than only a response.
    ChangeFailed {
        /// The setting key.
        key: String,
        /// The scope, as a tenant id.
        tenant_id: Uuid,
        /// Who tried.
        actor: String,
        /// Why.
        reason: String,
    },
}

/// The Change Publisher port. R1 binds no broker: the binding logs.
#[async_trait]
pub trait ChangePublisher: Send + Sync {
    /// Publish one event; never fails the write it describes.
    async fn publish(&self, event: ValueEvent);
}

/// Counters the write path reports.
pub trait WriteMetrics: Send + Sync {
    /// `settings_value_writes_total` by result.
    fn value_write(&self, result: &'static str);
    /// `settings_step_up_total` by operation and result.
    fn step_up(&self, operation: &'static str, result: &'static str);
}

/// Counts nothing; the test binding.
pub struct NoMetrics;

impl WriteMetrics for NoMetrics {
    fn value_write(&self, _result: &'static str) {}
    fn step_up(&self, _operation: &'static str, _result: &'static str) {}
}
