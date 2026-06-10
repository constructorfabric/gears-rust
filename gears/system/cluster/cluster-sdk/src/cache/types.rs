// Created: 2026-06-03 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-cache-primitive-types:p1
//! Cache domain types: the versioned entry, consistency class, key-only event,
//! the native features descriptor, and the capability requirement enum.

/// A versioned cache value.
///
/// `version` is opaque and monotonically increasing per key, starting at 1;
/// version 0 is reserved as a sentinel and never observed on a stored entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// The stored bytes.
    pub value: Vec<u8>,
    /// The monotonic version (`>= 1`).
    pub version: u64,
}

/// The consistency class a cache backend declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheConsistency {
    /// Linearizable reads and writes — required for correctness-sensitive CAS.
    Linearizable,
    /// Eventually consistent — CAS may exhibit split-brain under partition.
    EventuallyConsistent,
}

/// A lightweight, key-only cache mutation notification. It carries no value —
/// the consumer calls `get(key)` for the current value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheEvent {
    /// The key was created or updated.
    Changed {
        /// The affected key.
        key: String,
    },
    /// The key was deleted.
    Deleted {
        /// The affected key.
        key: String,
    },
    /// The key's TTL elapsed and it was removed.
    Expired {
        /// The affected key.
        key: String,
    },
}

impl CacheEvent {
    /// The key this event concerns.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Changed { key } | Self::Deleted { key } | Self::Expired { key } => key,
        }
    }
}

/// Native capability flags a cache backend declares via
/// [`ClusterCacheBackend::features`](crate::cache::ClusterCacheBackend::features).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheFeatures {
    /// Whether the backend natively supports prefix watches.
    pub prefix_watch: bool,
}

impl CacheFeatures {
    /// Creates a features descriptor.
    #[must_use]
    pub fn new(prefix_watch: bool) -> Self {
        Self { prefix_watch }
    }
}

/// A capability a consumer can require of a cache backend at resolution time.
/// Each variant maps to a concrete backend characteristic check (DESIGN §3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheCapability {
    /// Require the backend's [`CacheConsistency`] to be `Linearizable`.
    Linearizable,
    /// Require native prefix-watch support.
    PrefixWatch,
}

#[cfg(test)]
mod tests {
    use super::CacheEvent;

    #[test]
    fn event_exposes_affected_key() {
        assert_eq!(
            CacheEvent::Changed {
                key: "k".to_owned()
            }
            .key(),
            "k"
        );
        assert_eq!(
            CacheEvent::Deleted {
                key: "d".to_owned()
            }
            .key(),
            "d"
        );
        assert_eq!(
            CacheEvent::Expired {
                key: "e".to_owned()
            }
            .key(),
            "e"
        );
    }
}
