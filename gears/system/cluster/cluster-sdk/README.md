# Cluster SDK

`cf-gears-cluster-sdk` (lib `cluster_sdk`) is the shared, serde-free, dyn-safe
contract foundation that every cluster coordination primitive — cache, leader
election, distributed lock, and service discovery — builds on.

## Overview

This crate provides the cross-cutting types and helpers that let the public
cluster contract evolve independently of any backend:

- **`ClusterError`** — the unified error type returned by every primitive
  facade, with no `NotStarted` variant. Variants cover capability-not-met,
  profile-not-bound, profile-not-specified, invalid-name, invalid-config,
  shutdown, and a structured provider error.
- **`ProviderErrorKind`** — programmatic retryability classification
  (`ConnectionLost`, `Timeout`, `AuthFailure`, `ResourceExhausted`, `Other`)
  with `is_retryable()`, so consumers branch on the kind without parsing
  strings.
- **`ClusterProfile`** — a typed profile marker (`const NAME`) that is declared
  once on a zero-sized type and is the sole consumer-facing profile path; the SDK
  maps it to the stable `cluster:{profile}` lookup scope internally. The
  `validate_cluster_name` helper validates coordination names against the rule.
- **`assert_dyn_compatible!`** — a compile-time dyn-compatibility assertion
  harness applied per backend trait.

## Usage

```rust
use cluster_sdk::{ClusterProfile, validate_cluster_name};

// Declare a profile once, by type. Pass it to the resolvers/wiring by type —
// never by string — and the SDK resolves it to the `cluster:{profile}` scope
// internally.
struct OrdersProfile;
impl ClusterProfile for OrdersProfile {
    const NAME: &'static str = "orders";
}

// The profile name (and any coordination name) must satisfy the name rule.
validate_cluster_name(OrdersProfile::NAME)?;
# Ok::<(), cluster_sdk::ClusterError>(())
```

Apply the dyn-compatibility harness to each backend trait:

```rust
use cluster_sdk::assert_dyn_compatible;

trait CacheBackend: Send + Sync {
    fn consistency(&self) -> u8;
}
assert_dyn_compatible!(CacheBackend);
```

## License

Apache-2.0
