//! Admission outbox (T21): acceptance enqueues the operation UUID in its transaction;
//! [`AdmissionHandler`] calls [`RegistryService::admit`] and maps its result.
//!
//! Leased handlers run outside a transaction so admission can open its own.
//! Delivery is at-least-once; completed operations and terminal items are skipped.
//!
//! Retries block the single partition's cursor (see [`PARTITIONS`]). Permanent
//! failures are dead-lettered immediately; transient failures exhaust
//! `worker.max_delivery_attempts` first. Terminalizing as `admission_abandoned`
//! gives callers a readable outcome and prevents recovery on every boot.
//!
//! Deliveries cut short by the lease timeout increment `attempts` without recording
//! an outcome, so the handler cannot count them itself. Once `attempts` reaches
//! `max_attempts` the delivery in hand skips `registry.admit()` entirely and decides
//! from the stored status: it acks a completed operation and dead-letters anything
//! else. That decision is terminal, so admission runs on at most `max_attempts`
//! deliveries and the one after them ends the message — a hanging admission cannot
//! hold the single partition.
//!
//! Terminal has to mean *returning*, because the strategy converts a handler future
//! dropped at the lease deadline back into a retry. Every database call on that path
//! therefore stops at a [`work_deadline`] taken from the lease `Batch::remaining()`
//! reports — which is why this is a [`LeasedHandler`] and not a per-message one.

use std::sync::{Arc, OnceLock, Weak};

use toolkit_db::outbox::{
    Batch, EnqueueMessage, HandlerResult, LeaseConfig, LeasedHandler, MessageResult, Outbox,
    OutboxError, OutboxHandle, OutboxMessage, OutboxProfile, Partitions, WorkerTuning,
};
use toolkit_db::{Db, DbError, DbTx};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::LEASE_HEADROOM;
use crate::domain::admission::OperationDispatch;
use crate::domain::enums::OperationStatus;
use crate::domain::ports::RecoveryCursor;
use crate::domain::ports::metrics::{AdmissionMetrics, DeliveryOutcome};
use crate::domain::registry_service::{RegistryService, ServiceError};

/// Outbox table prefix shared by migrations, runtime and tests (SPEC §5).
pub const TABLE_PREFIX: &str = "types_registry_outbox";

/// Queue for admission operations.
pub(crate) const QUEUE: &str = "admission";

/// One partition preserves operation order. Entity commits are serialized by
/// `entity_write_order` (D4); more partitions would only parallelize evaluation.
const PARTITIONS: u16 = 1;

/// The message's declared type. Printable ASCII, as the outbox requires.
const PAYLOAD_TYPE: &str = "types_registry.admission_operation";

/// Operations per recovery page. Recovery runs in `init()` before readiness;
/// paging avoids reading the entire `operation` backlog at once during boot.
const RECOVERY_PAGE: u64 = 256;

/// Canonical operation UUID, readable in dead-letter rows.
/// Candidate content stays in operation items (SPEC T21).
#[must_use]
pub fn payload(operation_id: Uuid) -> Vec<u8> {
    operation_id.to_string().into_bytes()
}

/// Parse an operation UUID from the message body.
///
/// # Errors
/// Invalid UTF-8 or UUID; the handler rejects either permanently.
pub fn parse_payload(payload: &[u8]) -> Result<Uuid, ParsePayloadError> {
    let text = std::str::from_utf8(payload).map_err(|_| ParsePayloadError::NotUtf8)?;
    Uuid::parse_str(text).map_err(|_| ParsePayloadError::NotAUuid {
        text: text.to_owned(),
    })
}

/// Why a message body is not an operation UUID.
#[derive(Debug, thiserror::Error)]
pub enum ParsePayloadError {
    #[error("the outbox payload is not UTF-8")]
    NotUtf8,
    #[error("the outbox payload '{text}' is not an operation UUID")]
    NotAUuid { text: String },
}

#[derive(Debug, thiserror::Error)]
#[error("the outbox payload type '{actual}' is not '{expected}'")]
struct UnexpectedPayloadType<'a> {
    actual: &'a str,
    expected: &'static str,
}

