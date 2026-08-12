//! Instance-addressable topology helpers (`cpt-cf-adr-instance-addressable-discovery` §6).
//!
//! Toolkit *mechanisms* that targeting consumers (the event-broker dispatcher,
//! the cluster coordinator) would otherwise each re-implement:
//!
//! * [`TopologyView`] — a label-keyed membership view over
//!   [`resolve_by_labels`](crate::DirectoryClient::resolve_by_labels) with
//!   refresh and a consistent-hash [`pick`](TopologyView::pick).
//! * [`NotShardOwner`] — the typed "not the owner" signal for the stale-owner
//!   correction path.
//!
//! Both stay **mechanism**: the gear owns the hash key, the selection *policy*,
//! and the ownership assignment; these only remove the poll/refresh/hash
//! boilerplate and standardize the correction protocol.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use toolkit_stable_hash::murmur3_x86_32;

use crate::DirectoryClient;
use cf_system_sdks::directory::{LabelSelector, ServiceInstanceInfo};

/// RFC-9457 problem-type slug a gear SHOULD use when it surfaces a
/// [`NotShardOwner`] over HTTP (`cpt-cf-adr-instance-addressable-discovery` §6). The concrete `Problem` / GTS
/// wiring is owned by the gear's HTTP boundary; this is only the shared slug so
/// callers and targets agree on the identifier.
pub const NOT_SHARD_OWNER_PROBLEM_TYPE: &str = "not-shard-owner";

/// The typed "not the owner" rejection of the stale-ownership correction path
/// (`cpt-cf-adr-instance-addressable-discovery` §6).
///
/// A target MUST return this for a shard/partition/lease it no longer owns,
/// **optionally carrying the current owner's stable label** so the caller can
/// re-target without a full re-enumeration. The caller MUST treat it as a
/// **signal to refresh** its ownership map (via `resolve_by_labels`) and retry
/// against the corrected owner — **not** as a normal application error.
///
/// It is carried through [`anyhow::Error`] and recovered by downcast, the same
/// sentinel pattern as `DirectoryNotFound`, so it survives transport/`?`
/// boundaries without a bespoke error enum on every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotShardOwner {
    /// The shard/partition/lease key the caller targeted, for diagnostics.
    pub requested: String,
    /// The current owner's stable label(s) (e.g. `{shard: "7"}` or
    /// `{pod: "ingest-2"}`), if the target knows who took over. When present the
    /// caller can re-target directly; when `None` it must re-enumerate.
    pub current_owner: Option<BTreeMap<String, String>>,
}

impl NotShardOwner {
    /// A fence with no known current owner — the caller must re-enumerate.
    #[must_use]
    pub fn new(requested: impl Into<String>) -> Self {
        Self {
            requested: requested.into(),
            current_owner: None,
        }
    }

    /// A fence carrying the current owner's stable label so the caller can
    /// re-target without a full re-enumeration.
    #[must_use]
    pub fn with_owner(
        requested: impl Into<String>,
        current_owner: BTreeMap<String, String>,
    ) -> Self {
        Self {
            requested: requested.into(),
            current_owner: Some(current_owner),
        }
    }

    /// Recover a [`NotShardOwner`] from an [`anyhow::Error`], if that is what it
    /// wraps. Callers use this to distinguish the fence (refresh + retry) from a
    /// genuine application error.
    #[must_use]
    pub fn from_anyhow(err: &anyhow::Error) -> Option<&Self> {
        err.downcast_ref::<Self>()
    }
}

