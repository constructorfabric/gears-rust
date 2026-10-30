//! What a plugin supplies to run the bench against its medium.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::backend::Backend;

/// How big a phase's backend instance should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    /// Connections the instance may hold at once, for a backend that pools
    /// them: the phase's concurrent callers plus headroom, or `--pool-size`.
    pub connections: u32,
}

/// A plugin's medium, as the bench sees it.
#[async_trait]
pub trait Harness: Send + Sync {
    /// A backend over this harness's medium, built and not started: the bench
    /// starts it, and stops it once the phase is over. Called once per phase, so no phase inherits another's
    /// in-process state; the bench waits for the instance to answer before
    /// loading it. A backend without a connection pool ignores `sizing`.
    async fn build(&self, sizing: Sizing) -> Arc<dyn Backend>;

    /// The label the report gives this backend, such as `memory`.
    fn name(&self) -> &str;
}