/// Startup failures: outbox setup (table prefix or migration) and recovery.
/// Keeps recovery types and `anyhow`-backed [`DbError`] causes that conversion
/// to `OutboxError::Database(DbErr::Custom)` would erase.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("the admission outbox could not be started: {0}")]
    Outbox(#[from] OutboxError),
    #[error("re-enqueueing non-terminal operations failed: {0}")]
    Recovery(#[source] ServiceError),
    #[error("re-enqueueing non-terminal operations failed: {0}")]
    RecoveryEnqueue(#[source] DbError),
}

fn lease_config(operation_timeout: std::time::Duration) -> LeaseConfig {
    LeaseConfig {
        duration: operation_timeout.saturating_add(LEASE_HEADROOM),
        headroom: LEASE_HEADROOM,
    }
}

/// Enqueues an operation UUID inside the acceptance transaction.
///
/// Created before the pipeline and bound afterwards. A [`Weak`] breaks the
/// `Outbox → handler → service → dispatch → Outbox` ownership cycle.
/// [`OutboxHandle`] owns the pipeline; dispatch fails after it is dropped.
#[derive(Debug)]
pub struct OutboxDispatch {
    outbox: OnceLock<Weak<Outbox>>,
}

impl Default for OutboxDispatch {
    fn default() -> Self {
        Self::new()
    }
}

impl OutboxDispatch {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            outbox: OnceLock::new(),
        }
    }

    /// Attach the started pipeline. Called once, by whoever started it.
    pub fn bind(&self, outbox: &Arc<Outbox>) {
        if self.outbox.set(Arc::downgrade(outbox)).is_err() {
            warn!("types_registry admission dispatch was bound twice; keeping the first pipeline");
        }
    }
}

#[async_trait::async_trait]
impl OperationDispatch for OutboxDispatch {
    async fn enqueue(&self, tx: &DbTx<'_>, operation_id: Uuid) -> anyhow::Result<()> {
        let outbox = self
            .outbox
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| anyhow::anyhow!("the admission outbox is not running"))?;
        outbox
            .enqueue(tx, QUEUE, 0, payload(operation_id), PAYLOAD_TYPE)
            .await?;
        Ok(())
    }
}

/// Leased handler that parses the operation UUID and maps the admission result.
pub struct AdmissionHandler {
    registry: Arc<RegistryService>,
    /// Delivery attempts allowed before an operation is abandoned.
    max_attempts: u32,
}

/// Elide `RegistryService`: its trait objects and `Db` do not implement `Debug`.
impl std::fmt::Debug for AdmissionHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionHandler")
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}

impl AdmissionHandler {
    #[must_use]
    pub const fn new(registry: Arc<RegistryService>, max_attempts: u32) -> Self {
        Self {
            registry,
            max_attempts,
        }
    }

    /// Decide one message: refuse a foreign envelope, else admit its payload with
    /// `lease` left before the strategy cancels the handler.
    ///
    /// The body [`LeasedHandler::handle`] runs per message, and public so tests can
    /// reach the envelope guard without a [`Batch`], which only the outbox builds.
    pub async fn handle_message(
        &self,
        msg: &OutboxMessage,
        lease: std::time::Duration,
    ) -> MessageResult {
        if msg.payload_type != PAYLOAD_TYPE {
            let cause = UnexpectedPayloadType {
                actual: &msg.payload_type,
                expected: PAYLOAD_TYPE,
            };
            return reject_unusable(self.registry.metrics(), &cause);
        }
        self.admit_payload_within(&msg.payload, msg.attempts, lease)
            .await
    }

    /// Admit a payload directly for tests, with the lease a full first delivery
    /// would have. [`LeasedHandler::handle`] checks the envelope type, supplies the
    /// message's `attempts` (retries so far, `0` on first delivery) and passes the
    /// lease actually left.
    pub async fn admit_payload(&self, payload: &[u8], attempts: i16) -> MessageResult {
        self.admit_payload_within(payload, attempts, self.registry.operation_timeout())
            .await
    }

