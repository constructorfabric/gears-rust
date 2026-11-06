//! Caller-aware type catalog over the managed Types Registry contracts.
//!
//! Built-in validation is a platform read. Policy management keeps the
//! request's identity. Missing schemas are absent from the result; a failed
//! batch, timeout or omitted answer fails the complete validation.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsTypeId, LifecycleStatus, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, ReadOptions,
    TypesRegistryApi, TypesRegistryApiExt,
};

use crate::domain::ports::{PortError, TypeCatalogPort, TypeCatalogScope};

/// Registry reads for startup and for the policy management caller.
pub struct RegistryTypeCatalog {
    caller: Arc<dyn TypesRegistryApi>,
    platform: Arc<dyn PlatformTypesRegistryApi>,
    timeout: Duration,
}

impl std::fmt::Debug for RegistryTypeCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryTypeCatalog")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl RegistryTypeCatalog {
    /// Reads through the selected identity, bounding the complete batch operation.
    #[must_use]
    pub fn new(
        caller: Arc<dyn TypesRegistryApi>,
        platform: Arc<dyn PlatformTypesRegistryApi>,
        timeout: Duration,
    ) -> Self {
        Self {
            caller,
            platform,
            timeout,
        }
    }
}

#[async_trait]
impl TypeCatalogPort for RegistryTypeCatalog {
    async fn known_types(
        &self,
        scope: TypeCatalogScope<'_>,
        ids: &[String],
    ) -> Result<HashSet<String>, PortError> {
        let keys: Vec<GtsTypeId> = ids
            .iter()
            .filter_map(|id| GtsTypeId::try_new(id.as_str()).ok())
            .collect();
        if keys.is_empty() {
            return Ok(HashSet::new());
        }
        let read = async {
            let options = ReadOptions::new().with_fresh(true);
            match scope {
                TypeCatalogScope::Caller(ctx) => {
                    self.caller
                        .batch_get_type_schemas(ctx, &keys, options)
                        .await
                }
                TypeCatalogScope::Platform => {
                    self.platform
                        .batch_get_type_schemas(
                            &PlatformSecurityContext::outbound_marker(),
                            &keys,
                            options,
                        )
                        .await
                }
            }
        };
        let answers = tokio::time::timeout(self.timeout, read)
            .await
            .map_err(|_| PortError::Timeout)?
            .map_err(|error| registry_error(scope, error))?;
        let mut known = HashSet::new();
        for id in keys {
            match answers.get(&id) {
                Some(Some(schema)) if schema.lifecycle_status == LifecycleStatus::Active => {
                    known.insert(id.to_string());
                }
                Some(_) => {}
                None => {
                    return Err(PortError::Unavailable(
                        "types registry omitted a requested type".to_owned(),
                    ));
                }
            }
        }
        Ok(known)
    }
}

fn registry_error(scope: TypeCatalogScope<'_>, error: CanonicalError) -> PortError {
    tracing::warn!(error = %error, "policy type catalog read failed");
    match (scope, error) {
        (TypeCatalogScope::Caller(_), CanonicalError::Unauthenticated { .. }) => {
            PortError::Unauthenticated
        }
        (TypeCatalogScope::Caller(_), CanonicalError::PermissionDenied { .. }) => {
            PortError::PermissionDenied
        }
        (_, error) => PortError::Unavailable(format!("types registry: {error}")),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
