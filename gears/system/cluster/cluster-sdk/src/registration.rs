// Created: 2026-06-10 by Constructor Tech
//! `ClientHub` registration and deregistration helpers, keyed per profile per
//! primitive (DESIGN §3.6).
//!
//! These are the write-side counterpart to the per-primitive resolvers: a
//! resolver looks a backend up under `profile_scope(profile)`, and these
//! helpers register and remove it under the same scope. The follow-up wiring
//! crate composes them — it iterates the operator-declared profile×primitive
//! matrix and calls one `register_*_backend` per cell at startup and the
//! matching `deregister_*_backend` at shutdown, after which a resolve for that
//! profile fails with [`ClusterError::ProfileNotBound`].
//!
//! The profile is a runtime `&str` (not a typed [`ClusterProfile`] marker)
//! because wiring reads profile names from operator configuration. The name is
//! validated through [`profile_scope`]; an invalid name is rejected with
//! [`ClusterError::InvalidName`] before any hub mutation.
//!
//! **Registration is last-write-wins.** Each `register_*_backend` inserts
//! unconditionally into the hub, so registering a second backend for the same
//! profile×primitive cell silently replaces the first and still returns
//! `Ok(())` — there is no already-bound signal. The wiring crate must therefore
//! treat the operator-declared profile×primitive matrix as the source of
//! uniqueness (reject duplicate cells in config) rather than relying on these
//! helpers to detect a double-bind.
//!
//! [`ClusterProfile`]: crate::profile::ClusterProfile

use std::sync::Arc;

use toolkit::client_hub::ClientHub;

use crate::error::ClusterError;
use crate::profile::profile_scope;
use crate::{
    ClusterCacheBackend, DistributedLockBackend, LeaderElectionBackend, ServiceDiscoveryBackend,
};

/// Registers a cache backend for `profile` so consumers resolving the cache
/// primitive for that profile receive it.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule; the hub is left unchanged in that case.
pub fn register_cache_backend(
    hub: &ClientHub,
    profile: &str,
    backend: Arc<dyn ClusterCacheBackend>,
) -> Result<(), ClusterError> {
    let scope = profile_scope(profile)?;
    hub.register_scoped::<dyn ClusterCacheBackend>(scope, backend);
    Ok(())
}

/// Removes the cache backend registered for `profile`. Later resolves for that
/// profile then fail with [`ClusterError::ProfileNotBound`].
///
/// Returns `Ok(true)` if a backend was present and removed, `Ok(false)` if no
/// cache backend was bound for the profile.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule.
pub fn deregister_cache_backend(hub: &ClientHub, profile: &str) -> Result<bool, ClusterError> {
    let scope = profile_scope(profile)?;
    Ok(hub
        .remove_scoped::<dyn ClusterCacheBackend>(&scope)
        .is_some())
}

/// Registers a leader-election backend for `profile`.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule; the hub is left unchanged in that case.
pub fn register_leader_election_backend(
    hub: &ClientHub,
    profile: &str,
    backend: Arc<dyn LeaderElectionBackend>,
) -> Result<(), ClusterError> {
    let scope = profile_scope(profile)?;
    hub.register_scoped::<dyn LeaderElectionBackend>(scope, backend);
    Ok(())
}

/// Removes the leader-election backend registered for `profile`.
///
/// Returns `Ok(true)` if a backend was present and removed, `Ok(false)`
/// otherwise.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule.
pub fn deregister_leader_election_backend(
    hub: &ClientHub,
    profile: &str,
) -> Result<bool, ClusterError> {
    let scope = profile_scope(profile)?;
    Ok(hub
        .remove_scoped::<dyn LeaderElectionBackend>(&scope)
        .is_some())
}

/// Registers a distributed-lock backend for `profile`.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule; the hub is left unchanged in that case.
pub fn register_lock_backend(
    hub: &ClientHub,
    profile: &str,
    backend: Arc<dyn DistributedLockBackend>,
) -> Result<(), ClusterError> {
    let scope = profile_scope(profile)?;
    hub.register_scoped::<dyn DistributedLockBackend>(scope, backend);
    Ok(())
}

/// Removes the distributed-lock backend registered for `profile`.
///
/// Returns `Ok(true)` if a backend was present and removed, `Ok(false)`
/// otherwise.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule.
pub fn deregister_lock_backend(hub: &ClientHub, profile: &str) -> Result<bool, ClusterError> {
    let scope = profile_scope(profile)?;
    Ok(hub
        .remove_scoped::<dyn DistributedLockBackend>(&scope)
        .is_some())
}

/// Registers a service-discovery backend for `profile`.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule; the hub is left unchanged in that case.
pub fn register_service_discovery_backend(
    hub: &ClientHub,
    profile: &str,
    backend: Arc<dyn ServiceDiscoveryBackend>,
) -> Result<(), ClusterError> {
    let scope = profile_scope(profile)?;
    hub.register_scoped::<dyn ServiceDiscoveryBackend>(scope, backend);
    Ok(())
}

/// Removes the service-discovery backend registered for `profile`.
///
/// Returns `Ok(true)` if a backend was present and removed, `Ok(false)`
/// otherwise.
///
/// # Errors
/// Returns [`ClusterError::InvalidName`] if `profile` violates the cluster
/// name rule.
pub fn deregister_service_discovery_backend(
    hub: &ClientHub,
    profile: &str,
) -> Result<bool, ClusterError> {
    let scope = profile_scope(profile)?;
    Ok(hub
        .remove_scoped::<dyn ServiceDiscoveryBackend>(&scope)
        .is_some())
}

#[cfg(test)]
#[path = "registration_tests.rs"]
mod registration_tests;
