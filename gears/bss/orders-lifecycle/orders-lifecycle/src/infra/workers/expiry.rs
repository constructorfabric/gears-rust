//! The per-state TTL expiry and draft auto-void slots (DESIGN §3.8 roster keys `expiry` and
//! `draft-auto-void`). This package supplies the scheduling, locking, discovery connection and
//! metrics; the bodies (TTL policy evaluation, locked-row recheck through
//! `lock_target_order`, and the engine's `transition_internal`) are Stage 5 deliveries and are
//! explicitly unavailable until then.
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit_db::Db;

use super::metrics::WorkerMetrics;
use super::{WorkerError, WorkerKind, WorkerSettings};
use crate::infra::maintenance::TaskGrant;

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub candidates: u64,
    pub transitioned: u64,
    /// Candidates whose locked-row recheck found them changed, vanished or no longer eligible.
    pub skipped: u64,
}

/// A sweep body: discover due candidates through the granted discovery connection, recheck
/// each under the aggregate lock and transition through the engine's internal entry.
#[async_trait]
pub trait TransitionSweep: Send + Sync {
    /// # Errors
    /// `NotGranted`/`NotConfigured` when the body is not delivered; store failures otherwise.
    async fn sweep(
        &self,
        grant: &TaskGrant<'_>,
        discovery: &Db,
        batch: u64,
        cancel: &CancellationToken,
    ) -> Result<SweepReport, WorkerError>;
}

/// The undelivered body: the slot is scheduled and reported unavailable; nothing is read.
#[derive(Debug, Default, Clone, Copy)]
pub struct SweepUnavailable;
#[async_trait]
impl TransitionSweep for SweepUnavailable {
    async fn sweep(
        &self,
        _: &TaskGrant<'_>,
        _: &Db,
        _: u64,
        _: &CancellationToken,
    ) -> Result<SweepReport, WorkerError> {
        Err(WorkerError::NotGranted)
    }
}

/// One bounded sweep pass.
///
/// # Errors
/// As the body.
pub async fn run_pass(
    kind: WorkerKind,
    body: &dyn TransitionSweep,
    discovery: &Db,
    grant: &TaskGrant<'_>,
    settings: &WorkerSettings,
    metrics: &dyn WorkerMetrics,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let report = body
        .sweep(grant, discovery, settings.sweep_batch, cancel)
        .await?;
    metrics.sweep(kind, report.candidates, report.transitioned, report.skipped);
    Ok(())
}
