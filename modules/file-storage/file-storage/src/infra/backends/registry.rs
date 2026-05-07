//! Immutable Backend Router (DECOMPOSITION 2.3).

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use file_storage_sdk::{Backend, BackendId};

use crate::domain::error::DomainError;

use super::r#trait::SharedBackend;

pub struct BackendRegistry {
    backends: HashMap<BackendId, SharedBackend>,
}

impl BackendRegistry {
    pub fn new(backends: HashMap<BackendId, SharedBackend>) -> Self {
        Self { backends }
    }

    /// Resolve a backend by id, applying the per-tenant access list.
    /// Returns `NotFound` for ids the tenant cannot see.
    pub fn resolve_visible(
        &self,
        backend_id: BackendId,
        tenant_id: Uuid,
    ) -> Result<Arc<dyn super::r#trait::StorageBackend>, DomainError> {
        let entry = self.backends.get(&backend_id).ok_or(DomainError::NotFound)?;
        if !entry.descriptor().is_visible_to(tenant_id) {
            return Err(DomainError::NotFound);
        }
        Ok(entry.clone())
    }

    /// Project the visible roster as SDK descriptors for `list_backends`.
    pub fn list_visible_to_tenant(&self, tenant_id: Uuid) -> Vec<Backend> {
        self.backends
            .values()
            .filter(|b| b.descriptor().is_visible_to(tenant_id))
            .map(|b| b.descriptor().sdk.clone())
            .collect()
    }

    /// Total number of registered backends.
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// Iterate over `(id, backend)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (BackendId, &SharedBackend)> {
        self.backends.iter().map(|(id, b)| (*id, b))
    }
}