    /// Admit a payload with `lease` left before the strategy cancels this handler.
    ///
    /// **NOT cancel-safe.** `tokio::time::timeout_at` drops this future on lease
    /// expiry, leaving the operation `running`, committed items terminal with
    /// their real outcomes, and others `pending`. Per-candidate transactions
    /// prevent partial entity writes; redelivery skips terminal items and resumes
    /// the rest, making an interrupted pass recoverable.
    async fn admit_payload_within(
        &self,
        payload: &[u8],
        attempts: i16,
        lease: std::time::Duration,
    ) -> MessageResult {
        let operation_id = match parse_payload(payload) {
            Ok(operation_id) => operation_id,
            // Permanent by construction: no redelivery changes the bytes.
            Err(e) => return reject_unusable(self.registry.metrics(), &e),
        };

        let now = time::OffsetDateTime::now_utc();
        // Fixed before the first await, so it is one instant for the whole delivery.
        // Deriving it later would restart the budget from whatever admission had
        // already spent, handing the abandonment that follows a failure more lease
        // than remains — and a write that runs past the real deadline is dropped and
        // returned as a retry, which is the outcome abandoning exists to avoid.
        let deadline = work_deadline(lease);

        // Budget exhausted: `max_attempts` deliveries already incremented `attempts`
        // without the handler reaching a decision, which is what a lease timeout
        // leaves behind. Skip `registry.admit()` — it timed out before and would
        // hold the lease again — and decide from the stored status instead.
        if u64::try_from(attempts).unwrap_or(u64::MAX) >= u64::from(self.max_attempts) {
            return self
                .decide_from_stored_status(operation_id, attempts, now, deadline)
                .await;
        }

        match self.registry.admit(operation_id, now).await {
            Ok(()) => MessageResult::Ok,
            Err(ServiceError::Worker(e)) if e.transient() && self.may_retry(attempts) => {
                self.retry(operation_id, attempts, &e)
            }
            Err(e) => {
                self.abandon(operation_id, attempts, &e, now, deadline)
                    .await
            }
        }
    }

    /// Decide a delivery whose budget is spent, reading the stored status instead
    /// of admitting again.
    ///
    /// Every arm is terminal for the message, and that is the whole point:
    /// `MessageResult::Retry` here would put the next delivery back into this same
    /// branch, so a status read that keeps failing would hold the single partition
    /// forever and drive `attempts` past the `i16` the outbox stores it in.
    ///
    /// Terminal also has to mean *returning*. `LeasedStrategy` runs the handler
    /// under `timeout_at` and converts a dropped future into `Retry`, so an await
    /// that outlives the lease reopens that loop however the arms are written.
    ///
    /// `deadline` covers the read and the abandonment that may follow it, rather
    /// than a budget each: this path can take both in sequence, and two budgets that
    /// are individually safe still add up to more lease than there is.
    async fn decide_from_stored_status(
        &self,
        operation_id: Uuid,
        attempts: i16,
        now: time::OffsetDateTime,
        deadline: tokio::time::Instant,
    ) -> MessageResult {
        let status = tokio::time::timeout_at(deadline, self.registry.operation(operation_id)).await;
        match status {
            Ok(Ok(Some(op))) if op.status == OperationStatus::Completed => {
                info!(
                    %operation_id,
                    attempts,
                    max_attempts = self.max_attempts,
                    "types_registry admission completed on a prior delivery; acking"
                );
                MessageResult::Ok
            }
            Ok(Ok(Some(_))) => {
                self.abandon(
                    operation_id,
                    attempts,
                    &"delivery budget exhausted",
                    now,
                    deadline,
                )
                .await
            }
            Ok(Ok(None)) => {
                let cause = format!(
                    "operation {operation_id} not found at budget exhaustion; \
                     the message cannot be associated with any known operation"
                );
                reject_unusable(self.registry.metrics(), &cause)
            }
            // No status to go on, so abandon on the assumption it is unfinished.
            // Safe either way: `abandon` fails only undecided items and
            // `mark_abandoned` moves only a `pending`/`running` row, so an operation
            // that did complete keeps its outcomes and loses nothing but a spurious
            // dead-letter row. If the write fails too, the operation stays
            // non-terminal and boot recovery re-enqueues it.
            Ok(Err(e)) => {
                self.abandon(
                    operation_id,
                    attempts,
                    &format!("delivery budget exhausted; status unreadable: {e}"),
                    now,
                    deadline,
                )
                .await
            }
            Err(_) => {
                self.abandon(
                    operation_id,
                    attempts,
                    &"delivery budget exhausted; the status read did not answer in time",
                    now,
                    deadline,
                )
                .await
            }
        }
    }

    /// Whether a further delivery is allowed. `attempts` counts retries already
    /// taken, so the delivery in hand is number `attempts + 1`.
    fn may_retry(&self, attempts: i16) -> bool {
        u64::try_from(attempts).unwrap_or(u64::MAX) + 1 < u64::from(self.max_attempts)
    }

