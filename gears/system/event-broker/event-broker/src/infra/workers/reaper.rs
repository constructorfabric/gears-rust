//! `Reaper` worker: expired-subscription and idempotency-key cleanup
//! (`DESIGN.md` §3.7 Key Invariants). Runs under `LeaderElectionV1`
//! (`evbk.worker.reaper`, `DESIGN.md:761`) so exactly one instance runs
//! cluster-wide. Shell only - a runnable implementation lands with
//! #4346/#4347.

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