impl std::fmt::Display for NotShardOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not the owner of {}", self.requested)?;
        if let Some(owner) = &self.current_owner {
            write!(f, " (current owner:")?;
            for (k, v) in owner {
                write!(f, " {k}={v}")?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl std::error::Error for NotShardOwner {}

/// A label-keyed membership view over one directory name (`cpt-cf-adr-instance-addressable-discovery` §6).
///
/// Holds the latest [`resolve_by_labels`](DirectoryClient::resolve_by_labels)
/// snapshot for `(name, selector)`; [`refresh`](Self::refresh) re-polls it,
/// [`instances`](Self::instances) reads it, and [`pick`](Self::pick) does a
/// consistent-hash selection over the matched set.
pub struct TopologyView {
    client: Arc<dyn DirectoryClient>,
    name: String,
    selector: LabelSelector,
    snapshot: parking_lot::RwLock<Vec<ServiceInstanceInfo>>,
}

impl TopologyView {
    /// Build a view over `name` filtered by `selector`. The snapshot starts
    /// empty; call [`refresh`](Self::refresh) to populate it.
    #[must_use]
    pub fn new(
        client: Arc<dyn DirectoryClient>,
        name: impl Into<String>,
        selector: LabelSelector,
    ) -> Self {
        Self {
            client,
            name: name.into(),
            selector,
            snapshot: parking_lot::RwLock::new(Vec::new()),
        }
    }

    /// Re-poll `resolve_by_labels(name, selector)` and replace the snapshot.
    ///
    /// `Ok(empty)` is a valid (not-ready-yet) result and simply stores an empty
    /// snapshot; only a backend error propagates.
    ///
    /// # Errors
    /// Propagates a directory-backend failure from `resolve_by_labels`.
    pub async fn refresh(&self) -> Result<()> {
        let instances = self
            .client
            .resolve_by_labels(&self.name, &self.selector)
            .await?;
        *self.snapshot.write() = instances;
        Ok(())
    }

    /// The current snapshot of matched instances.
    #[must_use]
    pub fn instances(&self) -> Vec<ServiceInstanceInfo> {
        self.snapshot.read().clone()
    }

    /// Consistent-hash pick of the instance that owns `hash_key` (rendezvous /
    /// HRW over `instance_id`).
    ///
    /// `None` only when the snapshot is empty. Deterministic for a given
    /// snapshot + key, with ≈1/N of keys remapping when membership changes.
    /// Picks the *owner* regardless of transient health: if it is down, the
    /// caller learns at call time and corrects via the stale-owner path
    /// ([`NotShardOwner`]).
    #[must_use]
    pub fn pick(&self, hash_key: &str) -> Option<ServiceInstanceInfo> {
        self.snapshot
            .read()
            .iter()
            .max_by_key(|inst| rendezvous_score(hash_key, &inst.instance_id))
            .cloned()
    }
}

/// Version tag mixed into the rendezvous framing. Bump only on an intentional,
/// breaking change to the scoring encoding (it remaps ownership).
const RENDEZVOUS_HASH_VERSION: u8 = 1;

/// Distinct seeds for the two 32-bit halves that compose the `u64` score.
const RENDEZVOUS_SEED_HI: u32 = 0x9747_b28c;
const RENDEZVOUS_SEED_LO: u32 = 0x2545_f491;

/// Rendezvous (HRW) weight for `(key, node)`: a non-cryptographic hash is
/// sufficient — the property required is a uniform, deterministic ordering, not
/// collision resistance.
///
/// Uses the versioned stable [`murmur3_x86_32`] instead of `std`'s
/// `DefaultHasher` so the ordering is **identical across toolkit and Rust
/// versions** (`DefaultHasher` guarantees neither), keeping shard ownership
/// stable across rebuilds. Inputs are combined with **explicit fixed framing** —
/// a version tag plus length-prefixed fields — so `(key, node)` boundaries are
/// unambiguous (e.g. `("ab", "c")` never collides with `("a", "bc")`). Two
/// differently-seeded 32-bit hashes compose the deterministic `u64` score.
fn rendezvous_score(key: &str, node: &str) -> u64 {
    let mut buf = Vec::with_capacity(1 + 16 + key.len() + node.len());
    buf.push(RENDEZVOUS_HASH_VERSION);
    buf.extend_from_slice(&(key.len() as u64).to_le_bytes());
    buf.extend_from_slice(key.as_bytes());
    buf.extend_from_slice(&(node.len() as u64).to_le_bytes());
    buf.extend_from_slice(node.as_bytes());

    let hi = murmur3_x86_32(&buf, RENDEZVOUS_SEED_HI);
    let lo = murmur3_x86_32(&buf, RENDEZVOUS_SEED_LO);
    (u64::from(hi) << 32) | u64::from(lo)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cf_system_sdks::directory::{RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo};

    /// A directory stub whose `list_instances` returns a fixed set (the
    /// `resolve_by_labels` default filters it).
    struct StubDirectory {
        instances: Vec<ServiceInstanceInfo>,
    }

    fn inst(id: &str, shard: &str) -> ServiceInstanceInfo {
        ServiceInstanceInfo {
            gear: "event-broker-ingest".to_owned(),
            instance_id: id.to_owned(),
            endpoint: ServiceEndpoint::new(format!("http://{id}:8080")),
            version: None,
            rest_endpoint: Some(ServiceEndpoint::new(format!("http://{id}:8080"))),
            openapi_spec: None,
            openapi_spec_hash: None,
            grpc_services: Vec::new(),
            labels: BTreeMap::from([("shard".to_owned(), shard.to_owned())]),
        }
    }

    #[async_trait]
    impl DirectoryClient for StubDirectory {
        async fn resolve_grpc_service(&self, _: &str) -> Result<ServiceEndpoint> {
            unimplemented!()
        }
        async fn resolve_rest_service(&self, _: &str) -> Result<ServiceEndpoint> {
            unimplemented!()
        }
        async fn get_openapi_spec(&self, _: &str) -> Result<String> {
            unimplemented!()
        }
        async fn list_instances(&self, _: &str) -> Result<Vec<ServiceInstanceInfo>> {
            Ok(self.instances.clone())
        }
        async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>> {
            Ok(self.instances.clone())
        }
        async fn register_instance(&self, _: RegisterInstanceInfo) -> Result<()> {
            unimplemented!()
        }
        async fn deregister_instance(&self, _: &str, _: &str) -> Result<()> {
            unimplemented!()
        }
        async fn send_heartbeat(&self, _: &str, _: &str) -> Result<()> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn refresh_loads_matched_instances() {
        let dir = Arc::new(StubDirectory {
            instances: vec![
                inst("ingest-0", "0"),
                inst("ingest-1", "1"),
                inst("ingest-2", "2"),
            ],
        });
        let view = TopologyView::new(dir, "event-broker-ingest", LabelSelector::new());
        view.refresh().await.unwrap();
        assert_eq!(view.instances().len(), 3);
    }

    #[tokio::test]
    async fn pick_is_deterministic_and_covers_all_matched() {
        let dir = Arc::new(StubDirectory {
            instances: vec![
                inst("ingest-0", "0"),
                inst("ingest-1", "1"),
                inst("ingest-2", "2"),
            ],
        });
        let view = TopologyView::new(dir, "event-broker-ingest", LabelSelector::new());
        view.refresh().await.unwrap();

        // Deterministic for a given snapshot + key (picks the owner regardless
        // of health — no serving filter).
        let a = view.pick("partition-42").unwrap();
        let b = view.pick("partition-42").unwrap();
        assert_eq!(a.instance_id, b.instance_id);

        // Different keys spread across the matched set (the owner is one of them).
        let owners: std::collections::BTreeSet<_> = (0..50)
            .map(|i| view.pick(&format!("k-{i}")).unwrap().instance_id)
            .collect();
        assert!(
            owners.len() > 1,
            "hash ring should spread keys across owners"
        );
    }

    #[tokio::test]
    async fn pick_none_when_empty() {
        let dir = Arc::new(StubDirectory { instances: vec![] });
        let view = TopologyView::new(dir, "event-broker-ingest", LabelSelector::new());
        view.refresh().await.unwrap();
        assert!(view.pick("k").is_none());
    }

    #[test]
    fn rendezvous_score_known_answer_vectors() {
        // Pinned vectors over representative key/node pairs. These MUST stay
        // stable across toolkit and Rust versions (shard ownership depends on
        // the ordering); a change here means a deliberate encoding bump
        // (`RENDEZVOUS_HASH_VERSION`), which remaps ownership.
        let vectors: &[(&str, &str)] = &[
            ("", ""),
            ("partition-42", "ingest-0"),
            ("partition-42", "ingest-1"),
            ("k-1", "ingest-2"),
            ("shard-7", "node-a"),
        ];
        let actual: Vec<u64> = vectors
            .iter()
            .map(|&(k, n)| rendezvous_score(k, n))
            .collect();
        let expected: Vec<u64> = vec![
            3_576_388_067_079_361_786,
            15_910_422_322_216_717_070,
            5_515_605_313_099_864_569,
            2_581_831_032_155_280_097,
            15_212_296_452_332_127_228,
        ];
        assert_eq!(actual, expected);

        // Explicit framing: field boundaries are unambiguous, so a shared
        // concatenation ("abc") maps to distinct scores under different splits.
        assert_ne!(rendezvous_score("ab", "c"), rendezvous_score("a", "bc"));
    }

    #[test]
    fn not_shard_owner_is_recoverable_by_downcast() {
        let owner = BTreeMap::from([("shard".to_owned(), "7".to_owned())]);
        let err: anyhow::Error = NotShardOwner::with_owner("shard-7", owner.clone()).into();
        let recovered = NotShardOwner::from_anyhow(&err).expect("downcast");
        assert_eq!(recovered.requested, "shard-7");
        assert_eq!(recovered.current_owner.as_ref(), Some(&owner));

        // A different error is not mistaken for the fence.
        let other = anyhow::anyhow!("boom");
        assert!(NotShardOwner::from_anyhow(&other).is_none());
    }

    #[test]
    fn not_shard_owner_new_has_no_current_owner() {
        let fence = NotShardOwner::new("partition-3");
        assert_eq!(fence.requested, "partition-3");
        assert!(fence.current_owner.is_none());
    }

    #[test]
    fn not_shard_owner_display_covers_both_branches() {
        // Without a known owner: just the requested key.
        assert_eq!(
            NotShardOwner::new("shard-9").to_string(),
            "not the owner of shard-9"
        );

        // With a known owner: the stable label(s) are appended (BTreeMap orders
        // keys, so the rendering is deterministic).
        let owner = BTreeMap::from([
            ("pod".to_owned(), "ingest-2".to_owned()),
            ("shard".to_owned(), "7".to_owned()),
        ]);
        assert_eq!(
            NotShardOwner::with_owner("shard-7", owner).to_string(),
            "not the owner of shard-7 (current owner: pod=ingest-2 shard=7)"
        );
    }

    #[tokio::test]
    async fn refresh_propagates_backend_error() {
        struct ErrDirectory;

        #[async_trait]
        impl DirectoryClient for ErrDirectory {
            async fn resolve_grpc_service(&self, _: &str) -> Result<ServiceEndpoint> {
                unimplemented!()
            }
            async fn resolve_rest_service(&self, _: &str) -> Result<ServiceEndpoint> {
                unimplemented!()
            }
            async fn get_openapi_spec(&self, _: &str) -> Result<String> {
                unimplemented!()
            }
            async fn list_instances(&self, _: &str) -> Result<Vec<ServiceInstanceInfo>> {
                anyhow::bail!("directory backend down")
            }
            async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>> {
                unimplemented!()
            }
            async fn register_instance(&self, _: RegisterInstanceInfo) -> Result<()> {
                unimplemented!()
            }
            async fn deregister_instance(&self, _: &str, _: &str) -> Result<()> {
                unimplemented!()
            }
            async fn send_heartbeat(&self, _: &str, _: &str) -> Result<()> {
                unimplemented!()
            }
        }

        let view = TopologyView::new(
            Arc::new(ErrDirectory),
            "event-broker-ingest",
            LabelSelector::new(),
        );
        // A backend failure surfaces (rather than being swallowed as empty), and
        // the snapshot stays empty.
        assert!(view.refresh().await.is_err());
        assert!(view.instances().is_empty());
    }
}
