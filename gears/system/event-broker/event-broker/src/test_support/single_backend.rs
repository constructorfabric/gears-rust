use std::sync::Arc;

use event_broker_sdk::backend::Backend;

use crate::domain::backend::BackendResolver;
use crate::domain::model::Topic;

/// Always the one backend it was constructed with, regardless of `topic`: a
/// test that exercises one backend needs no registry or routing around it.
pub struct SingleBackendResolver {
    backend: Arc<dyn Backend>,
}

impl SingleBackendResolver {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

impl BackendResolver for SingleBackendResolver {
    fn resolve(&self, _topic: &Topic) -> Arc<dyn Backend> {
        Arc::clone(&self.backend)
    }
}
