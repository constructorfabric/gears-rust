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
/// to every election `name` on the write path, and — on the store-owned-leases
/// path — strips it back off the [`LeaseToken::name`] it returns. A
/// [`LeaderWatch`] carries no election name (DESIGN §3.8 table), so it needs no
/// read-path strip, but a `LeaseToken` exposes `name` as the *unprefixed* consumer
/// name, so the `join` path strips on the way out and `renew`/`resign` re-apply on
/// the way back in. Scoping composes by stacking wrappers.
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

    /// Joins under the scoped election name, then strips the prefix from the
    /// returned token so the caller sees the *unprefixed* name it asked for
    /// ([`LeaseToken::name`] is the consumer name, not the backend's cache key).
    /// `owner`, `fence` and `deadline` are preserved.
    async fn join(
        &self,
        name: &str,
        owner: &str,
        config: ElectionConfig,
    ) -> Result<Option<LeaseToken>, ClusterError> {
        let token = self
            .inner
            .join(&scope::apply(&self.prefix, name), owner, config)
            .await?;
        Ok(token.map(|mut token| {
            token.name = scope::strip(&self.prefix, &token.name).to_owned();
            token
        }))
    }

    /// Re-applies the prefix to a clone of the caller's token before delegating, so
    /// the scoped election name the inner backend recorded on `join` is
    /// reconstructed. The caller's token is left unchanged; only `name` is rewritten.
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let mut scoped = token.clone();
        scoped.name = scope::apply(&self.prefix, &token.name);
        self.inner.renew(&scoped, ttl).await
    }

    /// Re-applies the prefix on a clone, for the reason [`renew`](Self::renew) gives.
    async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let mut scoped = token.clone();
        scoped.name = scope::apply(&self.prefix, &token.name);
        self.inner.resign(&scoped).await
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
    use std::time::Duration;

    use async_trait::async_trait;

    use super::ScopedLeaderElectionBackend;
    use crate::error::ClusterError;
    use crate::leader::backend::LeaderElectionBackend;
    use crate::leader::types::{ElectionConfig, LeaderElectionFeatures, LeaderStatus};
    use crate::leader::watch::LeaderWatch;
    use crate::lease::LeaseToken;
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

        // The token half records the scoped name it was handed, exactly as the
        // `elect` half does, so `join_prepends_the_prefix` can assert scoping
        // applies to the store-owned-leases path a `Join` RPC takes too.
        async fn join(
            &self,
            name: &str,
            owner: &str,
            _config: ElectionConfig,
        ) -> Result<Option<LeaseToken>, ClusterError> {
            self.seen.lock().expect("lock").push(name.to_owned());
            Ok(Some(LeaseToken::new(name, owner, 1)))
        }

        // renew/resign record the token name they were handed, so a test can
        // assert the wrapper re-applied the prefix before delegating.
        async fn renew(&self, token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            self.seen.lock().expect("lock").push(token.name.clone());
            Ok(())
        }

        async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
            self.seen.lock().expect("lock").push(token.name.clone());
            Ok(())
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

    /// The token path scopes the same way `elect` does on the *write* side — the
    /// name the backend records is the `prefix/name` the wrapper composed — but the
    /// returned [`LeaseToken::name`] is stripped back to the bare election name,
    /// because that field is contractually unprefixed.
    #[tokio::test]
    async fn join_prepends_the_prefix_and_strips_the_returned_name() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        let token = wrapper
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        // The inner backend keyed the election under the scoped name...
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-leader"]
        );
        // ...but the caller gets the unprefixed name back, with owner/fence intact.
        assert_eq!(token.name, "shard-leader");
        assert_eq!(token.owner, "cand-a");
        assert_eq!(token.fence, 1);
    }

    /// A token minted by the wrapper (bare name) must round-trip back to the inner
    /// backend under the scoped election name on `renew`/`resign`, or the caller's
    /// token would fail to resolve to the record `join` created.
    #[tokio::test]
    async fn renew_and_resign_reapply_the_prefix() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        let token = wrapper
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        assert_eq!(token.name, "shard-leader");

        wrapper
            .renew(&token, Duration::from_secs(30))
            .await
            .expect("renew");
        wrapper.resign(&token).await.expect("resign");

        // The caller's token is untouched by the round-trip...
        assert_eq!(token.name, "shard-leader");
        // ...and both delegations reached the inner backend under the scoped name.
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            [
                "event-broker/shard-leader",
                "event-broker/shard-leader",
                "event-broker/shard-leader"
            ]
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
