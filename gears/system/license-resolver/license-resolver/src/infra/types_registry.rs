//! Types-registry adapter behind the [`ContractRegistry`] port.
//!
//! The adapter caches nothing across calls: every check re-resolves through the
//! SDK so a contract update takes effect immediately. The types-registry local
//! client owns whatever short-lived caching it chooses to do internally.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, LifecycleStatus, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, ReadOptions, TypeSchema,
};

use crate::domain::{ContractRegistry, ContractRegistryError};

/// Resolves licensing contracts through the platform registry API: the
/// authored content, the resolved schema and the effective traits.
pub struct GtsContractRegistry {
    hub: Arc<ClientHub>,
}

impl GtsContractRegistry {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl ContractRegistry for GtsContractRegistry {
    async fn contract_schema(&self, type_id: &str) -> Result<TypeSchema, ContractRegistryError> {
        // Resolved per call rather than captured during `init()`: the platform
        // registry API is a consumed contract, wired after `init`.
        let registry = self
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| ContractRegistryError::Unavailable(e.to_string()))?;
        let id = GtsTypeId::try_new(type_id)
            .map_err(|e| ContractRegistryError::MalformedTypeId(e.to_string()))?;
        let options =
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::ResolvedSchema,
                EntityField::EffectiveTraits,
            ])));

        let contract = registry
            .get_type_schema(&PlatformSecurityContext::outbound_marker(), &id, options)
            .await
            .map_err(|err| match err {
                CanonicalError::NotFound { .. } => ContractRegistryError::Unregistered,
                // A malformed or kind-mismatched id is the caller's fault: the
                // registry reports it as InvalidArgument, not as an outage.
                CanonicalError::InvalidArgument { detail, .. } => {
                    ContractRegistryError::MalformedTypeId(detail)
                }
                other => ContractRegistryError::Unavailable(other.to_string()),
            })?;
        // A deleted contract is no longer registered. One the registry answered
        // without a selected artifact is returned as is: validation reports it
        // unusable, not an outage.
        if contract.lifecycle_status != LifecycleStatus::Active {
            return Err(ContractRegistryError::Unregistered);
        }
        Ok(contract)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
