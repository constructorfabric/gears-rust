//! `FakeClusterLeaders` (DESIGN 11 §4.12): a `LeaderElectionV1` backend the test drives — it
//! grants and revokes leadership per candidate, so a test can force a leadership loss or make two
//! candidates lead at once (V09 and the stale-leader cases).
//!
//! The double is a `cluster-sdk` backend only. A test binds it as the leader election of Rating's
//! cluster profile through the cluster wiring over the standalone cache; that wiring is the
//! Cluster gear's implementation and a plugin, so it stays in the tests' dev-dependencies and
//! never in the gear's.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{
    ClusterError, ElectionConfig, LeaderElectionBackend, LeaderElectionFeatures, LeaderStatus,
    LeaderWatch, LeaderWatchSender,
};
use tokio::sync::Mutex;

/// A leader-election backend whose decisions the test makes.
#[derive(Clone, Default)]
pub struct FakeClusterLeaders {
    candidates: Arc<Mutex<HashMap<String, Vec<LeaderWatchSender>>>>,
}

impl FakeClusterLeaders {
    /// A backend with no candidate yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits until `count` candidates joined the election `scoped_name` (`bss-rating/<leaf>`).
    ///
    /// # Panics
    ///
    /// If they have not joined within five seconds.
    pub async fn wait_for_candidates(&self, scoped_name: &str, count: usize) {
        for _ in 0..500 {
            if self.candidate_count(scoped_name).await >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{count} candidates never joined {scoped_name}");
    }

    /// The candidates of `scoped_name` so far.
    pub async fn candidate_count(&self, scoped_name: &str) -> usize {
        self.candidates
            .lock()
            .await
            .get(scoped_name)
            .map_or(0, Vec::len)
    }

    /// Makes candidate `index` of `scoped_name` a leader. Granting a second candidate without
    /// revoking the first gives two concurrent leaders, as a partition or a paused holder can.
    pub async fn grant(&self, scoped_name: &str, index: usize) {
        self.send(scoped_name, index, LeaderStatus::Leader).await;
    }

    /// Forces candidate `index` of `scoped_name` to lose its leadership.
    pub async fn revoke(&self, scoped_name: &str, index: usize) {
        self.send(scoped_name, index, LeaderStatus::Lost).await;
    }

    /// Offers `status` to the candidate without waiting, so the lock is never held across an
    /// `await`: a driver that does not read cannot block the other calls. A full channel records
    /// the status in the snapshot and reports a `Lagged` gap, as a real backend does.
    async fn send(&self, scoped_name: &str, index: usize, status: LeaderStatus) {
        let mut candidates = self.candidates.lock().await;
        if let Some(sender) = candidates
            .get_mut(scoped_name)
            .and_then(|c| c.get_mut(index))
        {
            // A candidate that already left has no receiver; that is not the test's concern.
            let _offered = sender.try_send_status(status);
        }
    }
}

#[async_trait]
impl LeaderElectionBackend for FakeClusterLeaders {
    fn features(&self) -> LeaderElectionFeatures {
        LeaderElectionFeatures::new(true)
    }

    async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
        let (sender, mut resigns, watch) = LeaderWatch::channel(16, LeaderStatus::Follower);
        // Answer every resign at once, as a backend that released the claim.
        tokio::spawn(async move {
            while let Some(responder) = resigns.recv().await {
                responder.respond(Ok(()));
            }
        });
        self.candidates
            .lock()
            .await
            .entry(name.to_owned())
            .or_default()
            .push(sender);
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
