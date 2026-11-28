// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Self-tests of the conformance suite: a reference in-memory plugin passes
//! every check, and a plugin with one deliberate contract violation is caught
//! by the check that is meant to catch it.
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::conformance::{self, run_all};
use crate::{
    CredStorePluginClientV2, CredStorePluginDestroyerV2, CredStorePluginError, DestroySelector,
    SecretValue, StoreKey, ValueVersion,
};

/// One deliberate violation of the backend contract (or none).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// A conforming plugin with `destroy` and ordered versions.
    None,
    /// A conforming plugin without `destroy` and with unordered versions;
    /// `destroy` panics if the suite calls it.
    NoDestroyUnordered,
    /// Every `put` returns version `1` and overwrites the bytes.
    ConstantVersion,
    /// `get` returns the newest version's bytes whatever version is asked for.
    GetIgnoresVersion,
    /// Values are stored through a lossy UTF-8 conversion.
    LossyBytes,
    /// `delete_key` does nothing.
    DeleteKeepsVersions,
    /// `delete_key` of a key that is not held is `NotFound`.
    DeleteMissingFails,
    /// The tenant is not part of the storage key.
    TenantIgnored,
    /// `Below(v)` also removes `v`.
    BelowIsInclusive,
    /// Versions decrease with every put.
    DescendingVersions,
    /// The next version is derived from the versions still held.
    ReissuesVersions,
    /// `destroy` that removes nothing is an error.
    DestroyTwiceFails,
    /// Concurrent puts read the same counter and hand out equal versions.
    RacyVersions,
    /// `get` of a version or key it does not hold is an error, not `None`.
    GetUnknownFails,
    /// `destroy` leaks into the sibling record of the same tenant.
    DestroyHitsSiblings,
    /// `put` always fails with an internal error.
    PutFails,
}

#[derive(Default)]
struct Entry {
    last: u64,
    versions: BTreeMap<u64, Vec<u8>>,
}

/// Storage slot of a key: `(tenant, record)`, the tenant dropped under
/// [`Fault::TenantIgnored`].
type Slot = (Option<Uuid>, Uuid);

struct MemPlugin {
    fault: Fault,
    keys: Mutex<HashMap<Slot, Entry>>,
}

impl MemPlugin {
    fn new(fault: Fault) -> Self {
        Self {
            fault,
            keys: Mutex::new(HashMap::new()),
        }
    }

    fn slot(&self, key: &StoreKey) -> Slot {
        if self.fault == Fault::TenantIgnored {
            (None, key.record_id)
        } else {
            (Some(key.tenant_id.0), key.record_id)
        }
    }
}

#[async_trait]
impl CredStorePluginClientV2 for MemPlugin {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStorePluginError> {
        if self.fault == Fault::PutFails {
            return Err(CredStorePluginError::internal("injected put failure"));
        }
        let slot = self.slot(key);
        let bytes = if self.fault == Fault::LossyBytes {
            String::from_utf8_lossy(value.as_bytes())
                .into_owned()
                .into_bytes()
        } else {
            value.as_bytes().to_vec()
        };
        let observed_last = self
            .keys
            .lock()
            .unwrap()
            .get(&slot)
            .map_or(0, |entry| entry.last);
        if self.fault == Fault::RacyVersions {
            tokio::task::yield_now().await;
        }
        let mut keys = self.keys.lock().unwrap();
        let entry = keys.entry(slot).or_default();
        let version = match self.fault {
            Fault::ConstantVersion => 1,
            Fault::DescendingVersions | Fault::NoDestroyUnordered => {
                entry.last += 1;
                1_000 - entry.last
            }
            Fault::RacyVersions => {
                entry.last = observed_last + 1;
                entry.last
            }
            Fault::ReissuesVersions => entry
                .versions
                .keys()
                .next_back()
                .map_or(1, |newest| newest + 1),
            _ => {
                entry.last += 1;
                entry.last
            }
        };
        entry.versions.insert(version, bytes);
        Ok(ValueVersion::new(version.to_string()))
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStorePluginError> {
        let missing = || {
            if self.fault == Fault::GetUnknownFails {
                Err(CredStorePluginError::internal("unknown key or version"))
            } else {
                Ok(None)
            }
        };
        let Ok(n) = version.as_str().parse::<u64>() else {
            return missing();
        };
        let keys = self.keys.lock().unwrap();
        let Some(entry) = keys.get(&self.slot(key)) else {
            return missing();
        };
        if self.fault == Fault::GetIgnoresVersion {
            return Ok(entry
                .versions
                .values()
                .next_back()
                .map(|bytes| SecretValue::new(bytes.clone())));
        }
        match entry.versions.get(&n) {
            Some(bytes) => Ok(Some(SecretValue::new(bytes.clone()))),
            None => missing(),
        }
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
    ) -> Result<(), CredStorePluginError> {
        let mut keys = self.keys.lock().unwrap();
        match self.fault {
            Fault::DeleteKeepsVersions => Ok(()),
            Fault::DeleteMissingFails if !keys.contains_key(&self.slot(key)) => {
                Err(CredStorePluginError::internal("key not held"))
            }
            _ => {
                keys.remove(&self.slot(key));
                Ok(())
            }
        }
    }

