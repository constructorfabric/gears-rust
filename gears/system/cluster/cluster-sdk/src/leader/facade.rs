// Created: 2026-06-03 by Constructor Tech
//! The public `LeaderElectionV1` facade — a thin, cloneable handle delegating
//! to the resolved `Arc<dyn LeaderElectionBackend>`.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;

use crate::error::ClusterError;
use crate::leader::backend::LeaderElectionBackend;
use crate::leader::resolver::LeaderElectionResolverBuilder;
use crate::leader::types::{ElectionConfig, LeaderElectionFeatures};
use crate::leader::watch::LeaderWatch;
use crate::restart::ResubscribeFuture;

/// The public leader-election facade. Construct via
/// [`LeaderElectionV1::resolver`]; cloning is cheap (an `Arc` bump).
///
/// # Advisory coordination — not mutual exclusion
///
/// Leader election answers *which* node should run a singleton workload; it is
/// **not** a mutual-exclusion primitive. Under a linearizable backend at most
/// one participant observes itself as leader, but the leadership signal a
/// consumer reads is always a **cached snapshot** that lags backend truth by up
/// to one renewal interval plus a provider round-trip in steady state, and up
/// to a full TTL under partition (DESIGN §3.3 staleness bound). Two participants
/// can therefore transiently believe they are leader.
///
/// Three consumer patterns are available, ordered by tolerance for transient
/// dual-leadership:
///
/// 1. **Tolerant work** — gate each short, idempotent iteration on the cached
///    [`LeaderWatch::is_leader`](crate::leader::LeaderWatch::is_leader) snapshot.
/// 2. **Reactive work** — subscribe to
///    [`LeaderWatch::changed`](crate::leader::LeaderWatch::changed) and cancel
///    leader-only tasks on `Status(Lost)`; narrows but does not eliminate the
///    window.
/// 3. **Mutually exclusive work** — for workloads where two simultaneous writers
///    would corrupt state, combine the reactive pattern with
///    `DistributedLockV1::try_lock` or `ClusterCacheV1::compare_and_swap`
///    (`expected_version` from a prior `get`). The `LockContended` / `CasConflict`
///    is the authoritative "you are no longer the writer" signal.
///
/// Use pattern 3 whenever correctness depends on a single writer.
///
/// Use [`scoped`](Self::scoped) to carve a composable sub-namespace: every
/// election `name` is auto-prefixed (DESIGN §3.8). The auto-restart combinator
/// (`LeaderWatch::auto_restart`) is delivered by the watch-auto-restart feature
/// (DECOMPOSITION §2.8).
#[derive(Clone)]
pub struct LeaderElectionV1 {
    inner: Arc<dyn LeaderElectionBackend>,
}

impl LeaderElectionV1 {
    /// Wraps a resolved backend. Crate-internal: consumers obtain a facade
    /// through the resolver.
    pub(crate) fn from_backend(inner: Arc<dyn LeaderElectionBackend>) -> Self {
        Self { inner }
    }

    /// Static entry point: returns a fluent resolver bound to `hub`.
    pub fn resolver(hub: &ClientHub) -> LeaderElectionResolverBuilder<'_> {
        LeaderElectionResolverBuilder::new(hub)
    }

    /// Returns a sub-namespaced view: every election `name` is auto-prefixed with
    /// `prefix + "/"` (DESIGN §3.8). Scoping composes.
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidName`] if `prefix` violates the scope-prefix
    /// rule (`[a-zA-Z0-9_/-]+`).
    pub fn scoped(&self, prefix: &str) -> Result<Self, ClusterError> {
        let prefix = crate::scope::validated_prefix(prefix)?;
        Ok(Self::from_backend(Arc::new(
            crate::leader::ScopedLeaderElectionBackend::new(Arc::clone(&self.inner), prefix),
        )))
    }

    /// The bound backend's native capability flags.
    #[must_use]
    pub fn features(&self) -> LeaderElectionFeatures {
        self.inner.features()
    }

    /// Joins the named election with the default [`ElectionConfig`], returning a
    /// [`LeaderWatch`]. The claim is renewed automatically and the watch
    /// auto-reenrolls on a transient leadership loss.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
        crate::profile::validate_cluster_name(name)?;
        let mut watch = self.inner.elect(name).await?;
        install_leader_seam(Arc::clone(&self.inner), name.to_owned(), None, &mut watch);
        Ok(watch)
    }

    /// Joins the named election with custom timing.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn elect_with_config(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError> {
        crate::profile::validate_cluster_name(name)?;
        let mut watch = self.inner.elect_with_config(name, config).await?;
        install_leader_seam(
            Arc::clone(&self.inner),
            name.to_owned(),
            Some(config),
            &mut watch,
        );
        Ok(watch)
    }
}

/// Installs a self-reinstalling resubscribe seam that re-runs the election on
/// the bound backend (`elect` when `config` is `None`, else `elect_with_config`
/// with the original timing). Each reconnected watch is re-seamed, so
/// [`LeaderWatch::auto_restart`] reconnects *repeatedly*, not just once.
/// Capturing the backend (whose `async_trait` methods return a concretely-`Send`
/// boxed future) rather than the facade avoids a `Send` inference cycle.
fn install_leader_seam(
    backend: Arc<dyn LeaderElectionBackend>,
    name: String,
    config: Option<ElectionConfig>,
    watch: &mut LeaderWatch,
) {
    watch.set_resubscribe(move || -> ResubscribeFuture<LeaderWatch> {
        let backend = Arc::clone(&backend);
        let name = name.clone();
        Box::pin(async move {
            let mut fresh = match config {
                Some(config) => backend.elect_with_config(&name, config).await?,
                None => backend.elect(&name).await?,
            };
            install_leader_seam(Arc::clone(&backend), name, config, &mut fresh);
            Ok(fresh)
        })
    });
}
