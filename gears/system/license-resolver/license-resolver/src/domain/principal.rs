//! Mapping of principal types to Subject contract types.

use std::collections::HashMap;
use std::sync::Arc;

use gts::GtsTypeId;
use toolkit_macros::domain_model;

use super::error::DomainError;
use super::ports::{ContractRegistry, ContractRegistryError};

#[domain_model]
pub struct PrincipalMapper {
    subject_types: HashMap<String, GtsTypeId>,
    registry: Arc<dyn ContractRegistry>,
}

impl PrincipalMapper {
    #[must_use]
    pub fn new(
        subject_types: HashMap<String, GtsTypeId>,
        registry: Arc<dyn ContractRegistry>,
    ) -> Self {
        Self {
            subject_types,
            registry,
        }
    }

    /// The Subject contract type for `principal_type`: its own entry, else the
    /// entry of its nearest ancestor. `None` when the principal is not mapped,
    /// including an absent, malformed or unregistered principal type.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] — the ancestors could not be
    /// resolved.
    pub async fn subject_type(
        &self,
        principal_type: Option<&str>,
    ) -> Result<Option<GtsTypeId>, DomainError> {
        let Some(principal_type) = principal_type else {
            return Ok(None);
        };
        if let Some(subject_type) = self.subject_types.get(principal_type) {
            return Ok(Some(subject_type.clone()));
        }
        if self.subject_types.is_empty() {
            return Ok(None);
        }
        match self.registry.contract_schema(principal_type).await {
            Ok(schema) => Ok(schema
                .ancestors()
                .find_map(|ancestor| self.subject_types.get(ancestor.type_id.as_ref()))
                .cloned()),
            Err(
                ContractRegistryError::Unregistered | ContractRegistryError::MalformedTypeId(_),
            ) => Ok(None),
            Err(ContractRegistryError::Unavailable(reason)) => {
                Err(DomainError::TypesRegistryUnavailable(reason))
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "principal_tests.rs"]
mod principal_tests;
