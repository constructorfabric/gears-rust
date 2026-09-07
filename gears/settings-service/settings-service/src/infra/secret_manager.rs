// Created: 2026-09-07 by Constructor Tech
//! The Secret Manager over the Credential Store, and the per-setting gate of
//! the machine path.
//!
//! Every entry is written `private` to a principal that exists only for this
//! gear, in the tenant the value belongs to, under a reference derived from
//! the setting and the tenant. Ownership is what keeps "nowhere else" true
//! beyond this service's tables: a tenant's users hold no path to the entry
//! through the store's own API.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, SecretRef, SecretValue, SharingMode, WritePrecondition,
};
use serde_json::Value;
use settings_service_sdk::gts::VALUE_SCHEMA;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::domain::error::DomainError;
use crate::domain::ports::{SecretManager, SecretResolveGate};

/// The name the gear's store namespace and principal derive from.
const NAMESPACE_NAME: &[u8] = b"urn:constructorfabric:gears:settings-service";

/// The Secret Manager bound to `credstore`.
// @cpt-dod:cpt-cf-settings-service-dod-secret-values-manager:p1
pub struct CredStoreSecretManager {
    client: Arc<dyn CredStoreClientV1>,
    namespace: Uuid,
    principal: Uuid,
}

impl CredStoreSecretManager {
    /// Bind to the store's client.
    #[must_use]
    pub fn new(client: Arc<dyn CredStoreClientV1>) -> Self {
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_URL, NAMESPACE_NAME);
        let principal = Uuid::new_v5(&namespace, b"principal");
        Self {
            client,
            namespace,
            principal,
        }
    }

    /// The fixed principal this gear presents to the store.
    #[must_use]
    pub fn principal(&self) -> Uuid {
        self.principal
    }

    /// The prefix every entry for `(key, tenant)` is stored under.
    #[must_use]
    pub fn reference_prefix(&self, key: &str, tenant: Uuid) -> String {
        // @cpt-begin:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-1
        // The key's characters (`.`, `~`) are outside the store's alphabet; a
        // name-based UUID of the key keeps its identity without them.
        let key_id = Uuid::new_v5(&self.namespace, key.as_bytes());
        format!("cf-settings-{key_id}-{tenant}")
        // @cpt-end:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-1
    }

    /// A reference for one write of `(key, tenant)`: the prefix and a nonce.
    #[must_use]
    pub fn reference(&self, key: &str, tenant: Uuid) -> String {
        // @cpt-begin:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-2
        // Unique per write, so a set that fails after the store leg leaves the
        // live entry untouched and a superseded entry can be released by name.
        // 118 characters of `[a-z0-9-]`, within the store's alphabet and length.
        format!(
            "{}-{}",
            self.reference_prefix(key, tenant),
            Uuid::new_v4().simple()
        )
        // @cpt-end:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-2
    }

    fn context(&self, tenant: Uuid) -> Result<SecurityContext, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-3
        // @cpt-begin:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-4
        // The target tenant is the subject tenant, so the entry lives where the
        // value belongs; the subject is this gear's principal, so only this
        // gear reads it back; no subject type, since it is no user.
        SecurityContext::builder()
            .subject_id(self.principal)
            .subject_tenant_id(tenant)
            .build()
            .map_err(|err| DomainError::Internal {
                diagnostic: format!("store context: {err}"),
            })
        // @cpt-end:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-4
        // @cpt-end:cpt-cf-settings-service-algo-secret-values-reference:p1:inst-sv-ref-3
    }

    fn parse_ref(secret_ref: &str) -> Result<SecretRef, DomainError> {
        SecretRef::new(secret_ref).map_err(|err| DomainError::Internal {
            diagnostic: format!("stored secret reference does not parse: {err}"),
        })
    }
}

fn unavailable(operation: &str, err: &CredStoreError) -> DomainError {
    DomainError::Unavailable {
        detail: format!("the credential store could not {operation}: {err}"),
    }
}

/// The bytes stored for a JSON plaintext: a string as itself, anything else
/// as its JSON text, so a string-shaped secret type round-trips byte for byte.
fn bytes_of(plaintext: &Value) -> Vec<u8> {
    match plaintext {
        Value::String(s) => s.as_bytes().to_vec(),
        other => serde_json::to_vec(other).unwrap_or_default(),
    }
}

#[async_trait]
impl SecretManager for CredStoreSecretManager {
    async fn store_secret(
        &self,
        key: &str,
        tenant: Uuid,
        plaintext: &Value,
    ) -> Result<String, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-3
        let reference = self.reference(key, tenant);
        let secret_ref = Self::parse_ref(&reference)?;
        let ctx = self.context(tenant)?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-3
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-4
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-5
        // Create-only under a fresh reference: nothing this write does can
        // touch the entry the row currently holds.
        self.client
            .create(
                &ctx,
                &secret_ref,
                SecretValue::new(bytes_of(plaintext)),
                SharingMode::Private,
            )
            .await
            .map_err(|err| unavailable("store the secret", &err))?;
        Ok(reference)
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-5
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-4
    }

    async fn resolve_plaintext(
        &self,
        _key: &str,
        tenant: Uuid,
        secret_ref: &str,
    ) -> Result<String, DomainError> {
        let reference = Self::parse_ref(secret_ref)?;
        let ctx = self.context(tenant)?;
        let found = match self.client.get(&ctx, &reference).await {
            Ok(found) => found,
            Err(CredStoreError::NotFound) => None,
            Err(err) => return Err(unavailable("resolve the secret", &err)),
        };
        let Some(entry) = found else {
            return Err(DomainError::NotFound {
                resource: VALUE_SCHEMA,
            });
        };
        String::from_utf8(entry.value.as_bytes().to_vec()).map_err(|_| DomainError::Internal {
            diagnostic: "the stored secret is not UTF-8 text".to_owned(),
        })
    }

    async fn delete_secret(
        &self,
        _key: &str,
        tenant: Uuid,
        secret_ref: &str,
    ) -> Result<(), DomainError> {
        let reference = Self::parse_ref(secret_ref)?;
        let ctx = self.context(tenant)?;
        match self
            .client
            .delete(&ctx, &reference, WritePrecondition::Exists)
            .await
        {
            Ok(()) | Err(CredStoreError::NotFound) => Ok(()),
            Err(err) => Err(unavailable("release the secret", &err)),
        }
    }
}

/// The per-setting gate of the machine path: `read` on the value resource
/// naming the declaration, decided by the `PolicyEnforcer`.
pub struct PepSecretGate {
    enforcer: Arc<PolicyEnforcer>,
}

impl PepSecretGate {
    /// Gate over the gear's enforcer.
    #[must_use]
    pub fn new(enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { enforcer }
    }
}

#[async_trait]
impl SecretResolveGate for PepSecretGate {
    async fn may_resolve(
        &self,
        ctx: &SecurityContext,
        declaration_id: Uuid,
    ) -> Result<(), DomainError> {
        authz::access_scope(
            &self.enforcer,
            ctx,
            &resource::VALUE,
            "read",
            Some(declaration_id),
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "secret_manager_tests.rs"]
mod secret_manager_tests;
