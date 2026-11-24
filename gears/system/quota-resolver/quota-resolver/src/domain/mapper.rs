//! Principal type to quota subject type mapping.

use std::collections::{BTreeMap, HashMap};
use std::sync::{PoisonError, RwLock};
use std::time::Duration;

use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    LifecycleStatus, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, ReadOptions,
};

/// Bounds the read that says whether a principal type is registered. Expiry
/// is a retryable outage and memoizes nothing.
const LOOKUP_BUDGET: Duration = Duration::from_secs(2);

/// Maps a principal type to the quota subject type that configuration names for
/// it or for its nearest ancestor.
///
/// A registered type keeps its answer for the life of the process, because a
/// GTS type id never changes its ancestors. An unregistered type is asked again
/// on every call.
pub struct PrincipalMapper {
    entries: BTreeMap<String, String>,
    resolved: RwLock<HashMap<String, Option<String>>>,
}

impl PrincipalMapper {
    #[must_use]
    pub fn new(entries: BTreeMap<String, String>) -> Self {
        Self {
            entries,
            resolved: RwLock::new(HashMap::new()),
        }
    }

    /// The quota subject type for `principal_type`, or `None` when the type is
    /// absent, malformed, unregistered, deleted, or has no entry on its chain.
    ///
    /// # Errors
    ///
    /// `ServiceUnavailable` when types-registry cannot say whether the type is
    /// registered.
    pub async fn map(
        &self,
        hub: &ClientHub,
        principal_type: Option<&str>,
    ) -> Result<Option<String>, CanonicalError> {
        let Some(principal_type) = principal_type else {
            return Ok(None);
        };
        if let Some(subject_type) = self.entries.get(principal_type) {
            return Ok(Some(subject_type.clone()));
        }
        if self.entries.is_empty() {
            return Ok(None);
        }
        let Ok(type_id) = gts::GtsTypeId::try_new(principal_type) else {
            return Ok(None);
        };
        let known = self
            .resolved
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(principal_type)
            .cloned();
        if let Some(known) = known {
            return Ok(known);
        }

        let registry = hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| unavailable(&e))?;
        let caller = PlatformSecurityContext::outbound_marker();
        let read = registry.get_type_schema(&caller, &type_id, ReadOptions::default());
        let Ok(answer) = tokio::time::timeout(LOOKUP_BUDGET, read).await else {
            tracing::warn!(
                principal_type,
                budget = ?LOOKUP_BUDGET,
                "types-registry did not say in time whether the principal type is registered"
            );
            return Err(unavailable(&format_args!(
                "no answer within {LOOKUP_BUDGET:?}"
            )));
        };
        match answer {
            Ok(schema) if schema.lifecycle_status == LifecycleStatus::Active => {
                let mapped = self.nearest_ancestor_entry(principal_type);
                self.resolved
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(principal_type.to_owned(), mapped.clone());
                Ok(mapped)
            }
            Ok(_)
            | Err(CanonicalError::NotFound { .. } | CanonicalError::InvalidArgument { .. }) => {
                Ok(None)
            }
            Err(e) => {
                tracing::warn!(
                    principal_type,
                    error = %e,
                    "types-registry failed to say whether the principal type is registered"
                );
                Err(super::service::registry_failure(&e))
            }
        }
    }

    // Every `~` but the last ends an ancestor of the type.
    fn nearest_ancestor_entry(&self, type_id: &str) -> Option<String> {
        type_id
            .match_indices('~')
            .rev()
            .skip(1)
            .find_map(|(end, _)| self.entries.get(&type_id[..=end]))
            .cloned()
    }
}

fn unavailable(cause: &dyn std::fmt::Display) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(format!("types-registry is not available: {cause}"))
        .create()
}
