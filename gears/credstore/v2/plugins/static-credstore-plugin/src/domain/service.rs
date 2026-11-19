// Updated: 2026-10-06 by Constructor Tech
//! Thread-safe in-memory versioned value store.
//!
//! Keyed by `StoreKey { tenant_id, record_id }` (ADR-0006). Per key the store
//! keeps a map `version -> bytes` and a monotonic counter: the n-th `put`
//! under a key returns the version `"n"`, so versions are ordered per key as
//! `destroy(Below)` requires. Versions are immutable. `delete_key` and
//! `destroy` of anything not held are successes (idempotent).
use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

use credstore_sdk::{DestroySelector, SecretValue, StoreKey, ValueVersion};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::config::StaticCredStorePluginConfig;

/// All versions of one key plus its monotonic counter.
#[domain_model]
#[derive(Debug, Default)]
struct KeyEntry {
    /// Last version number handed out under this key.
    last: u64,
    versions: BTreeMap<u64, SecretValue>,
}

/// In-memory backend store: `(tenant_id, record_id) -> versions`.
#[domain_model]
#[derive(Debug, Default)]
struct Store {
    keys: HashMap<(Uuid, Uuid), KeyEntry>,
}

/// Static credstore backend.
///
/// A versioned in-memory value store implementing the
/// `CredStorePluginClientV2` contract, including the optional `destroy`.
/// No configuration seeds values (ADR-0006) - the store starts empty and is
/// populated only through the gear's write protocol.
#[domain_model]
#[derive(Debug, Default)]
pub struct Service {
    inner: RwLock<Store>,
}

fn map_key(key: &StoreKey) -> (Uuid, Uuid) {
    (key.tenant_id.0, key.record_id)
}

impl Service {
    /// Create a service from plugin configuration.
    ///
    /// Configuration carries only GTS-registration input (vendor, priority);
    /// this constructor never fails but keeps the fallible signature other
    /// plugin backends need. The store always starts empty.
    ///
    /// # Errors
    ///
    /// Never returns an error today; kept `Result` so a future backend that
    /// does validate configuration does not need a signature change.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "signature matches the plugin construction contract other backends use, \
                  which may validate configuration and fail"
    )]
    pub fn from_config(_cfg: &StaticCredStorePluginConfig) -> anyhow::Result<Self> {
        Ok(Self {
            inner: RwLock::new(Store::default()),
        })
    }

    /// Store a new immutable version under `key` and return its version.
    pub fn put_value(&self, key: &StoreKey, value: SecretValue) -> ValueVersion {
        let mut store = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = store.keys.entry(map_key(key)).or_default();
        entry.last += 1;
        entry.versions.insert(entry.last, value);
        ValueVersion::new(entry.last.to_string())
    }

    /// Read the bytes of `version` under `key`, or `None` if absent.
    #[must_use]
    pub fn get_value(&self, key: &StoreKey, version: &ValueVersion) -> Option<SecretValue> {
        let n: u64 = version.as_str().parse().ok()?;
        let store = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // `SecretValue` is not `Clone` (it zeroizes on drop), so reconstruct
        // from the stored bytes.
        store
            .keys
            .get(&map_key(key))?
            .versions
            .get(&n)
            .map(|v| SecretValue::new(v.as_bytes().to_vec()))
    }

    /// Remove the key with all its versions (and its counter). A miss is a
    /// no-op.
    pub fn delete_key_value(&self, key: &StoreKey) {
        let mut store = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store.keys.remove(&map_key(key));
    }

    /// Destroy the selected versions of `key`. A miss is a no-op; a version
    /// string that is not one this store issued selects nothing. The counter
    /// is kept, so destroyed version numbers are never reissued.
    pub fn destroy_value(&self, key: &StoreKey, selector: &DestroySelector) {
        let mut store = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = store.keys.get_mut(&map_key(key)) else {
            return;
        };
        match selector {
            DestroySelector::Below(v) => {
                if let Ok(n) = v.as_str().parse::<u64>() {
                    entry.versions = entry.versions.split_off(&n);
                }
            }
            DestroySelector::Exactly(v) => {
                if let Ok(n) = v.as_str().parse::<u64>() {
                    entry.versions.remove(&n);
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
