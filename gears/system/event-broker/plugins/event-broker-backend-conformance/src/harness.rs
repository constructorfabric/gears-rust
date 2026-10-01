//! What a plugin supplies to run the suite against its medium.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::backend::Backend;
use tokio_util::sync::CancellationToken;

/// A plugin's medium, as the suite sees it.
#[async_trait]
pub trait Harness: Send + Sync {
    /// A backend over this harness's shared medium. Every call reaches the
    /// same storage, so a second call is a second instance of the same
    /// backend, or the same one after a restart. Its workers run until
    /// `stop` is cancelled.
    ///
    /// The backend's own retention should run often (well under a second),
    /// so retention scenarios finish within [`retention_deadline`](Self::retention_deadline).
    async fn build(&self, stop: CancellationToken) -> Arc<dyn Backend>;

    /// A backend over storage no other instance shares, for scenarios that
    /// observe the workers of one instance alone.
    async fn build_isolated(&self, stop: CancellationToken) -> Arc<dyn Backend>;

    /// A backend whose medium cannot be reached, built the way the plugin's
    /// provider builds one.
    async fn build_unreachable(&self, stop: CancellationToken) -> Unreachable;

    /// How long the backend's own retention may take to remove an event past
    /// its bound.
    fn retention_deadline(&self) -> Duration {
        Duration::from_secs(15)
    }
}

/// What [`Harness::build_unreachable`] has to offer.
pub enum Unreachable {
    /// A backend over a medium that is down.
    Backend(Arc<dyn Backend>),
    /// The plugin has no medium that can be down; the scenario is skipped
    /// and the reason printed.
    NotApplicable { reason: &'static str },
}
