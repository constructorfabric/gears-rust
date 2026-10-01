//! Upload reaper — fails attachments left in `pending` / `uploaded` by an
//! upload whose request was dropped (client disconnect, api-gateway timeout).
//!
//! The upload runs inside the HTTP request. When the request future is
//! dropped, the service never records the outcome, so the row keeps counting
//! against the chat's limits and its provider file is never deleted. The
//! reaper marks such rows `failed` with `error_code = upload_abandoned` and,
//! when a provider file was stored, enqueues the regular attachment cleanup
//! event, which deletes the file.
//!
//! Requires leader election: exactly one active reaper per environment.

use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::UploadReaperConfig;
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::repos::{AttachmentCleanupEvent, OutboxEnqueuer, Wake};
use crate::domain::service::DbProvider;
use crate::infra::db::entity::attachment::Model as AttachmentModel;
use crate::infra::db::repo::attachment_repo::AttachmentRepository;
use crate::infra::leader::{LeaderElector, work_fn};

/// Dependencies for the upload reaper loop.
pub struct UploadReaperDeps {
    pub db: Arc<DbProvider>,
    pub outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
    pub metrics: Arc<dyn MiniChatMetricsPort>,
}

/// Maximum number of stale rows processed per scan tick.
const BATCH_LIMIT: u64 = 100;

/// Run the upload reaper under leader election.
///
/// Returns when `cancel` fires (gear shutdown) or on unrecoverable error.
pub async fn run(
    elector: Arc<dyn LeaderElector>,
    config: UploadReaperConfig,
    deps: UploadReaperDeps,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    if !config.enabled {
        info!("upload_reaper: disabled, skipping");
        return Ok(());
    }

    info!(
        scan_interval_secs = config.scan_interval_secs,
        stale_after_secs = config.stale_after_secs,
        "upload_reaper: starting",
    );

    let interval = Duration::from_secs(config.scan_interval_secs);
    let deps = Arc::new(deps);

    elector
        .run_role(
            "upload-reaper",
            cancel,
            work_fn(move |cancel| {
                let deps = Arc::clone(&deps);
                let config = config.clone();
                async move {
                    let mut ticker = tokio::time::interval(interval);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        tokio::select! {
                            _ = ticker.tick() => {
                                let scan_start = std::time::Instant::now();
                                let result = scan_and_reap(&deps, &config, &cancel).await;
                                deps.metrics.record_upload_reaper_scan_duration_seconds(
                                    scan_start.elapsed().as_secs_f64(),
                                );
                                if result == ScanOutcome::Cancelled {
                                    return Ok(());
                                }
                            }
                            () = cancel.cancelled() => {
                                info!("upload_reaper: shutting down");
                                return Ok(());
                            }
                        }
                    }
                }
            }),
        )
        .await
}

/// Result of one scan.
#[derive(Debug, PartialEq, Eq)]
pub enum ScanOutcome {
    Done,
    /// Shutdown was requested mid-scan.
    Cancelled,
    /// The scan query failed (already logged); the next tick retries.
    Failed,
}

/// Run one scan.
#[tracing::instrument(name = "worker", skip_all, fields(worker = "upload_reaper"))]
pub async fn scan_and_reap(
    deps: &UploadReaperDeps,
    config: &UploadReaperConfig,
    cancel: &CancellationToken,
) -> ScanOutcome {
    let cutoff =
        OffsetDateTime::now_utc() - time::Duration::seconds(config.stale_after_secs.cast_signed());
    let conn = match deps.db.conn() {
        Ok(conn) => conn,
        Err(e) => {
            error!(error = %e, "upload_reaper: failed to get DB connection");
            return ScanOutcome::Failed;
        }
    };
    let stale = match AttachmentRepository
        .find_stale_uploads(&conn, cutoff, BATCH_LIMIT)
        .await
    {
        Ok(stale) => stale,
        Err(e) => {
            error!(error = %e, "upload_reaper: scan query failed");
            return ScanOutcome::Failed;
        }
    };
    if stale.is_empty() {
        debug!("upload_reaper: scan completed, no stale uploads");
        return ScanOutcome::Done;
    }
    info!(count = stale.len(), "upload_reaper: stale uploads found");

    for row in stale {
        if cancel.is_cancelled() {
            info!("upload_reaper: shutting down mid-scan");
            return ScanOutcome::Cancelled;
        }
        reap_one(deps, row, cutoff).await;
    }
    ScanOutcome::Done
}

#[allow(
    clippy::cognitive_complexity,
    reason = "one transaction plus the outcome logging; the tracing macros add most of the score"
)]
async fn reap_one(deps: &UploadReaperDeps, row: AttachmentModel, cutoff: OffsetDateTime) {
    let attachment_id = row.id;
    let secondary_file_id = row.secondary_file_id.clone();
    let from_status = row.status.clone();
    let from_label = from_status.to_string();
    let outbox_enqueuer = Arc::clone(&deps.outbox_enqueuer);

    let result = deps
        .db
        .transaction(|tx| {
            let row = row.clone();
            let from_status = from_status.clone();
            let outbox_enqueuer = Arc::clone(&outbox_enqueuer);
            Box::pin(async move {
                let has_file = row.provider_file_id.is_some();
                let affected = AttachmentRepository
                    .cas_abandon_upload(tx, row.tenant_id, row.id, from_status, cutoff, has_file)
                    .await
                    .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;
                if affected == 0 {
                    // The upload finished or was deleted meanwhile.
                    return Ok(None);
                }
                if !has_file {
                    return Ok(Some(Wake::empty()));
                }
                let event = AttachmentCleanupEvent {
                    event_type: "attachment_upload_abandoned".to_owned(),
                    tenant_id: row.tenant_id,
                    chat_id: row.chat_id,
                    attachment_id: row.id,
                    provider_file_id: row.provider_file_id.clone(),
                    vector_store_id: None,
                    storage_backend: row.storage_backend.clone(),
                    attachment_kind: row.attachment_kind.to_string(),
                    deleted_at: OffsetDateTime::now_utc(),
                    // A secondary (Anthropic) copy is not deleted: its
                    // cleanup reference needs the chat's upstream alias,
                    // which the reaper does not resolve.
                    secondary_ref: None,
                };
                let wake = outbox_enqueuer
                    .enqueue_attachment_cleanup(tx, event)
                    .await
                    .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;
                Ok(Some(wake))
            })
        })
        .await;

    match result {
        Ok(Some(wake)) => {
            wake.fire();
            deps.metrics.record_upload_abandoned(&from_label);
            info!(%attachment_id, from_status = %from_label, "upload_reaper: marked abandoned upload failed");
            if let Some(secondary_file_id) = secondary_file_id {
                // Not deleted here (see `secondary_ref` above): name it so the
                // leftover copy can be found and removed.
                warn!(%attachment_id, secondary_file_id, "upload_reaper: secondary provider copy is not deleted");
            }
        }
        Ok(None) => {
            debug!(%attachment_id, "upload_reaper: row no longer stale");
        }
        Err(e) => {
            warn!(%attachment_id, error = %e, "upload_reaper: failed to reap row");
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "upload_reaper_tests.rs"]
mod tests;
