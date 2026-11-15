//! `Reaper` worker: expired-subscription and idempotency-key cleanup
//! (`DESIGN.md` §3.7 Key Invariants). Shell only: nothing constructs or runs
//! it. The design runs it under `LeaderElectionV1` (`evbk.worker.reaper`,
//! `DESIGN.md` "`ClusterCapabilities` (Platform Dependency)") so exactly one
//! instance runs cluster-wide (not yet implemented, tracked in #4346/#4347).

#[derive(Debug, Default)]
pub struct Reaper;

impl Reaper {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub async fn run_once(&self) {
        todo!("subscription + idempotency-key expiry sweep - lands with #4346/#4347")
    }
}
