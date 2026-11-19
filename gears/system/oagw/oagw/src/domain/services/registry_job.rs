//! One run of the registry reconcile, as the gear drives it: read the
//! registry instances and, when one names no tenant, the root tenant, then
//! reconcile, behind the gate on a database backend (`registry_state`) or
//! directly in memory.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use parking_lot::RwLock;
use tenant_resolver_sdk::TenantResolverClient;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use tracing::{error, info, warn};

use super::RegistryProvisioner;
use super::registry_reconcile::{
    RunControl, bounded, log_counts, needs_root_tenant, reconcile_registry,
};
#[cfg(test)]
use super::registry_state::jitter;
use super::registry_state::{Gate, RunOutcome, gated_reconcile, log_outcome};
use super::system_identity::bootstrap_ctx;
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::provisioning_outcome;
use crate::domain::type_provisioning::TypeProvisioningService;

/// Everything a registry run needs.
#[domain_model]
#[derive(Clone)]
pub(crate) struct RegistryJob {
    /// The registry write path.
    pub(crate) cp: Arc<dyn RegistryProvisioner>,
    /// The registry instances.
    pub(crate) provisioning: Arc<dyn TypeProvisioningService>,
    /// The root tenant, for instances that name none.
    pub(crate) tenant_resolver: Arc<dyn TenantResolverClient>,
    /// Failures and run outcomes.
    pub(crate) metrics: Arc<dyn OagwMetricsPort>,
    /// The gate on a database backend; `None` in memory, where each process
    /// owns its store.
    pub(crate) gate: Option<Gate>,
    /// Spreads the background task's waits ([`jitter`]; identity in tests).
    pub(crate) jitter: fn(Duration) -> Duration,
}

impl RegistryJob {
    /// One run: list the instances, resolve the root tenant if an instance
    /// needs it, reconcile.
    /// Counts the run in `registry_provisioning_runs_total` and logs it.
    /// Only an infrastructure error returns `Err`; so does a panic, so the
    /// background task records it and retries instead of dying unnoticed
    /// (a lease it held expires with the TTL).
    pub(crate) async fn run_once(&self, cancel: &CancellationToken) -> anyhow::Result<RunOutcome> {
        let result = match AssertUnwindSafe(self.run(cancel)).catch_unwind().await {
            Ok(result) => result,
            Err(panic) => Err(anyhow::anyhow!(
                "registry provisioning run panicked: {}",
                panic_message(panic.as_ref())
            )),
        };
        let outcome = match &result {
            Ok(outcome) => {
                match (&self.gate, outcome) {
                    // In memory there is no gate to report, only the counts.
                    (None, RunOutcome::Reconciled(counts)) => log_counts(counts),
                    (gate, outcome) => {
                        log_outcome(outcome, gate.as_ref().and_then(|g| g.generation));
                    }
                }
                outcome.metric_label()
            }
            Err(_) => provisioning_outcome::FAILED,
        };
        self.metrics.record_registry_provisioning_run(outcome);
        result
    }

    async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<RunOutcome> {
        // Each call is bounded, and a stop does not wait for them.
        let inputs = async {
            let upstreams = bounded(
                "listing the types-registry upstreams",
                self.provisioning.list_upstreams(),
            )
            .await??;
            let routes = bounded(
                "listing the types-registry routes",
                self.provisioning.list_routes(),
            )
            .await??;
            let root_tenant_id = if needs_root_tenant(&upstreams, &routes) {
                let ctx = bootstrap_ctx()?;
                let root = bounded(
                    "resolving the root tenant",
                    self.tenant_resolver.get_root_tenant(&ctx),
                )
                .await?
                .map_err(|e| anyhow::anyhow!("Failed to resolve root tenant: {e}"))?;
                Some(root.id.0)
            } else {
                None
            };
            anyhow::Ok((root_tenant_id, upstreams, routes))
        };
        let (root_tenant_id, upstreams, routes) = tokio::select! {
            () = cancel.cancelled() => return Ok(RunOutcome::Cancelled),
            inputs = inputs => inputs?,
        };
        match &self.gate {
            Some(gate) => {
                gated_reconcile(
                    gate,
                    &*self.cp,
                    &*self.metrics,
                    cancel,
                    &upstreams,
                    &routes,
                    root_tenant_id,
                )
                .await
            }
            None => reconcile_registry(
                &*self.cp,
                &*self.metrics,
                &Cancellable(cancel),
                &upstreams,
                &routes,
                root_tenant_id,
            )
            .await
            .map(RunOutcome::Reconciled),
        }
    }
}

/// An in-memory run: nothing else coordinates it, and a stop ends it
/// between writes.
struct Cancellable<'a>(&'a CancellationToken);

impl RunControl for Cancellable<'_> {
    fn cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// The message of a panic payload (`panic!` with a literal or a format).
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

