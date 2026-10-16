//! `InMemoryRegistryStateStore`: the registry-state row in memory, with the
//! database store's semantics and the Tokio clock for lease times, so gate
//! and loop tests run under a paused clock. The in-memory backend itself
//! never gates its reconcile (each process owns its store).

use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::time::Instant;
use uuid::Uuid;

use crate::domain::repo::{RegistryState, RegistryStateStore, RepositoryError};

#[derive(Default)]
struct Row {
    state: RegistryState,
    lease: Option<(Uuid, Instant)>,
}

/// In-memory registry-state store.
#[derive(Default)]
pub(crate) struct InMemoryRegistryStateStore {
    row: Mutex<Row>,
}

impl InMemoryRegistryStateStore {
    /// An empty row: no generation, no hash, no lease.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Who holds the lease now, if anyone.
    pub(crate) fn lease_owner(&self) -> Option<Uuid> {
        let row = self.row.lock();
        row.lease
            .filter(|(_, expires)| *expires > Instant::now())
            .map(|(owner, _)| owner)
    }

    /// Overwrite the stored hash, as another writer would.
    pub(crate) fn set_content_hash(&self, hash: Option<&str>) {
        self.row.lock().state.content_hash = hash.map(str::to_owned);
    }
}

#[async_trait]
impl RegistryStateStore for InMemoryRegistryStateStore {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        Ok(self.row.lock().state.clone())
    }

    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        let mut row = self.row.lock();
        if row.state.generation.is_some_and(|g| g >= generation) {
            return Ok(false);
        }
        row.state.generation = Some(generation);
        Ok(true)
    }

    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        let mut row = self.row.lock();
        let now = Instant::now();
        let free = row
            .lease
            .is_none_or(|(holder, expires)| holder == owner || expires < now);
        let not_newer = generation.is_none_or(|g| row.state.generation.is_none_or(|s| s <= g));
        if !(free && not_newer) {
            return Ok(false);
        }
        row.lease = Some((owner, now + ttl.max(Duration::from_secs(1))));
        row.state.content_hash = None;
        if let Some(g) = generation {
            row.state.generation = Some(g);
        }
        Ok(true)
    }

    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError> {
        let mut row = self.row.lock();
        match row.lease {
            Some((holder, _)) if holder == owner => {
                row.lease = Some((owner, Instant::now() + ttl.max(Duration::from_secs(1))));
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn finish(
        &self,
        owner: Uuid,
        content_hash: Option<&str>,
    ) -> Result<bool, RepositoryError> {
        let mut row = self.row.lock();
        match row.lease {
            Some((holder, _)) if holder == owner => {
                row.lease = None;
                if let Some(hash) = content_hash {
                    row.state.content_hash = Some(hash.to_owned());
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
