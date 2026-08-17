//! Link table for the `cluster-oop` binary (DESIGN-DEPLOYABLE-GEAR §12.8).
//!
//! `GearRegistry::discover_and_build` enumerates `inventory` entries, and
//! `inventory` only sees a crate the linker kept. A crate nothing references is
//! dropped, so each gear this process must run needs a `use … as _;` here.
//!
//! Two entries, and both are load-bearing:
//!
//! - `cluster` - the gear itself, registered by `#[toolkit::gear(name = "cluster",
//!   capabilities = [stateful, system, grpc, rest])]`.
//! - `grpc_hub` - **mandatory**, not optional. Cluster declares the `grpc`
//!   capability, and a registry carrying gRPC services with no hub fails the gRPC
//!   phase with `RegistryError::GrpcRequiresHub` (`host_runtime.rs:777-779`). This
//!   is Risk 8 and `D-24`: it applies to any process linking `cluster`, Profile 1
//!   monoliths included, not only to this binary.
//!
//! **The two cluster plugins are deliberately absent**, and §12.8's sketch lists
//! them. They are not gears: neither `standalone-cluster-plugin` nor
//! `postgres-cluster-plugin` submits an `inventory` entry (verified - no
//! `#[toolkit::gear]` and no `inventory::submit!` in either crate). They are
//! provider crates named directly by `ClusterGear::provider_registry()`, so the
//! library already forces them to be linked and a `use … as _;` line here would
//! be inert. Worse, it would read as though the plugin set were assembled by
//! linkage the way gears are, when in this gear it is a compile-time list in
//! `gear.rs` - which is the thing a future plugin author needs to find.
#![allow(
    unused_imports,
    reason = "the whole point of this module: `use X as _` keeps X linked so its inventory entry exists"
)]

use cluster as _;
use grpc_hub as _;
