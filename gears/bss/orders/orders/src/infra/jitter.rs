//! Full jitter for retry pauses: the gear's one random-delay helper, shared by the producer
//! binding's backoff (`gear.rs`) and the gate's retry pause (`gate::budget`).
use std::time::Duration;

/// Full jitter (the cluster SDK `RetryPolicy` with `jitter_factor: 1.0`, the
/// `toolkit-transport-grpc` `rpc_retry` strategy): a delay spread uniformly over `(0, max]`, so
/// callers that failed together do not retry in lockstep (review finding 44). The random bits are
/// the 48 leading random bits of a v4 UUID, which the gear already generates; no new dependency.
pub fn full_jitter(max: Duration) -> Duration {
    let random = u128::from(uuid::Uuid::new_v4().as_u64_pair().0 >> 16);
    let nanos = max.as_nanos();
    let cut = (nanos * random) >> 48;
    Duration::from_nanos(u64::try_from(nanos - cut).unwrap_or(u64::MAX))
}
