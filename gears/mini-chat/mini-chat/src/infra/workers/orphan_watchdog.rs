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
/// Returns when `cancel` fires (gear shutdown) or on unrecoverable error.
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
#[tracing::instrument(name = "worker", skip_all, fields(worker = "orphan_watchdog"))]
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
        file_search_completed_count: u32::try_from(turn.file_search_completed_count)
            .unwrap_or_else(|_| {
                warn!(turn_id = %turn.id, value = turn.file_search_completed_count, "negative file_search_completed_count in DB, defaulting to 0");
                0
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn disabled_returns_immediately() {
        let elector = crate::infra::leader::noop();
        let cancel = CancellationToken::new();
        let config = OrphanWatchdogConfig {
            enabled: false,
            ..Default::default()
        };
        let deps = test_deps().await;
        let result = run(elector, config, deps, cancel).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn shutdown_on_cancel() {
        let elector = crate::infra::leader::noop();
        let cancel = CancellationToken::new();
        let config = OrphanWatchdogConfig::default();
        let deps = test_deps().await;

        let c = cancel.clone();
        let handle = tokio::spawn(async move { run(elector, config, deps, c).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(matches!(result, Ok(Ok(Ok(())))));
    }

    /// Build minimal test deps using the concrete infra repos and an in-memory `SQLite` DB.
    async fn test_deps() -> OrphanWatchdogDeps<
        crate::infra::db::repo::turn_repo::TurnRepository,
        crate::infra::db::repo::message_repo::MessageRepository,
    > {
        use crate::domain::ports::metrics::NoopMetrics;
        use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};
        use crate::infra::db::repo::message_repo::MessageRepository as MsgRepo;
        use crate::infra::db::repo::turn_repo::TurnRepository as TurnRepo;

        let db = mock_db_provider(inmem_db().await);
        let turn_repo = Arc::new(TurnRepo);
        let message_repo = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));

        let finalization_svc = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo),
            Arc::clone(&message_repo),
            Arc::new(NoopQuotaSettler)
                as Arc<dyn crate::domain::service::quota_settler::QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::new(NoopMetrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        OrphanWatchdogDeps {
            finalization_svc,
            turn_repo,
            db,
            metrics: Arc::new(NoopMetrics),
        }
    }

    struct NoopQuotaSettler;

    #[async_trait::async_trait]
    impl crate::domain::service::quota_settler::QuotaSettler for NoopQuotaSettler {
        async fn settle_in_tx(
            &self,
            _tx: &toolkit_db::secure::DbTx<'_>,
            _scope: &toolkit_security::AccessScope,
            _input: crate::domain::model::quota::SettlementInput,
        ) -> Result<crate::domain::model::quota::SettlementOutcome, crate::domain::error::DomainError>
        {
            Ok(crate::domain::model::quota::SettlementOutcome {
                settlement_method: crate::domain::model::quota::SettlementMethod::Estimated,
                actual_credits_micro: 0,
                charged_tokens: 0,
                overshoot_capped: false,
            })
        }
    }

    struct NoopOutboxEnqueuer;

    #[async_trait::async_trait]
    impl crate::domain::repos::OutboxEnqueuer for NoopOutboxEnqueuer {
        async fn enqueue_usage_event(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: mini_chat_sdk::UsageEvent,
        ) -> Result<crate::domain::repos::Wake, crate::domain::repos::OutboxError> {
            Ok(crate::domain::repos::Wake::empty())
        }
        async fn enqueue_attachment_cleanup(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: crate::domain::repos::AttachmentCleanupEvent,
        ) -> Result<crate::domain::repos::Wake, crate::domain::repos::OutboxError> {
            Ok(crate::domain::repos::Wake::empty())
        }
        async fn enqueue_chat_cleanup(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: crate::domain::repos::ChatCleanupEvent,
        ) -> Result<crate::domain::repos::Wake, crate::domain::repos::OutboxError> {
            Ok(crate::domain::repos::Wake::empty())
        }
        async fn enqueue_audit_event(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: crate::domain::model::audit_envelope::AuditEnvelope,
        ) -> Result<crate::domain::repos::Wake, crate::domain::repos::OutboxError> {
            Ok(crate::domain::repos::Wake::empty())
        }
        async fn enqueue_thread_summary(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _payload: crate::domain::repos::ThreadSummaryTaskPayload,
        ) -> Result<crate::domain::repos::Wake, crate::domain::repos::OutboxError> {
            Ok(crate::domain::repos::Wake::empty())
        }
    }

    // ── orphan_input_from_turn ──

    fn stub_turn(
        web_search_completed_count: i32,
        code_interpreter_completed_count: i32,
        file_search_completed_count: i32,
    ) -> TurnModel {
        use crate::infra::db::entity::chat_turn::TurnState;
        TurnModel {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            chat_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            requester_type: "user".to_owned(),
            requester_user_id: Some(Uuid::new_v4()),
            state: TurnState::Running,
            provider_name: None,
            provider_response_id: None,
            assistant_message_id: None,
            error_code: None,
            error_detail: None,
            reserve_tokens: None,
            max_output_tokens_applied: None,
            reserved_credits_micro: None,
            policy_version_applied: None,
            effective_model: None,
            minimal_generation_floor_applied: None,
            web_search_enabled: false,
            web_search_completed_count,
            code_interpreter_completed_count,
            file_search_completed_count,
            deleted_at: None,
            replaced_by_request_id: None,
            started_at: time::OffsetDateTime::now_utc(),
            last_progress_at: None,
            completed_at: None,
            updated_at: time::OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn orphan_input_maps_tool_counts() {
        let turn = stub_turn(3, 5, 7);
        let input = orphan_input_from_turn(&turn);
        assert_eq!(input.web_search_completed_count, 3);
        assert_eq!(input.code_interpreter_completed_count, 5);
        assert_eq!(input.file_search_completed_count, 7);
    }

    #[test]
    fn orphan_input_clamps_negative_web_search_count() {
        let turn = stub_turn(-1, 0, 0);
        let input = orphan_input_from_turn(&turn);
        assert_eq!(input.web_search_completed_count, 0);
    }

    #[test]
    fn orphan_input_clamps_negative_code_interpreter_count() {
        let turn = stub_turn(0, -2, 0);
        let input = orphan_input_from_turn(&turn);
        assert_eq!(input.code_interpreter_completed_count, 0);
    }

    #[test]
    fn orphan_input_clamps_negative_file_search_count() {
        let turn = stub_turn(0, 0, -3);
        let input = orphan_input_from_turn(&turn);
        assert_eq!(input.file_search_completed_count, 0);
    }
}
