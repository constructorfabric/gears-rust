//! Leader election abstraction for background workers.
//!
//! Two implementations behind a feature gate:
//! - [`NoopLeaderElector`] — single-process mode, no coordination overhead.
//! - [`K8sLeaseElector`](k8s_lease::K8sLeaseElector) — k8s `coordination.k8s.io/v1` Lease
//!   (requires `k8s` feature).

use std::future::Future;
use std::pin::Pin;
#[cfg(any(not(feature = "k8s"), test))]
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "k8s")]
pub mod k8s_lease;

// ────────────────────────────────────────────────────────────────────────────
// Public types
// ────────────────────────────────────────────────────────────────────────────

/// Boxed async work function that receives a [`CancellationToken`].
///
/// The token fires when leadership is lost or the gear shuts down.
/// The function may be invoked more than once (after re-election),
/// hence `Fn` and not `FnOnce`.
pub type LeaderWorkFn = Box<
    dyn Fn(CancellationToken) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Wraps a closure into a [`LeaderWorkFn`].
pub fn work_fn<F, Fut>(f: F) -> LeaderWorkFn
where
    F: Fn(CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    Box::new(move |cancel| Box::pin(f(cancel)))
}

// ────────────────────────────────────────────────────────────────────────────
// Trait
// ────────────────────────────────────────────────────────────────────────────

/// Abstraction for leader election.
///
/// Workers wrap their periodic loop inside [`run_role`](LeaderElector::run_role).
/// The elector manages acquisition / renewal / release; the worker implements
/// its logic inside the provided [`LeaderWorkFn`].
#[async_trait]
pub trait LeaderElector: Send + Sync + std::fmt::Debug {
    /// Run `work` only while this instance holds leadership for `role`.
    ///
    /// Returns when `cancel` fires (gear shutdown) or on unrecoverable error.
    ///
    /// # Errors
    ///
    /// Returns an error if leadership cannot be acquired due to an
    /// infrastructure failure (e.g. k8s API unavailable).
    async fn run_role(
        &self,
        role: &str,
        cancel: CancellationToken,
        work: LeaderWorkFn,
    ) -> anyhow::Result<()>;
}

// ────────────────────────────────────────────────────────────────────────────
// Noop implementation (single-process / on-prem)
// ────────────────────────────────────────────────────────────────────────────

/// No-op leader elector for single-process deployments.
///
/// Immediately delegates to the work function with the parent cancel token.
#[derive(Debug)]
#[cfg(any(not(feature = "k8s"), test))]
pub struct NoopLeaderElector;

#[cfg(any(not(feature = "k8s"), test))]
#[async_trait]
impl LeaderElector for NoopLeaderElector {
    async fn run_role(
        &self,
        _role: &str,
        cancel: CancellationToken,
        work: LeaderWorkFn,
    ) -> anyhow::Result<()> {
        work(cancel).await
    }
}

/// Creates a [`NoopLeaderElector`] wrapped in an `Arc`.
#[cfg(any(not(feature = "k8s"), test))]
#[must_use]
pub fn noop() -> Arc<dyn LeaderElector> {
    Arc::new(NoopLeaderElector)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
