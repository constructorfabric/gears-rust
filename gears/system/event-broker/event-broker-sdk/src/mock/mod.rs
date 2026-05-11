pub mod control;
pub mod core;

#[cfg(test)]
mod tests;

mod ingest;
mod rebalance;
mod storage;
mod stream;
pub mod stubs;
mod transport;

pub use control::{MockBrokerHandle, PartitionSlot};
pub use core::{CursorEntry, MockBroker, StoredEvent};

impl MockBroker {
    /// Get a test-facing handle for setup, fault injection, and assertions.
    pub fn handle(&self) -> MockBrokerHandle {
        MockBrokerHandle::from_broker(self)
    }
}
