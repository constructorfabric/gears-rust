// Created: 2026-06-10 by Constructor Tech
//! The per-primitive scoping wrapper for leader election (DESIGN §3.8).

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ClusterError;
use crate::leader::backend::LeaderElectionBackend;
use crate::leader::types::{ElectionConfig, LeaderElectionFeatures};
use crate::leader::watch::LeaderWatch;
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
}