    /// Retry an infrastructure failure that still has attempts left.
    fn retry(
        &self,
        operation_id: Uuid,
        attempts: i16,
        cause: &dyn std::fmt::Display,
    ) -> MessageResult {
        warn!(
            %operation_id,
            attempts,
            max_attempts = self.max_attempts,
            error = %cause,
            "types_registry admission failed transiently; the message will be redelivered"
        );
        self.registry
            .metrics()
            .admission_delivery(DeliveryOutcome::Retried);
        MessageResult::Retry
    }

    /// Dead-letter and terminalize so `GET /operations/{id}` shows abandonment
    /// and boot recovery does not re-enqueue the operation.
    async fn abandon(
        &self,
        operation_id: Uuid,
        attempts: i16,
        cause: &(dyn std::fmt::Display + Sync),
        now: time::OffsetDateTime,
        deadline: tokio::time::Instant,
    ) -> MessageResult {
        let reason = cause.to_string();
        error!(
            %operation_id,
            attempts,
            max_attempts = self.max_attempts,
            error = %reason,
            "types_registry abandoned an admission; the message is dead-lettered"
        );
        self.registry
            .metrics()
            .admission_delivery(DeliveryOutcome::DeadLettered);
        self.terminalize_abandoned(operation_id, now, deadline)
            .await;
        MessageResult::Reject(reason)
    }

    /// Terminalize an abandoned operation, best effort, stopping at `deadline`.
    ///
    /// The dead-letter stands either way — a failure here only means the operation
    /// stays non-terminal until boot recovery re-enqueues it. Bounded for the same
    /// reason the status read is: a write that outlives the lease is dropped by
    /// `timeout_at` and returns as `Retry`, undoing the dead-letter its caller
    /// exists to produce.
    async fn terminalize_abandoned(
        &self,
        operation_id: Uuid,
        now: time::OffsetDateTime,
        deadline: tokio::time::Instant,
    ) {
        let written =
            tokio::time::timeout_at(deadline, self.registry.abandon(operation_id, now)).await;
        let failure: &dyn std::fmt::Display = match &written {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e,
            Err(elapsed) => elapsed,
        };
        error!(
            %operation_id,
            error = %failure,
            "types_registry could not terminalize an abandoned operation; it stays \
             non-terminal until the next recovery scan"
        );
    }
}

/// When database work must stop, given the lease left when the delivery began.
///
/// A tenth is held back so the handler can return its result and the strategy can
/// act on it. Without that margin the work would run to the instant `timeout_at`
/// fires, and a result produced exactly then is a result nobody reads.
fn work_deadline(lease: std::time::Duration) -> tokio::time::Instant {
    tokio::time::Instant::now() + lease.mul_f32(0.9)
}

/// An unusable payload cannot be retried or identify an operation to terminalize.
///
/// Takes `metrics` because the counter must fire here too: an unusable message is a
/// dead letter like any other, and leaving it uncounted puts a blind spot in the
/// series an operator alerts on.
fn reject_unusable(metrics: &dyn AdmissionMetrics, cause: &dyn std::fmt::Display) -> MessageResult {
    error!(error = %cause, "types_registry received an unusable admission message");
    metrics.admission_delivery(DeliveryOutcome::DeadLettered);
    MessageResult::Reject(cause.to_string())
}

/// Drives the batch directly rather than through the [`LeasedMessageHandler`]
/// blanket impl, which is otherwise the right shape for `batch_size(1)`.
///
/// `Batch::remaining()` is the reason. It is the only account of how much lease is
/// actually left — the strategy starts its clock before acquiring the lease and
/// reading the batch, and never tells a per-message handler what that cost. Without
/// it the budget-exhausted path can only guess, and a guess that runs long is
/// dropped at the deadline and returned as a retry, which is the loop that path
/// exists to close.
///
/// The loop is otherwise the blanket impl's: one message at a time, `ack` or
/// `reject` per result, and stop starting new work once the lease is spent.
#[async_trait::async_trait]
impl LeasedHandler for AdmissionHandler {
    async fn handle(&self, batch: &mut Batch<'_>) -> HandlerResult {
        loop {
            // Read before taking the message: `next_msg` borrows the batch until the
            // message is done with, and `OutboxMessage` is not `Clone` to step around
            // that. The instant between the two costs nothing that the return margin
            // in `work_deadline` does not already cover.
            let lease = batch.remaining();
            let Some(msg) = batch.next_msg() else { break };

            let result = self.handle_message(msg, lease).await;

            match result {
                MessageResult::Ok => batch.ack(),
                MessageResult::Retry => {
                    return HandlerResult::Retry {
                        reason: "types_registry admission asked for redelivery".to_owned(),
                    };
                }
                MessageResult::Reject(reason) => batch.reject(reason),
            }

            // The message just handled finished inside its own budget; do not start
            // another one on a lease that is already spent.
            if batch.remaining().is_zero() {
                break;
            }
        }
        HandlerResult::Success
    }
}

