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

    /// Joins under the scoped election name, then — on a win — strips the prefix
    /// from the returned token so the caller sees the *unprefixed* name it asked for
    /// ([`LeaseToken::name`] is the consumer name, not the backend's cache key) and
    /// records this layer's prefix in [`LeaseToken::scope`] so a later
    /// `renew`/`resign` can prove the token came from *this* view. A follower's
    /// `Ok(None)` is propagated unchanged. `owner`, `fence` and `deadline` are
    /// preserved; a name-bearing error is stripped of the prefix too (via
    /// [`scope::strip_error_name`]) so the error path matches the success path.
    async fn join(
        &self,
        name: &str,
        owner: &str,
        config: ElectionConfig,
    ) -> Result<Option<LeaseToken>, ClusterError> {
        let token = self
            .inner
            .join(&scope::apply(&self.prefix, name), owner, config)
            .await
            .map_err(|e| scope::strip_error_name(&self.prefix, e))?;
        Ok(token.map(|mut token| {
            token.name = scope::strip(&self.prefix, &token.name).to_owned();
            token.scope.push(self.prefix.clone());
            token
        }))
    }

    /// Verifies the token was minted by *this* view, then re-applies the prefix to a
    /// clone before delegating, so the scoped election name the inner backend
    /// recorded on `join` is reconstructed. This view's prefix must be the *top* layer
    /// of [`LeaseToken::scope`] (it was pushed last on the way out); a claim token from
    /// a different scoped view — or a raw-backend token whose scope is empty — fails
    /// with [`ClusterError::InvalidName`]/[`scope::SCOPE_MISMATCH`] rather than being
    /// silently re-prefixed into a phantom key that reports `LockExpired`
    /// ([`scope::reapply_for_token`] holds the shared check). A name-bearing error is
    /// stripped of the prefix too (via [`scope::strip_error_name`]), so the error path
    /// returns the bare consumer name — through a chain, each layer peels its own. The
    /// caller's token is left unchanged.
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let scoped = scope::reapply_for_token(&self.prefix, token)?;
        self.inner
            .renew(&scoped, ttl)
            .await
            .map_err(|e| scope::strip_error_name(&self.prefix, e))
    }

    /// Verifies-then-strips this view's scope and re-applies the prefix on a clone,
    /// for the reason [`renew`](Self::renew) gives; name-bearing errors are stripped
    /// of the prefix the same way.
    async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let scoped = scope::reapply_for_token(&self.prefix, token)?;
        self.inner
            .resign(&scoped)
            .await
            .map_err(|e| scope::strip_error_name(&self.prefix, e))
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

    /// A leader backend that always loses the election, so the scoped wrapper's
    /// `Ok(None)` follower arm — the ordinary, documented outcome — is exercised.
    struct FollowerBackend {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LeaderElectionBackend for FollowerBackend {
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

        async fn join(
            &self,
            name: &str,
            _owner: &str,
            _config: ElectionConfig,
        ) -> Result<Option<LeaseToken>, ClusterError> {
            self.seen.lock().expect("lock").push(name.to_owned());
            Ok(None)
        }

        async fn renew(&self, _token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            Ok(())
        }

        async fn resign(&self, _token: &LeaseToken) -> Result<(), ClusterError> {
            Ok(())
        }
    }

    /// Losing the election is the ordinary outcome, so the scoped wrapper must
    /// propagate `Ok(None)` — while still keying the election under the scoped name.
    #[tokio::test]
    async fn join_returns_none_for_a_follower() {
        let backend = Arc::new(FollowerBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = ScopedLeaderElectionBackend::new(
            Arc::clone(&backend) as Arc<dyn LeaderElectionBackend>,
            scope::validated_prefix("event-broker").expect("valid prefix"),
        );
        let result = wrapper
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join");
        assert!(
            result.is_none(),
            "a follower gets Ok(None) back through the scoped wrapper"
        );
        // The election was still keyed under the scoped name.
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-leader"]
        );
    }

    /// A claim token minted through a *chain* of scoped wrappers round-trips through
    /// the same chain: join records the full effective name, the caller sees the bare
    /// name, and renew/resign reconstruct that name layer by layer.
    #[tokio::test]
    async fn a_chained_scope_round_trips_join_renew_resign() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner = scoped(Arc::clone(&backend), "a");
        let outer = ScopedLeaderElectionBackend::new(
            Arc::new(inner),
            scope::validated_prefix("b").expect("valid prefix"),
        );

        let token = outer
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        assert_eq!(token.name, "shard-leader");
        assert_eq!(token.scope, ["a/", "b/"]);

        outer
            .renew(&token, Duration::from_secs(30))
            .await
            .expect("renew");
        outer.resign(&token).await.expect("resign");

        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["a/b/shard-leader", "a/b/shard-leader", "a/b/shard-leader"]
        );
    }

    /// A raw-backend claim token (bare name, empty scope) presented to a scoped view
    /// fails loudly with `SCOPE_MISMATCH` instead of a silent `LockExpired`.
    #[tokio::test]
    async fn a_raw_backend_token_is_rejected_by_a_scoped_renew() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        let raw = LeaseToken::new("shard-leader", "cand-a", 1);
        let err = wrapper
            .renew(&raw, Duration::from_secs(30))
            .await
            .expect_err("a raw token is not from this view");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH)
        );
        assert!(
            backend.seen.lock().expect("lock").is_empty(),
            "the wrapper must reject before delegating"
        );
    }

    /// A claim token from a *sibling* scope fails against a differently-scoped view.
    #[tokio::test]
    async fn a_sibling_scope_token_is_rejected_by_a_scoped_resign() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let team_a = scoped(Arc::clone(&backend), "team-a");
        let team_b = scoped(Arc::clone(&backend), "team-b");
        let token = team_a
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        assert_eq!(token.scope, ["team-a/"]);
        let err = team_b
            .resign(&token)
            .await
            .expect_err("team-a's token is not team-b's");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH)
        );
    }

    /// A full-chain claim token presented to a *sub*-wrapper of that chain fails: the
    /// sub-wrapper's prefix is not the suffix of the accumulated scope.
    #[tokio::test]
    async fn a_full_chain_token_is_rejected_by_a_sub_wrapper_renew() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner: Arc<dyn LeaderElectionBackend> = Arc::new(scoped(Arc::clone(&backend), "a"));
        let outer = ScopedLeaderElectionBackend::new(
            Arc::clone(&inner),
            scope::validated_prefix("b").expect("valid prefix"),
        );
        let token = outer
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        assert_eq!(token.scope, ["a/", "b/"]);
        let err = inner
            .renew(&token, Duration::from_secs(30))
            .await
            .expect_err("the full-chain token is not the inner view's");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH)
        );
    }

    /// The multi-segment boundary case (the leader mirror of the lock guard): a claim
    /// minted under `.scoped("eu/team")` (scope `["eu/team/"]`) presented to a sibling
    /// `.scoped("team")` view. A flat-string `strip_suffix("team/")` on `"eu/team/"`
    /// would wrongly accept it; the layered top-of-stack check rejects it loudly.
    #[tokio::test]
    async fn a_multi_segment_token_is_rejected_by_a_sibling_view_sharing_its_tail_segment() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let eu_team = scoped(Arc::clone(&backend), "eu/team");
        let team = scoped(Arc::clone(&backend), "team");
        let token = eu_team
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        assert_eq!(token.scope, ["eu/team/"]);
        let err = team
            .renew(&token, Duration::from_secs(30))
            .await
            .expect_err("eu/team's claim is not team's, despite the shared tail segment");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH),
            "the shared-tail-segment claim must be rejected loudly, not accepted into a phantom key"
        );
        assert_eq!(
            backend.seen.lock().expect("lock").len(),
            1,
            "only the join reached the backend; the mismatched renew rejected before delegating"
        );
    }

    /// A leader backend whose `join` fails with a name-bearing error, so the scoped
    /// wrapper's error path can be proven to strip the prefix (the leader mirror of
    /// the lock read-path strip).
    struct FailingJoinBackend;

    #[async_trait]
    impl LeaderElectionBackend for FailingJoinBackend {
        fn features(&self) -> LeaderElectionFeatures {
            LeaderElectionFeatures::new(true)
        }

        async fn elect(&self, _name: &str) -> Result<LeaderWatch, ClusterError> {
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

        async fn join(
            &self,
            name: &str,
            _owner: &str,
            _config: ElectionConfig,
        ) -> Result<Option<LeaseToken>, ClusterError> {
            Err(ClusterError::LockExpired {
                name: name.to_owned(),
            })
        }

        async fn renew(&self, _token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            Ok(())
        }

        async fn resign(&self, _token: &LeaseToken) -> Result<(), ClusterError> {
            Ok(())
        }
    }

    /// On the read path, a name-bearing error from `join` must return the *bare*
    /// election name, not the internal `event-broker/shard-leader`.
    #[tokio::test]
    async fn join_strips_the_prefix_from_a_name_bearing_error() {
        let wrapper = ScopedLeaderElectionBackend::new(
            Arc::new(FailingJoinBackend),
            scope::validated_prefix("event-broker").expect("valid prefix"),
        );
        let err = wrapper
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect_err("join failed");
        assert!(
            matches!(err, ClusterError::LockExpired { name } if name == "shard-leader"),
            "the error must name the bare `shard-leader`"
        );
    }

    /// A leader backend that wins `join` but fails `renew`/`resign` with a name-bearing
    /// error carrying the scoped name, so the wrapper's *write*-path error stripping
    /// (the mirror of the lock write-path strip) can be proven.
    struct RenewResignFailsBackend;

    #[async_trait]
    impl LeaderElectionBackend for RenewResignFailsBackend {
        fn features(&self) -> LeaderElectionFeatures {
            LeaderElectionFeatures::new(true)
        }

        async fn elect(&self, _name: &str) -> Result<LeaderWatch, ClusterError> {
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

        async fn join(
            &self,
            name: &str,
            owner: &str,
            _config: ElectionConfig,
        ) -> Result<Option<LeaseToken>, ClusterError> {
            Ok(Some(LeaseToken::new(name, owner, 1)))
        }

        async fn renew(&self, token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            Err(ClusterError::LockExpired {
                name: token.name.clone(),
            })
        }

        async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
            Err(ClusterError::LockExpired {
                name: token.name.clone(),
            })
        }
    }

    /// The write-path counterpart of `join_strips_the_prefix_from_a_name_bearing_error`:
    /// a name-bearing error from `renew`/`resign` must return the *bare* election name,
    /// not the internal `event-broker/shard-leader`.
    #[tokio::test]
    async fn renew_and_resign_strip_the_prefix_from_a_name_bearing_error() {
        let wrapper = ScopedLeaderElectionBackend::new(
            Arc::new(RenewResignFailsBackend),
            scope::validated_prefix("event-broker").expect("valid prefix"),
        );
        let token = wrapper
            .join("shard-leader", "cand-a", ElectionConfig::default())
            .await
            .expect("join")
            .expect("became leader");
        let renew_err = wrapper
            .renew(&token, Duration::from_secs(30))
            .await
            .expect_err("renew fails");
        assert!(
            matches!(renew_err, ClusterError::LockExpired { name } if name == "shard-leader"),
            "renew error must name the bare `shard-leader`, not the scoped key"
        );
        let resign_err = wrapper.resign(&token).await.expect_err("resign fails");
        assert!(
            matches!(resign_err, ClusterError::LockExpired { name } if name == "shard-leader"),
            "resign error must name the bare `shard-leader`, not the scoped key"
        );
    }
}
