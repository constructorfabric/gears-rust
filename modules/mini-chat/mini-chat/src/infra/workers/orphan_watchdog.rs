//! Orphan watchdog — detects and finalizes turns abandoned by crashed pods.
//!
//! Requires leader election: exactly one active watchdog instance per environment.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::OrphanWatchdogConfig;
use mini_chat_sdk::RequesterType;

use crate::domain::model::finalization::OrphanFinalizationInput;
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::repos::{MessageRepository, TurnRepository};
use crate::domain::service::DbProvider;
use crate::domain::service::finalization_service::FinalizationService;
use crate::infra::db::entity::chat_turn::Model as TurnModel;
use crate::infra::leader::{LeaderElector, work_fn};

/// Dependencies for the orphan watchdog scan-finalize loop.
pub struct OrphanWatchdogDeps<TR: TurnRepository + 'static, MR: MessageRepository + 'static> {
    pub finalization_svc: Arc<FinalizationService<TR, MR>>,
    pub turn_repo: Arc<TR>,
    pub db: Arc<DbProvider>,
    /// Metrics port for recording orphan detection/finalization metrics.
    /// Wired in Phase 6 when orphan-specific metric methods are added to `MiniChatMetricsPort`.
    pub metrics: Arc<dyn MiniChatMetricsPort>,
}

/// Maximum number of orphan candidates to process per scan tick.
const BATCH_LIMIT: u32 = 100;

/// Run the orphan watchdog under leader election.
///
/// Returns when `cancel` fires (module shutdown) or on unrecoverable error.
pub async fn run<TR: TurnRepository + 'static, MR: MessageRepository + 'static>(
    elector: Arc<dyn LeaderElector>,
    config: OrphanWatchdogConfig,
    deps: OrphanWatchdogDeps<TR, MR>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    if !config.enabled {
        info!("orphan_watchdog: disabled, skipping");
        return Ok(());
    }

    info!(
        scan_interval_secs = config.scan_interval_secs,
        timeout_secs = config.timeout_secs,
        "orphan_watchdog: starting",
    );

    let interval = Duration::from_secs(config.scan_interval_secs);
    let deps = Arc::new(deps);

    elector
        .run_role(
            "orphan-watchdog",
            cancel,
            work_fn(move |cancel| {
                let interval = interval;
                let deps = Arc::clone(&deps);
                let config = config.clone();
                async move {
                    let mut ticker = tokio::time::interval(interval);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                    loop {
                        tokio::select! {
                            _ = ticker.tick() => {
                                let scan_start = std::time::Instant::now();

                                let result = scan_and_finalize(&deps, &config, &cancel).await;

                                // Always record scan duration — even on error — so
                                // dashboards detect silent watchdog failures.
                                deps.metrics.record_orphan_scan_duration_seconds(
                                    scan_start.elapsed().as_secs_f64(),
                                );

                                if let Err(()) = result {
                                    // scan_and_finalize already logged the error.
                                    continue;
                                }
                                if result == Ok(true) {
                                    // Shutdown requested mid-scan.
                                    return Ok(());
                                }
                            }
                            () = cancel.cancelled() => {
                                info!("orphan_watchdog: shutting down");
                                return Ok(());
                            }
                        }
                    }
                }
            }),
        )
        .await
}

#[allow(
    clippy::cognitive_complexity,
    reason = "linear scan-finalize loop, complexity from match arms"
)]
/// Run one scan-finalize cycle. Returns:
/// - `Ok(false)` — scan completed normally
/// - `Ok(true)` — shutdown requested mid-scan
/// - `Err(())` — scan failed (already logged)
async fn scan_and_finalize<TR: TurnRepository + 'static, MR: MessageRepository + 'static>(
    deps: &OrphanWatchdogDeps<TR, MR>,
    config: &OrphanWatchdogConfig,
    cancel: &CancellationToken,
) -> Result<bool, ()> {
    let conn = deps.db.conn().map_err(|e| {
        error!(error = %e, "orphan_watchdog: failed to get DB connection");
    })?;

    let candidates = deps
        .turn_repo
        .find_orphan_candidates(&conn, config.timeout_secs, BATCH_LIMIT)
        .await
        .map_err(|e| {
            error!(error = %e, "orphan_watchdog: scan query failed");
        })?;

    if candidates.is_empty() {
        debug!("orphan_watchdog: scan completed, no candidates");
    } else {
        info!(count = candidates.len(), "orphan_watchdog: scan completed");
    }

    for turn in &candidates {
        if cancel.is_cancelled() {
            info!("orphan_watchdog: shutting down mid-scan");
            return Ok(true);
        }

        deps.metrics.record_orphan_detected("stale_progress");

        let input = orphan_input_from_turn(turn);
        match deps
            .finalization_svc
            .finalize_orphan_turn(input, config.timeout_secs)
            .await
        {
            Ok(true) => {
                deps.metrics.record_orphan_finalized("stale_progress");
                info!(
                    turn_id = %turn.id,
                    tenant_id = %turn.tenant_id,
                    chat_id = %turn.chat_id,
                    "orphan_watchdog: finalized orphan turn"
                );
            }
            Ok(false) => {
                debug!(
                    turn_id = %turn.id,
                    "orphan_watchdog: CAS lost (already finalized or progress renewed)"
                );
            }
            Err(e) => {
                error!(
                    turn_id = %turn.id,
                    error = %e,
                    "orphan_watchdog: finalization error"
                );
            }
        }
    }

    Ok(false)
}

/// Build [`OrphanFinalizationInput`] from an infra entity.
/// Lives in the infra layer to avoid domain→infra coupling.
fn orphan_input_from_turn(turn: &TurnModel) -> OrphanFinalizationInput {
    let requester_type = match turn.requester_type.as_str() {
        "system" => RequesterType::System,
        "user" => RequesterType::User,
        other => {
            warn!(
                requester_type = other,
                "orphan_watchdog: unknown requester_type, defaulting to User"
            );
            RequesterType::User
        }
    };
    OrphanFinalizationInput {
        turn_id: turn.id,
        tenant_id: turn.tenant_id,
        chat_id: turn.chat_id,
        request_id: turn.request_id,
        user_id: turn.requester_user_id,
        requester_type,
        effective_model: turn.effective_model.clone(),
        reserve_tokens: turn.reserve_tokens,
        max_output_tokens_applied: turn.max_output_tokens_applied,
        reserved_credits_micro: turn.reserved_credits_micro,
        policy_version_applied: turn.policy_version_applied,
        minimal_generation_floor_applied: turn.minimal_generation_floor_applied,
        started_at: turn.started_at,
        web_search_completed_count: u32::try_from(turn.web_search_completed_count)
            .unwrap_or_else(|_| {
                warn!(turn_id = %turn.id, value = turn.web_search_completed_count, "negative web_search_completed_count in DB, defaulting to 0");
                0
            }),
        code_interpreter_completed_count: u32::try_from(turn.code_interpreter_completed_count)
            .unwrap_or_else(|_| {
                warn!(turn_id = %turn.id, value = turn.code_interpreter_completed_count, "negative code_interpreter_completed_count in DB, defaulting to 0");
                0
            }),
    }
}
#[cfg(test)]
#[path = "orphan_watchdog_tests.rs"]
mod tests;
