//! The cluster profile this gear participates in.
//!
//! The marker is the join key, and it is the whole of what a `requires =
//! [cluster.*]` in `gear.gdl` binds against: the SDK turns `NAME` into
//! `ClientScope::new("cluster:event-broker")`, and Gearbox refuses a
//! requirement naming a profile no marker supplies (GBX0508). So the
//! description cannot state this on its own -- which is the point. The fact
//! lives in Rust and is projected.
//!
//! `NAME` is read, never derived. The identifier is `AuditProfile` and the
//! profile is `event-broker`, because the scope belongs to the product's
//! coordination domain rather than to this gear's name; deriving the profile
//! from the ident would yield `audit-profile` and bind nothing.
//!
//! Deliberately **no `deps = [cluster]`** on the gear. `deps` is a hard
//! topo-sort edge and `RegistryBuilder` fails the whole registry build with
//! `RegistryError::UnknownDependency` when a named gear is not linked into the
//! process, so declaring it would make an out-of-process consumer refuse to
//! start. Nothing is lost: the cluster gear declares the `system` capability
//! and therefore starts before any application consumer, and readiness gating
//! comes from cluster-sdk's own `ConsumerRegistration::dep_gear`. The reasoning
//! is written out in `cluster/tests/consumer_wiring.rs:71-90`.

use cluster_sdk::ClusterProfile;

/// The `event-broker` coordination scope, as this gear names it.
///
/// Public because it is the type a caller passes to
/// `ClusterCacheV1::resolver(hub).profile(...)`, and because a private marker
/// nothing constructs reads as dead code to the lint set.
#[derive(Debug, Clone, Copy)]
pub struct AuditProfile;

impl ClusterProfile for AuditProfile {
    const NAME: &'static str = "event-broker";
}

// Says which profiles this process expects, so the wiring can warn about one
// the server does not bind. Not a prerequisite for resolving.
cluster_sdk::register_cluster_profile!(AuditProfile);
