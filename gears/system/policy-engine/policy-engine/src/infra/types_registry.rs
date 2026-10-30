//! [`TypeCatalogPort`] over the types-registry client.
//!
//! Resolved by the batch `get_type_schemas` call under the configured
//! registry bound. Identifiers the registry definitively does not know (not
//! found, or not a valid identifier) are absent from the result; anything
//! else - timeout, an outage, a per-item failure that is not a definitive
//! absence, an answer that omits a requested item - fails the whole call, so
//! a caller can never mistake an outage for an absence.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::TypesRegistryClient;

use crate::domain::ports::{PortError, TypeCatalogPort};

/// [`TypeCatalogPort`] asking a [`TypesRegistryClient`].
pub struct RegistryTypeCatalog {
    client: Arc<dyn TypesRegistryClient>,
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
    /// Asks `client`, bounding each call by `timeout`.
    #[must_use]
    pub fn new(client: Arc<dyn TypesRegistryClient>, timeout: Duration) -> Self {
        Self { client, timeout }
    }
}

#[async_trait]
impl TypeCatalogPort for RegistryTypeCatalog {
    async fn known_types(&self, ids: &[String]) -> Result<HashSet<String>, PortError> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut answers =
            tokio::time::timeout(self.timeout, self.client.get_type_schemas(ids.to_vec()))
                .await
                .map_err(|_| PortError::Timeout)?;
        let mut known = HashSet::new();
        for id in ids {
            match answers.remove(id) {
                Some(Ok(_)) => {
                    known.insert(id.clone());
                }
                Some(Err(
                    CanonicalError::NotFound { .. } | CanonicalError::InvalidArgument { .. },
                )) => {}
                Some(Err(err)) => {
                    return Err(PortError::Unavailable(format!("types registry: {err}")));
                }
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
