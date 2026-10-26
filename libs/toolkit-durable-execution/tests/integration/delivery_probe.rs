use async_trait::async_trait;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    BeforeEnqueue,
    AfterEnqueue,
    AfterIntentAck,
}
#[async_trait]
pub trait Probe: Send + Sync {
    /// Returning true injects a delivery failure at this named boundary.
    async fn fail_at(&self, phase: Phase) -> bool;
}
