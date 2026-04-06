use std::time::Duration;

use sea_orm::ConnectionTrait;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::super::handler::HandlerResult;
use super::super::strategy::{ProcessContext, ProcessingStrategy};
use super::super::taskward::{Directive, WorkerAction};
use super::super::types::OutboxError;
use crate::Db;

/// Report emitted by a processor execution cycle.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProcessorReport {
    /// The partition that was processed.
    pub partition_id: i64,
    /// Number of messages dispatched to the handler.
    pub messages_processed: u32,
    /// Outcome of the handler invocation.
    pub handler_result: HandlerResult,
}

/// Per-partition adaptive batch sizing state machine.
///
/// Degrades to single-message mode on failure, ramps back up on consecutive
/// successes. Analogous to TCP slow start.
#[derive(Debug, Clone)]
pub struct PartitionMode {
    pub state: PartitionModeState,
    pub consecutive_failures: u32,
}

impl PartitionMode {
    /// Create a new `PartitionMode` in normal state.
    pub fn new() -> Self {
        Self {
            state: PartitionModeState::Normal,
            consecutive_failures: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PartitionModeState {
    /// Normal operation — use configured `batch_size`.
    Normal,
    /// Degraded after failure — process fewer messages at a time.
    /// Ramps back up (doubling) on consecutive successes until reaching
    /// the configured `batch_size`, then transitions back to `Normal`.
    Degraded {
        effective_size: u32,
        consecutive_successes: u32,
    },
}

impl PartitionMode {
    /// Returns the effective batch size for this mode.
    pub(crate) fn effective_batch_size(&self, configured: u32) -> u32 {
        match &self.state {
            PartitionModeState::Normal => configured,
            PartitionModeState::Degraded { effective_size, .. } => *effective_size,
        }
    }

    /// Compute the current retry backoff based on consecutive failures.
    ///
    /// Returns `base` when no failures have occurred, then escalates
    /// exponentially (base * 2^(failures-1)) capped at `max`.
    pub fn current_backoff(&self, base: Duration, max: Duration) -> Duration {
        let failures = self.consecutive_failures;
        if failures == 0 {
            return base;
        }
        #[allow(clippy::cast_possible_wrap)]
        let exp = (failures.saturating_sub(1) as i32).min(30);
        base.mul_f64(2.0_f64.powi(exp)).min(max)
    }

    /// Transition after a handler result.
    ///
    /// `processed_count`: how many messages were successfully processed before
    /// the batch ended. `Some` for `PerMessageAdapter` handlers, `None` for batch
    /// handlers. On Retry/Reject, degradation uses `max(pc, 1)` when known,
    /// or falls back to 1 when `None` (batch handler — we can't know where
    /// the failure occurred).
    ///
    /// `degradation_threshold`: how many consecutive failures before the batch
    /// size degrades. Set to 1 for immediate degradation (legacy behavior).
    pub(crate) fn transition(
        &mut self,
        result: &HandlerResult,
        configured_batch_size: u32,
        processed_count: Option<u32>,
        degradation_threshold: u32,
    ) {
        match result {
            HandlerResult::Success => {
                self.consecutive_failures = 0;
                match &mut self.state {
                    PartitionModeState::Normal => {}
                    PartitionModeState::Degraded {
                        effective_size,
                        consecutive_successes,
                    } => {
                        *consecutive_successes += 1;
                        // Double the effective size on each consecutive success
                        let next = effective_size.saturating_mul(2).min(configured_batch_size);
                        if next >= configured_batch_size {
                            self.state = PartitionModeState::Normal;
                        } else {
                            *effective_size = next;
                        }
                    }
                }
            }
            HandlerResult::Retry { .. } => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= degradation_threshold {
                    // Degrade: use max(processed_count, 1) as the new effective
                    // size. If the handler processed some messages before failing,
                    // we know the failure is at position pc+1, so we degrade to
                    // max(pc, 1) to isolate the poison message. For batch handlers
                    // (None), fall back to 1 (most conservative).
                    let degrade_to = processed_count.map_or(1, |pc| pc.max(1));
                    self.state = PartitionModeState::Degraded {
                        effective_size: degrade_to,
                        consecutive_successes: 0,
                    };
                }
            }
            HandlerResult::Reject { .. } => {
                // Reject is terminal — the message is poison and has been
                // dead-lettered, so the cursor advances past it. Reset failure
                // count (no backoff needed) but degrade immediately to isolate
                // any further poison messages in the next batch.
                self.consecutive_failures = 0;
                let degrade_to = processed_count.map_or(1, |pc| pc.max(1));
                self.state = PartitionModeState::Degraded {
                    effective_size: degrade_to,
                    consecutive_successes: 0,
                };
            }
        }
    }
}

/// A per-partition processor parameterized by its processing strategy.
///
/// Each instance owns exactly one `partition_id` and runs as a long-lived
/// tokio task. The strategy (`TransactionalStrategy` or `DecoupledStrategy`)
/// is baked in at compile time via monomorphization.
pub struct PartitionProcessor<S: ProcessingStrategy> {
    strategy: S,
    partition_id: i64,
    tuning: super::super::types::WorkerTuning,
    db: Db,
    partition_mode: PartitionMode,
}

impl<S: ProcessingStrategy> PartitionProcessor<S> {
    pub fn new(
        strategy: S,
        partition_id: i64,
        tuning: super::super::types::WorkerTuning,
        db: Db,
    ) -> Self {
        Self {
            strategy,
            partition_id,
            tuning,
            db,
            partition_mode: PartitionMode::new(),
        }
    }
}

impl<S: ProcessingStrategy> WorkerAction for PartitionProcessor<S> {
    type Payload = ProcessorReport;
    type Error = OutboxError;

    async fn execute(
        &mut self,
        _cancel: &CancellationToken,
    ) -> Result<Directive<ProcessorReport>, OutboxError> {
        let (backend, dialect) = {
            let sea_conn = self.db.sea_internal();
            let b = sea_conn.get_database_backend();
            (b, super::super::dialect::Dialect::from(b))
        };

        let effective_size = self
            .partition_mode
            .effective_batch_size(self.tuning.batch_size);

        let ctx = ProcessContext {
            db: &self.db,
            backend,
            dialect,
            partition_id: self.partition_id,
        };

        let result = self.strategy.process(&ctx, effective_size).await?;

        if let Some(pr) = result {
            let has_more = pr.count >= effective_size;
            let clamped_pc = pr.processed_count.map(|pc| pc.min(pr.count));
            self.partition_mode.transition(
                &pr.handler_result,
                self.tuning.batch_size,
                clamped_pc,
                self.tuning.degradation_threshold,
            );
            if pr.count > 0 {
                debug!(
                    partition_id = self.partition_id,
                    count = pr.count,
                    mode = ?self.partition_mode,
                    "partition batch complete"
                );
            }
            let report = ProcessorReport {
                partition_id: self.partition_id,
                messages_processed: pr.count,
                handler_result: pr.handler_result.clone(),
            };
            match pr.handler_result {
                HandlerResult::Success => {
                    if has_more {
                        Ok(Directive::Proceed(report))
                    } else {
                        Ok(Directive::Idle(report))
                    }
                }
                HandlerResult::Retry { .. } => {
                    let backoff = self
                        .partition_mode
                        .current_backoff(self.tuning.retry_base, self.tuning.retry_max);
                    Ok(Directive::Sleep(backoff, report))
                }
                HandlerResult::Reject { .. } => {
                    // Cursor advanced past the poison message — proceed
                    // immediately to process the next batch without backoff.
                    Ok(Directive::Proceed(report))
                }
            }
        } else {
            Ok(Directive::Idle(ProcessorReport {
                partition_id: self.partition_id,
                messages_processed: 0,
                handler_result: HandlerResult::Success,
            }))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "processor_tests.rs"]
mod tests;
