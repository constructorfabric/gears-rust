//! Doubles for the activation tests: an in-process key lock and a recording
//! lock-metrics sink.
//!
//! The production lock is `toolkit-db`'s advisory lock, which on `SQLite` is a
//! file marker under the user's cache directory shared by every test process:
//! tests that run in parallel over different in-memory databases would contend
//! on it through equal keys. The unit tests therefore take the lock through
//! this double, and the real one is exercised on `PostgreSQL`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::{HeldKey, KeyLocks, LockMetrics};
use crate::domain::error::DomainError;

/// A lock table in this process: one holder per key, a budget, and a count of
/// what was asked.
pub struct InProcessLocks {
    held: Arc<Mutex<HashSet<String>>>,
    budget: Duration,
    /// How many acquisitions were attempted, by key.
    pub asked: Arc<Mutex<Vec<String>>>,
    /// How many releases ran.
    pub released: Arc<AtomicUsize>,
}

impl InProcessLocks {
    /// A table whose waits give up after `budget`.
    #[must_use]
    pub fn with_budget(budget: Duration) -> Arc<Self> {
        Arc::new(Self {
            held: Arc::default(),
            budget,
            asked: Arc::default(),
            released: Arc::default(),
        })
    }

    /// A table with a budget no test outlasts.
    #[must_use]
    pub fn generous() -> Arc<Self> {
        Self::with_budget(Duration::from_secs(30))
    }

    /// Hold `key` as another mutation would, until the guard is released.
    ///
    /// # Panics
    /// If the table's own mutex is poisoned.
    #[must_use]
    pub fn hold(&self, key: &str) -> Held {
        self.held.lock().expect("lock table").insert(key.to_owned());
        Held {
            table: Arc::clone(&self.held),
            key: key.to_owned(),
            released: Arc::clone(&self.released),
        }
    }

    /// Whether `key` is held now.
    ///
    /// # Panics
    /// If the table's own mutex is poisoned.
    #[must_use]
    pub fn is_held(&self, key: &str) -> bool {
        self.held.lock().expect("lock table").contains(key)
    }

    /// How many times `key` was asked for.
    ///
    /// # Panics
    /// If the table's own mutex is poisoned.
    #[must_use]
    pub fn asks_for(&self, key: &str) -> usize {
        self.asked
            .lock()
            .expect("asked")
            .iter()
            .filter(|k| *k == key)
            .count()
    }
}

/// A held key of [`InProcessLocks`].
pub struct Held {
    table: Arc<Mutex<HashSet<String>>>,
    key: String,
    released: Arc<AtomicUsize>,
}

#[async_trait]
impl HeldKey for Held {
    async fn release(self: Box<Self>) {
        self.table.lock().expect("lock table").remove(&self.key);
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl KeyLocks for InProcessLocks {
    async fn acquire(
        &self,
        key: &str,
        cap: Option<Duration>,
    ) -> Result<Box<dyn HeldKey>, DomainError> {
        self.asked.lock().expect("asked").push(key.to_owned());
        let budget = cap.map_or(self.budget, |cap| cap.min(self.budget));
        let started = std::time::Instant::now();
        loop {
            if self.held.lock().expect("lock table").insert(key.to_owned()) {
                return Ok(Box::new(Held {
                    table: Arc::clone(&self.held),
                    key: key.to_owned(),
                    released: Arc::clone(&self.released),
                }));
            }
            if started.elapsed() >= budget {
                return Err(DomainError::Unavailable {
                    detail: "the key's mutation lock is busy".to_owned(),
                });
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

/// A metrics sink that remembers what it was told.
#[derive(Default)]
pub struct RecordingLockMetrics {
    /// The waits of the acquisitions that succeeded.
    pub acquired: Mutex<Vec<Duration>>,
    /// The waits of those that ran out their budget.
    pub exhausted: Mutex<Vec<Duration>>,
    /// How many errors were counted.
    pub errors: AtomicUsize,
}

impl LockMetrics for RecordingLockMetrics {
    fn acquired(&self, wait: Duration) {
        self.acquired.lock().expect("metrics").push(wait);
    }

    fn exhausted(&self, wait: Duration) {
        self.exhausted.lock().expect("metrics").push(wait);
    }

    fn error(&self) {
        self.errors.fetch_add(1, Ordering::SeqCst);
    }
}