    fn destroyer(&self) -> Option<&dyn CredStorePluginDestroyerV2> {
        (self.fault != Fault::NoDestroyUnordered).then_some(self as &dyn CredStorePluginDestroyerV2)
    }
}

#[async_trait]
impl CredStorePluginDestroyerV2 for MemPlugin {
    async fn destroy(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        selector: DestroySelector,
    ) -> Result<(), CredStorePluginError> {
        assert!(
            self.fault != Fault::NoDestroyUnordered,
            "the suite called destroy on a plugin whose destroyer() is None"
        );
        let mut keys = self.keys.lock().unwrap();
        let slots: Vec<Slot> = if self.fault == Fault::DestroyHitsSiblings {
            keys.keys()
                .filter(|(tenant, _)| *tenant == Some(key.tenant_id.0))
                .copied()
                .collect()
        } else {
            vec![self.slot(key)]
        };
        for slot in slots {
            let Some(entry) = keys.get_mut(&slot) else {
                continue;
            };
            let before = entry.versions.len();
            match &selector {
                DestroySelector::Below(v) => {
                    if let Ok(n) = v.as_str().parse::<u64>() {
                        let inclusive = self.fault == Fault::BelowIsInclusive;
                        entry
                            .versions
                            .retain(|k, _| if inclusive { *k > n } else { *k >= n });
                    }
                }
                DestroySelector::Exactly(v) => {
                    if let Ok(n) = v.as_str().parse::<u64>() {
                        entry.versions.remove(&n);
                    }
                }
            }
            if self.fault == Fault::DestroyTwiceFails && entry.versions.len() == before {
                return Err(CredStorePluginError::internal("nothing to destroy"));
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn reference_plugin_passes_every_check() {
    run_all(&MemPlugin::new(Fault::None)).await;
}

#[tokio::test]
async fn plugin_without_destroy_passes_and_is_never_asked_to_destroy() {
    // Unordered versions are fine without `destroy`, and `destroy` would panic.
    run_all(&MemPlugin::new(Fault::NoDestroyUnordered)).await;
}

/// The macro, expanded inside the crate against the reference plugin: one test
/// per check.
mod generated {
    use super::{Fault, MemPlugin};

    crate::credstore_plugin_conformance!(MemPlugin::new(Fault::None));
}

/// The macro with outer attributes (two of them, and a trailing comma): they
/// are applied to every generated test. The tests are ignored in a default
/// run, which is the observable effect (`cargo test -- --list` marks them,
/// `-- --ignored` runs them).
mod generated_with_attributes {
    use super::{Fault, MemPlugin};

    crate::credstore_plugin_conformance!(
        #[ignore = "self-test of attribute forwarding; run with `-- --ignored`"]
        #[allow(clippy::too_many_lines, reason = "second attribute, forwarded as is")]
        MemPlugin::new(Fault::None),
    );
}

#[tokio::test]
#[should_panic(expected = "conformance[puts_yield_distinct_immutable_versions]")]
async fn constant_version_is_caught() {
    conformance::puts_yield_distinct_immutable_versions(&MemPlugin::new(Fault::ConstantVersion))
        .await;
}

#[tokio::test]
#[should_panic(expected = "returned by two different puts")]
async fn constant_version_is_named_in_the_message() {
    conformance::puts_yield_distinct_immutable_versions(&MemPlugin::new(Fault::ConstantVersion))
        .await;
}

#[tokio::test]
#[should_panic(expected = "conformance[puts_yield_distinct_immutable_versions]")]
async fn get_ignoring_the_version_is_caught() {
    conformance::puts_yield_distinct_immutable_versions(&MemPlugin::new(Fault::GetIgnoresVersion))
        .await;
}

#[tokio::test]
#[should_panic(expected = "conformance[get_unissued_version_is_none]")]
async fn get_returning_bytes_for_an_unissued_version_is_caught() {
    conformance::get_unissued_version_is_none(&MemPlugin::new(Fault::GetIgnoresVersion)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[put_get_binary]")]
async fn lossy_binary_storage_is_caught() {
    conformance::put_get_binary(&MemPlugin::new(Fault::LossyBytes)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[delete_key_removes_all_versions]")]
async fn delete_key_keeping_versions_is_caught() {
    conformance::delete_key_removes_all_versions(&MemPlugin::new(Fault::DeleteKeepsVersions)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[delete_key_is_idempotent]")]
async fn non_idempotent_delete_key_is_caught() {
    conformance::delete_key_is_idempotent(&MemPlugin::new(Fault::DeleteMissingFails)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[keys_are_isolated_across_tenants]")]
async fn ignoring_the_tenant_is_caught() {
    conformance::keys_are_isolated_across_tenants(&MemPlugin::new(Fault::TenantIgnored)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[get_never_written_key_is_none]")]
async fn get_error_on_an_unknown_key_is_caught() {
    conformance::get_never_written_key_is_none(&MemPlugin::new(Fault::GetUnknownFails)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[get_unissued_version_is_none]")]
async fn get_error_on_an_unissued_version_is_caught() {
    conformance::get_unissued_version_is_none(&MemPlugin::new(Fault::GetUnknownFails)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[concurrent_puts_yield_distinct_versions]")]
async fn colliding_concurrent_puts_are_caught() {
    conformance::concurrent_puts_yield_distinct_versions(&MemPlugin::new(Fault::RacyVersions))
        .await;
}

#[tokio::test]
#[should_panic(expected = "conformance[destroy_below_removes_only_older_versions]")]
async fn inclusive_below_is_caught() {
    conformance::destroy_below_removes_only_older_versions(&MemPlugin::new(
        Fault::BelowIsInclusive,
    ))
    .await;
}

#[tokio::test]
#[should_panic(expected = "conformance[destroy_is_idempotent]")]
async fn non_idempotent_destroy_is_caught() {
    conformance::destroy_is_idempotent(&MemPlugin::new(Fault::DestroyTwiceFails)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[destroy_does_not_touch_other_keys]")]
async fn destroy_leaking_into_sibling_keys_is_caught() {
    conformance::destroy_does_not_touch_other_keys(&MemPlugin::new(Fault::DestroyHitsSiblings))
        .await;
}

#[tokio::test]
#[should_panic(expected = "conformance[versions_are_ordered_across_sequential_puts]")]
async fn descending_versions_are_caught() {
    conformance::versions_are_ordered_across_sequential_puts(&MemPlugin::new(
        Fault::DescendingVersions,
    ))
    .await;
}

#[tokio::test]
#[should_panic(expected = "conformance[destroyed_versions_are_not_reissued]")]
async fn reissued_versions_are_caught() {
    conformance::destroyed_versions_are_not_reissued(&MemPlugin::new(Fault::ReissuesVersions))
        .await;
}

#[tokio::test]
#[should_panic(expected = "every put returned the same version")]
async fn conformance_get_unissued_version_detects_constant_versions() {
    // With one version for every put there is no foreign version to probe
    // with; the check must say so instead of passing vacuously.
    conformance::get_unissued_version_is_none(&MemPlugin::new(Fault::ConstantVersion)).await;
}

#[tokio::test]
#[should_panic(expected = "conformance[put_get_text]: put of 28 bytes")]
async fn conformance_put_failure_is_reported() {
    // The first value `put_get_text` writes is 28 bytes long.
    conformance::put_get_text(&MemPlugin::new(Fault::PutFails)).await;
}

#[tokio::test]
#[should_panic(expected = "failed: store error: injected put failure")]
async fn conformance_put_failure_carries_the_plugin_error() {
    conformance::put_get_text(&MemPlugin::new(Fault::PutFails)).await;
}
