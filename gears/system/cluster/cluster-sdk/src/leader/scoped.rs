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
/// [`LeaderWatch`] carries no election name (DESIGN §3.8 table), and a claim
/// [`LeaseToken`] carries the *scoped* name the inner backend recorded, so it is
/// presented back verbatim. Scoping composes by stacking wrappers.
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

    /// Joins under the scoped election name. The result is the inner backend's,
    /// unchanged — a follower's `Ok(None)` included — so a won claim's
    /// [`LeaseToken::name`] is the scoped name the election is stored under, which
    /// is what lets [`renew`](Self::renew) and [`resign`](Self::resign) forward it
    /// without any prefix arithmetic.
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
    /// (DESIGN §3.8). A claim token therefore addresses the election it was won
    /// for whichever view it is presented to, and survives its wire mirror intact
    /// (invariant I7).
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
    use std::time::Duration;

    use async_trait::async_trait;

    use super::ScopedLeaderElectionBackend;
    use crate::error::ClusterError;
    use crate::leader::backend::LeaderElectionBackend;
    use crate::leader::types::{ElectionConfig, LeaderElectionFeatures, LeaderStatus};
    use crate::leader::watch::LeaderWatch;
    use crate::lease::LeaseToken;
    use crate::scope;

    /// Records every election name it was asked to join and every claim token it
    /// was handed back. `wins` decides whether `join` hands out a claim or answers
    /// as a follower.
    struct RecordingBackend {
        wins: bool,
        seen: Mutex<Vec<String>>,
        tokens: Mutex<Vec<LeaseToken>>,
    }

    impl RecordingBackend {
        fn new(wins: bool) -> Arc<Self> {
            Arc::new(Self {
                wins,
                seen: Mutex::new(Vec::new()),
                tokens: Mutex::new(Vec::new()),
            })
        }
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

        // The token half records the name it was handed, exactly as the `elect`
        // half does, so scoping can be asserted on the path a `Join` RPC takes too.
        async fn join(
            &self,
            name: &str,
            owner: &str,
            _config: ElectionConfig,
        ) -> Result<Option<LeaseToken>, ClusterError> {
            self.seen.lock().expect("lock").push(name.to_owned());
            Ok(self.wins.then(|| LeaseToken::new(name, owner, 1)))
        }

        // renew/resign record the whole token they were handed, so a test can
        // assert the wrapper forwarded it byte-for-byte.
        async fn renew(&self, token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            self.tokens.lock().expect("lock").push(token.clone());
            Ok(())
        }

        async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
            self.tokens.lock().expect("lock").push(token.clone());
            Ok(())
        }

        async fn probe(&self) -> Result<(), ClusterError> {
            // Recorded under a name no election could take, so a forwarded probe is
            // distinguishable from the trait's `Ok(())` default.
            self.seen.lock().expect("lock").push("<probe>".to_owned());
            Ok(())
        }
    }

    fn scoped(inner: Arc<dyn LeaderElectionBackend>, prefix: &str) -> ScopedLeaderElectionBackend {
        ScopedLeaderElectionBackend::new(
            inner,
            scope::validated_prefix(prefix).expect("valid prefix"),
        )
    }

    #[tokio::test]
    async fn elect_prepends_the_prefix() {
        let backend = RecordingBackend::new(true);
        let wrapper = scoped(backend.clone(), "event-broker");
        assert!(wrapper.elect("shard-leader").await.is_ok());
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-leader"]
        );
    }

    #[tokio::test]
    async fn scoping_composes_when_nested() {
        let backend = RecordingBackend::new(true);
        let outer = scoped(Arc::new(scoped(backend.clone(), "event-broker")), "shard-0");
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
        let backend = RecordingBackend::new(true);
        let outer = scoped(Arc::new(scoped(backend.clone(), "event-broker")), "shard-0");

        assert!(outer.probe().await.is_ok());
        assert_eq!(backend.seen.lock().expect("lock").as_slice(), ["<probe>"]);
    }

    /// `join` scopes the election name on the way in and hands back the inner
    /// backend's claim token unchanged — its `name` is the scoped election name.
    #[tokio::test]
    async fn join_prepends_the_prefix_and_returns_the_scoped_token() {
        let backend = RecordingBackend::new(true);
        let wrapper = scoped(backend.clone(), "event-broker");
        let token = wrapper
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        assert_eq!(
            token,
            LeaseToken::new("event-broker/shard-leader", "cand-a", 1)
        );
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-leader"]
        );
    }

    /// Losing the election is the ordinary outcome, so the scoped wrapper must
    /// propagate `Ok(None)` — while still keying the election under the scoped name.
    #[tokio::test]
    async fn join_returns_none_for_a_follower() {
        let backend = RecordingBackend::new(false);
        let wrapper = scoped(backend.clone(), "event-broker");
        let result = wrapper
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join");
        assert!(
            result.is_none(),
            "a follower gets Ok(None) back through the scoped wrapper"
        );
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-leader"]
        );
    }

    /// A claim won through a *chain* of scoped wrappers carries the full chained
    /// name, and `renew`/`resign` forward it to the innermost backend unchanged —
    /// no layer re-applies (which would double the prefix) or strips anything.
    #[tokio::test]
    async fn a_chained_token_is_forwarded_verbatim_on_renew_and_resign() {
        let backend = RecordingBackend::new(true);
        let outer = scoped(Arc::new(scoped(backend.clone(), "a")), "b");

        let token = outer
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        assert_eq!(token.name, "a/b/shard-leader");

        outer
            .renew(&token, Duration::from_secs(30))
            .await
            .expect("renew");
        outer.resign(&token).await.expect("resign");

        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["a/b/shard-leader"]
        );
        assert_eq!(
            backend.tokens.lock().expect("lock").as_slice(),
            [token.clone(), token],
            "renew and resign must reach the backend with the token byte-for-byte"
        );
    }

    /// A claim token presented to a *different* view still addresses the election
    /// it was won for: the token carries its own scoped name, so there is no
    /// phantom name to derive. Authority stays with the backend's owner + fence
    /// check, not the view.
    #[tokio::test]
    async fn a_token_presented_to_another_view_still_addresses_its_own_election() {
        let backend = RecordingBackend::new(true);
        let eu_team = scoped(backend.clone(), "eu/team");
        let team = scoped(backend.clone(), "team");

        let token = eu_team
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        team.resign(&token)
            .await
            .expect("resign through a sibling view");

        let forwarded = backend.tokens.lock().expect("lock");
        assert_eq!(
            forwarded[0].name, "eu/team/shard-leader",
            "the sibling view must not re-derive `team/shard-leader`"
        );
    }
}
