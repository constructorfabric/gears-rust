use std::sync::Arc;

use event_broker_sdk::backend::Backend;

use crate::domain::model::Topic;

/// Resolves the backend for a topic. Takes `&Topic` because every call site
/// already holds the topic from `SpecificationManager`, and the topic is what
/// routing keys on.
///
/// `IngestServiceImpl`/`DeliveryServiceImpl` call the resolved
/// `event_broker_sdk::backend::Backend` directly for event
/// append/read/resolve/segments, with no repo-level indirection.
pub trait BackendResolver: Send + Sync {
    fn resolve(&self, topic: &Topic) -> Arc<dyn Backend>;
}