// ---------------------------------------------------------------------------
// The background task
// ---------------------------------------------------------------------------

/// Where provisioning stands, for the healthcheck.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProvisioningStatus {
    /// No run has finished yet, or the last one was cut off by a stop.
    Pending,
    /// The stored rows match the registry content (or a newer generation's).
    Settled,
    /// The last run skipped instances, kept upstreams, or stopped early.
    NotClean,
    /// The last run failed on an infrastructure error; it is retried.
    Failed,
}

/// The provisioning status, shared by the run and the healthcheck.
#[domain_model]
#[derive(Clone)]
pub(crate) struct ProvisioningState(Arc<RwLock<ProvisioningStatus>>);

impl Default for ProvisioningState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(ProvisioningStatus::Pending)))
    }
}

impl ProvisioningState {
    /// The current status.
    pub(crate) fn get(&self) -> ProvisioningStatus {
        *self.0.read()
    }

    /// Replaces the current status.
    pub(crate) fn set(&self, status: ProvisioningStatus) {
        *self.0.write() = status;
    }

    /// The status a finished run leaves.
    pub(crate) fn record(&self, outcome: &anyhow::Result<RunOutcome>) {
        match outcome {
            Ok(RunOutcome::Cancelled) => {}
            Ok(outcome) if outcome.is_settled() => self.set(ProvisioningStatus::Settled),
            Ok(_) => self.set(ProvisioningStatus::NotClean),
            Err(_) => self.set(ProvisioningStatus::Failed),
        }
    }
}

/// First wait before retrying a failed run.
pub(crate) const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// Longest wait before retrying a failed run.
pub(crate) const BACKOFF_MAX: Duration = Duration::from_mins(5);

/// Exponential backoff between failed runs.
#[derive(Debug)]
struct Backoff {
    next: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self { next: BACKOFF_MIN }
    }

    fn reset(&mut self) {
        self.next = BACKOFF_MIN;
    }

    fn next(&mut self) -> Duration {
        let wait = self.next;
        self.next = (self.next * 2).min(BACKOFF_MAX);
        wait
    }
}

/// Sets the status to `Pending` when dropped armed: the run it guards did
/// not finish, so the store may be partly reconciled and the next `start`
/// must reconcile at once.
struct CutOffGuard<'a> {
    status: &'a ProvisioningState,
    armed: bool,
}

impl Drop for CutOffGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.status.set(ProvisioningStatus::Pending);
        }
    }
}

/// The registry reconcile as a background task: after `first_wait`, a run,
/// then a re-check after `recheck` (±20%) whatever the outcome, for the life
/// of the process (on a database an unchanged state costs one query; in
/// memory, with no state row, a re-check reconciles and writes only what
/// changed); a failed run is retried with backoff from 1 s to 5 min (±20%).
/// Never ends on its own; `cancel` stops it between writes, and on a
/// database the run releases its lease on the way out.
///
/// NOT cancel-safe: a run cancelled or aborted midway keeps the writes it
/// made, so the store may be partly reconciled (on a database an aborted run
/// also holds its lease until the TTL). Such a run leaves the status
/// `Pending`, so the next `start` reconciles at once.
pub(crate) async fn run_registry_loop(
    job: RegistryJob,
    first_wait: Duration,
    recheck: Duration,
    status: ProvisioningState,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new();
    if !first_wait.is_zero() {
        tokio::select! {
            () = cancel.cancelled() => {
                info!("Registry provisioning stopped");
                return;
            }
            () = tokio::time::sleep(first_wait) => {}
        }
    }
    loop {
        let mut cut_off = CutOffGuard {
            status: &status,
            armed: true,
        };
        let outcome = job.run_once(&cancel).await;
        if cancel.is_cancelled() {
            // A run that settled before the stop leaves the store complete.
            if matches!(&outcome, Ok(o) if o.is_settled()) {
                cut_off.armed = false;
                status.record(&outcome);
            }
            info!("Registry provisioning stopped");
            return;
        }
        cut_off.armed = false;
        status.record(&outcome);
        let wait = match outcome {
            Ok(_) => {
                backoff.reset();
                (job.jitter)(recheck)
            }
            Err(e) => {
                let delay = backoff.next();
                let wait = (job.jitter)(delay);
                // A dependency may not answer for a while after startup (the
                // root tenant before account-management creates it): warn
                // until the backoff reaches its cap, then report an error.
                if delay < BACKOFF_MAX {
                    warn!(error = %format!("{e:#}"), retry_in = ?wait, "Registry provisioning run failed; retrying");
                } else {
                    error!(error = %format!("{e:#}"), retry_in = ?wait, "Registry provisioning run failed; retrying");
                }
                wait
            }
        };
        tokio::select! {
            () = cancel.cancelled() => {
                info!("Registry provisioning stopped");
                return;
            }
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
#[path = "registry_job_tests.rs"]
mod tests;
