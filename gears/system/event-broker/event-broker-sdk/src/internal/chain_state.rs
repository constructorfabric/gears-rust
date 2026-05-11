use std::collections::HashMap;
use std::sync::RwLock;

use crate::ids::ProducerId;

type Key = (ProducerId, String, u32); // (producer_id, topic, partition)

/// Per-`(producer_id, topic, partition)` local sequence tracker.
/// Advanced at enqueue time for chained/monotonic producers.
#[derive(Debug, Default)]
pub struct ChainState {
    inner: RwLock<HashMap<Key, i64>>,
}

impl ChainState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Read the current `last_sequence` for a key (0 if unseen).
    pub(crate) fn peek(&self, key: &Key) -> i64 {
        self.inner
            .read()
            .expect("chain state lock")
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    /// Advance the tracker to `sequence` for a key.
    pub(crate) fn advance(&self, key: Key, sequence: i64) {
        self.inner
            .write()
            .expect("chain state lock")
            .insert(key, sequence);
    }

    /// Reset the tracker for a key (used by `reset_chain`).
    pub(crate) fn reset(&self, key: &Key) {
        self.inner.write().expect("chain state lock").remove(key);
    }

    /// Prime multiple keys from broker cursors (used on `reuse` at build time).
    pub(crate) fn bulk_prime(&self, entries: impl IntoIterator<Item = (Key, i64)>) {
        let mut guard = self.inner.write().expect("chain state lock");
        for (k, seq) in entries {
            guard.insert(k, seq);
        }
    }
}
