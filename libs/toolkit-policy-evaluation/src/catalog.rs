//! The set of backends a process can select from.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::backend::EvaluationBackend;
use crate::rego::{REGO_BACKEND_ID, RegoBackend};

/// Backends keyed by their id.
#[derive(Default)]
pub struct BackendCatalog {
    backends: BTreeMap<String, Arc<dyn EvaluationBackend>>,
}

impl BackendCatalog {
    /// The catalog of every backend this crate carries (today: Rego,
    /// [`REGO_BACKEND_ID`]).
    #[must_use]
    pub fn with_default_backends() -> Self {
        let mut backends: BTreeMap<String, Arc<dyn EvaluationBackend>> = BTreeMap::new();
        backends.insert(REGO_BACKEND_ID.to_owned(), Arc::new(RegoBackend::new()));
        Self { backends }
    }

    /// The backend registered under `backend_id`, if any.
    #[must_use]
    pub fn get(&self, backend_id: &str) -> Option<Arc<dyn EvaluationBackend>> {
        self.backends.get(backend_id).cloned()
    }
}

impl fmt::Debug for BackendCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackendCatalog")
            .field("backends", &self.backends.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "catalog_tests.rs"]
mod catalog_tests;
