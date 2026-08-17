// Created: 2026-06-10 by Constructor Tech
//! The per-primitive scoping wrapper for leader election (DESIGN §3.8).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::ClusterError;
use crate::leader::backend::LeaderElectionBackend;
use crate::leader::types::{ElectionConfig, LeaderElectionFeatures};
use crate::leader::watch::LeaderWatch;
use crate::lease::LeaseToken;
use crate::scope;

/// A delegating [`LeaderElectionBackend`] that prepends a validated scope prefix
/// to every election `name` on the write path. There is no read-path strip: a
/// [`LeaderWatch`] carries no election name (DESIGN §3.8 table), so the consumer
/// never observes the prefixed name. Scoping composes by stacking wrappers.
pub struct ScopedLeaderElectionBackend {
    inner: Arc<dyn LeaderElectionBackend>,
    prefix: String,
}

impl ScopedLeaderElectionBackend {
    /// Wraps `inner` with the effective `prefix` (already validated and
    /// separator-terminated by [`scope::validated_prefix`]).
    pub fn new(inner: Arc<dyn LeaderElectionBackend>, prefix: String) -> Self {
        Self { inner, prefix }
    }
}

#[async_trait]
impl LeaderElectionBackend for ScopedLeaderElectionBackend {
    fn features(&self) -> LeaderElectionFeatures {
        self.inner.features()
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
        self.inner.elect(&scope::apply(&self.prefix, name)).await
    }

    async fn elect_with_config(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError> {
        self.inner
            .elect_with_config(&scope::apply(&self.prefix, name), config)
            .await
    }

    async fn join(
        &self,
        name: &str,
        owner: &str,
        config: ElectionConfig,
    ) -> Result<Option<LeaseToken>, ClusterError> {
        self.inner
            .join(&scope::apply(&self.prefix, name), owner, config)
            .await
    }

    /// Forwarded verbatim: the token names the *scoped* election and is presented
    /// back unchanged, so neither re-applying nor stripping the prefix is correct
    /// (DESIGN §3.8).
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        self.inner.renew(token, ttl).await
    }

    /// Forwarded verbatim, for the reason [`renew`](Self::renew) gives.
    async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        self.inner.resign(token).await
    }

    /// Forwarded: a probe carries no name to scope, and a scoped view must not
    /// answer the trait's `Ok(())` default over an unreachable backend.
    async fn probe(&self) -> Result<(), ClusterError> {
        self.inner.probe().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::ScopedLeaderElectionBackend;
    use crate::error::ClusterError;
    use crate::leader::backend::LeaderElectionBackend;
    use crate::leader::types::{ElectionConfig, LeaderElectionFeatures, LeaderStatus};
    use crate::leader::watch::LeaderWatch;
    use crate::scope;

    /// Records the election name the backend was asked to join.
    struct RecordingBackend {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LeaderElectionBackend for RecordingBackend {
        fn features(&self) -> LeaderElectionFeatures {
            LeaderElectionFeatures::new(true)
        }

        async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
            self.seen.lock().expect("lock").push(name.to_owned());
            let (_tx, _resign, watch) = LeaderWatch::channel(1, LeaderStatus::Follower);
            Ok(watch)
        }

        async fn elect_with_config(
            &self,
            name: &str,
            _config: ElectionConfig,
        ) -> Result<LeaderWatch, ClusterError> {
            self.elect(name).await
        }

        async fn probe(&self) -> Result<(), ClusterError> {
            // Recorded under a name no election could take, so a forwarded probe is
            // distinguishable from the trait's `Ok(())` default.
            self.seen.lock().expect("lock").push("<probe>".to_owned());
            Ok(())
        }
    }

    fn scoped(inner: Arc<RecordingBackend>, prefix: &str) -> ScopedLeaderElectionBackend {
        ScopedLeaderElectionBackend::new(
            inner,
            scope::validated_prefix(prefix).expect("valid prefix"),
        )
    }

    #[tokio::test]
    async fn elect_prepends_the_prefix() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        assert!(wrapper.elect("shard-leader").await.is_ok());
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-leader"]
        );
    }

    #[tokio::test]
    async fn scoping_composes_when_nested() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner = scoped(Arc::clone(&backend), "event-broker");
        let outer = ScopedLeaderElectionBackend::new(
            Arc::new(inner),
            scope::validated_prefix("shard-0").expect("valid prefix"),
        );
        assert!(outer.elect("leader").await.is_ok());
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-0/leader"]
        );
    }

    /// A probe carries no name to scope, but it must still be forwarded — through
    /// nesting too — or a scoped view answers the trait's `Ok(())` default over an
    /// unreachable backend.
    #[tokio::test]
    async fn probe_is_forwarded_through_every_scoping_layer() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner = scoped(Arc::clone(&backend), "event-broker");
        let outer = ScopedLeaderElectionBackend::new(
            Arc::new(inner),
            scope::validated_prefix("shard-0").expect("valid prefix"),
        );

        assert!(outer.probe().await.is_ok());
        assert_eq!(backend.seen.lock().expect("lock").as_slice(), ["<probe>"]);
    }
}