/// Start and bind the pipeline with shared runtime/test settings.
/// `low_latency` avoids pacing bursts while consumers await admission in `init()`.
///
/// # Errors
/// [`StartError`] for outbox startup or for a failed recovery scan.
pub async fn start(
    db: Db,
    registry: &Arc<RegistryService>,
    dispatch: &Arc<OutboxDispatch>,
) -> Result<OutboxHandle, StartError> {
    let handle = Outbox::builder(db.clone())
        .table_prefix(TABLE_PREFIX)?
        .profile(OutboxProfile::low_latency())
        // One message per read batch. The outbox keeps `attempts` per partition and
        // hands the same value to every message in a batch, resetting it when the
        // lease is released after a partial success — so with a batch the delivery
        // budget below is shared and reset by unrelated messages. At size 1 it is
        // the budget of the message in hand. It is not free: ten messages now take ten
        // acquire/read/ack cycles where one batch did before, and on a serial
        // partition that latency adds to service time. Unmeasured, and accepted
        // because a shared budget is not a budget.
        .processor_tuning(WorkerTuning::processor_low_latency().batch_size(1))
        .queue(QUEUE, Partitions::of(PARTITIONS))
        .leased(AdmissionHandler::new(
            Arc::clone(registry),
            registry.max_delivery_attempts(),
        ))
        .lease(lease_config(registry.operation_timeout()))
        .start()
        .await?;
    dispatch.bind(handle.outbox());
    recover_nonterminal_operations(&db, registry, handle.outbox()).await?;
    Ok(handle)
}

/// Re-enqueue non-terminal operations at boot, including interrupted inline submissions.
/// Duplicate messages are safe because admission is idempotent.
///
/// Keyset paging by [`RECOVERY_PAGE`] advances even while prior rows remain non-terminal.
async fn recover_nonterminal_operations(
    db: &Db,
    registry: &Arc<RegistryService>,
    outbox: &Arc<Outbox>,
) -> Result<(), StartError> {
    let mut after: Option<RecoveryCursor> = None;
    let mut total = 0usize;
    loop {
        let page = registry
            .nonterminal_operation_page(after, RECOVERY_PAGE)
            .await
            .map_err(StartError::Recovery)?;
        if page.is_empty() {
            break;
        }
        after = page.last().copied();
        let short = u64::try_from(page.len()).unwrap_or(u64::MAX) < RECOVERY_PAGE;

        let messages: Vec<EnqueueMessage<'_>> = page
            .iter()
            .map(|cursor| EnqueueMessage {
                partition: 0,
                payload: payload(cursor.id),
                payload_type: PAYLOAD_TYPE,
            })
            .collect();
        total += messages.len();
        db.transaction_ref(|tx| {
            let outbox = Arc::clone(outbox);
            Box::pin(async move {
                outbox
                    .enqueue_batch(tx, QUEUE, &messages)
                    .await
                    .map_err(|error| DbError::Other(anyhow::Error::new(error)))?;
                Ok(())
            })
        })
        .await
        .map_err(StartError::RecoveryEnqueue)?;

        if short {
            break;
        }
    }
    if total > 0 {
        info!(
            count = total,
            "types_registry recovered nonterminal operations"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_budget_matches_the_configured_operation_timeout() {
        let config = lease_config(std::time::Duration::from_secs(300));

        assert_eq!(config.duration, std::time::Duration::from_secs(302));
        assert_eq!(
            config.duration.saturating_sub(config.headroom),
            std::time::Duration::from_secs(300)
        );
    }
}
