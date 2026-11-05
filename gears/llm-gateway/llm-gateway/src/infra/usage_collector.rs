//! Usage Reporter adapter: delivers the AI credit usage record of each
//! terminal request to Usage Collector (DESIGN §3.2 Usage Reporter,
//! `seq-usage-delivery`).
//!
//! [`UsageSink::record`] only enqueues into a bounded in-memory buffer and
//! never blocks or fails the consumer request. A background worker
//! ([`UsageCollectorReporter::run`]) batches the buffer into
//! `UsageCollectorClientV1::create_usage_records` calls under the gear's
//! service identity, retries transient failures with backoff, drops records
//! the collector rejects permanently (logged, no content). When the gear stops
//! it first waits (bounded) for the requests in flight and their settles, so
//! their usage is still enqueued, then drains the buffer within
//! `shutdown.usage_drain_reserve`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rand::RngExt as _;
use rust_decimal::Decimal;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use usage_collector_sdk::{
    CreateUsageRecord, IdempotencyKey, MetadataKey, ResourceRef, SubjectRef,
    UsageCollectorClientV1, UsageCollectorError, UsageRecord, UsageTypeGtsId,
};

use crate::config::{ShutdownConfig, UsageConfig};
use crate::domain::ports::{ServiceIdentity, UsageProducers, UsageReport, UsageSink};

/// `resource_type` of the usage record resource reference.
pub const RESOURCE_TYPE: &str = "llm_gateway.response";

/// Most response ids in one "undelivered at shutdown" log event.
const UNDELIVERED_LOG_CHUNK: usize = 100;

/// Closes the reporter when the worker ends, whatever the way: no `record`
/// is queued after it (it logs the record as undelivered instead), and the
/// queued records are logged.
struct CloseGuard<'a> {
    reporter: &'a UsageCollectorReporter,
}

impl Drop for CloseGuard<'_> {
    fn drop(&mut self) {
        let reporter = self.reporter;
        // `closed` is set under the buffer lock, so a concurrent `record`
        // either queued before (and is taken here) or sees `closed`.
        let (rest, lost) = {
            let mut buffer = reporter.buffer();
            reporter.closed.store(true, Ordering::Relaxed);
            let rest: Vec<Entry> = buffer.queue.drain(..).collect();
            // Non-zero only when the worker died mid-delivery: those records
            // were owned by the dead task and cannot be listed.
            (rest, std::mem::take(&mut buffer.in_flight))
        };
        if !rest.is_empty() {
            UsageCollectorReporter::log_undelivered(&rest);
        }
        if lost > 0 {
            tracing::error!(
                count = lost,
                "usage records lost: the usage reporter worker stopped while delivering them"
            );
        }
    }
}

/// A record waiting for delivery.
struct Entry {
    response_id: String,
    record: CreateUsageRecord,
    /// How many times the collector answered `Internal` for this record.
    internal_failures: u32,
}

/// The delivery buffer: queued records plus the count of those in the batch
/// being sent (both count against `buffer_capacity`).
#[derive(Default)]
struct Buffer {
    queue: VecDeque<Entry>,
    in_flight: usize,
}

/// How the collector answered for a record or a batch.
enum Verdict {
    /// Unavailable: retry without limit.
    Transient,
    /// Internal: retry up to `max_internal_retries`.
    Internal,
    /// Rejected for good: drop.
    Permanent,
}

fn classify(err: &UsageCollectorError) -> Verdict {
    if err.is_retryable() {
        Verdict::Transient
    } else if matches!(err, UsageCollectorError::Internal { .. }) {
        Verdict::Internal
    } else {
        Verdict::Permanent
    }
}

/// Outcome of one delivery attempt.
enum Attempt {
    /// No service context could be obtained.
    NoContext(anyhow::Error),
    /// The collector answered.
    Answer(Result<Vec<Result<UsageRecord, UsageCollectorError>>, UsageCollectorError>),
    /// The drain deadline passed while the call was in flight.
    DrainExpired,
}

/// Reporter handle: the [`UsageSink`] the Quota Guard hands usage to, plus
/// the worker entry point the gear lifecycle runs.
pub struct UsageCollectorReporter {
    client: Arc<dyn UsageCollectorClientV1>,
    identity: Arc<dyn ServiceIdentity>,
    usage_type: UsageTypeGtsId,
    config: UsageConfig,
    shutdown: ShutdownConfig,
    jitter: f64,
    buffer: Mutex<Buffer>,
    wake: Notify,
    dropped: AtomicU64,
    /// The worker has stopped: nothing will be delivered any more.
    closed: AtomicBool,
}

impl UsageCollectorReporter {
    /// Reporter over `client`, delivering under `identity`.
    ///
    /// # Errors
    ///
    /// `usage.usage_type` is not a valid usage type GTS id.
    pub fn new(
        client: Arc<dyn UsageCollectorClientV1>,
        identity: Arc<dyn ServiceIdentity>,
        usage: &UsageConfig,
        shutdown: &ShutdownConfig,
    ) -> Result<Self, UsageCollectorError> {
        Ok(Self {
            client,
            identity,
            usage_type: UsageTypeGtsId::new(usage.usage_type.clone())?,
            config: usage.clone(),
            shutdown: shutdown.clone(),
            jitter: 0.2,
            buffer: Mutex::new(Buffer::default()),
            wake: Notify::new(),
            dropped: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        })
    }

    /// Override the retry backoff jitter fraction (tests use 0).
    #[must_use]
    pub fn with_backoff_jitter(mut self, jitter: f64) -> Self {
        self.jitter = jitter;
        self
    }

    /// Records waiting for delivery (queued or in the batch being sent).
    #[must_use]
    pub fn pending(&self) -> usize {
        let buffer = self.buffer();
        buffer.queue.len() + buffer.in_flight
    }

    /// Records dropped so far because the buffer was full.
    #[must_use]
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn buffer(&self) -> std::sync::MutexGuard<'_, Buffer> {
        // A poisoned lock still holds a consistent queue: keep delivering.
        self.buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Worker: deliver until `cancel` fires. After it the worker keeps
    /// delivering (and `record` keeps accepting) while `producers` finish the
    /// requests still in flight and their settles, for at most
    /// [`ShutdownConfig::quiescence_budget`]; then the drain starts: whatever
    /// is queued is delivered within `shutdown.usage_drain_reserve`, the
    /// records left undelivered are logged and the reporter closes.
    pub async fn run(&self, cancel: CancellationToken, producers: Arc<dyn UsageProducers>) {
        // Closes the reporter (and logs what is left) however `run` ends,
        // including a panic or an abort of the worker task.
        let _close = CloseGuard { reporter: self };
        // Fires when the drain starts; the delivery code treats it as the
        // stop signal.
        let drain_start = CancellationToken::new();
        let worker = self.deliver_until(&drain_start);
        let quiesce = async {
            cancel.cancelled().await;
            let budget = self.shutdown.quiescence_budget();
            if tokio::time::timeout(budget, producers.quiescent())
                .await
                .is_err()
            {
                tracing::warn!(
                    budget_secs = budget.as_secs(),
                    "usage drain starts before the requests in flight finished: quiescence budget spent"
                );
            }
            drain_start.cancel();
        };
        tokio::join!(worker, quiesce);
    }

    /// The delivery loop; ends once `drain_start` fired and the drain is over.
    async fn deliver_until(&self, drain_start: &CancellationToken) {
        let mut drain: Option<Instant> = None;
        loop {
            let Some(batch) = self.next_batch(drain_start, &mut drain).await else {
                break;
            };
            let left = self.deliver(batch, drain_start, &mut drain).await;
            if !left.is_empty() {
                Self::log_undelivered(&left);
                self.finish(left.len());
                break;
            }
        }
    }

    /// Log records that will never be delivered (shutdown reserve spent), in
    /// chunks so one event never carries an unbounded id list.
    fn log_undelivered(entries: &[Entry]) {
        for chunk in entries.chunks(UNDELIVERED_LOG_CHUNK) {
            let ids: Vec<&str> = chunk.iter().map(|e| e.response_id.as_str()).collect();
            tracing::error!(
                total = entries.len(),
                count = ids.len(),
                response_ids = ?ids,
                "usage records undelivered at shutdown: the drain reserve ended"
            );
        }
    }

    /// `n` in-flight records are settled (delivered, dropped or abandoned).
    fn finish(&self, n: usize) {
        let mut buffer = self.buffer();
        buffer.in_flight = buffer.in_flight.saturating_sub(n);
    }

    /// Wait for the next batch: full, or partial once the oldest queued
    /// record has waited `flush_interval`; on cancel, whatever is queued.
    /// `None` once cancelled and the queue is empty.
    async fn next_batch(
        &self,
        cancel: &CancellationToken,
        drain: &mut Option<Instant>,
    ) -> Option<Vec<Entry>> {
        let mut flush_at: Option<Instant> = None;
        loop {
            if drain.is_none() && cancel.is_cancelled() {
                *drain = Some(Instant::now() + self.shutdown.usage_drain_reserve);
            }
            {
                let mut buffer = self.buffer();
                let len = buffer.queue.len();
                let due = flush_at.is_some_and(|at| Instant::now() >= at);
                if len >= self.config.batch_size || (len > 0 && (drain.is_some() || due)) {
                    let n = len.min(self.config.batch_size);
                    buffer.in_flight += n;
                    return Some(buffer.queue.drain(..n).collect());
                }
                if drain.is_some() {
                    return None;
                }
                if len == 0 {
                    flush_at = None;
                } else if flush_at.is_none() {
                    flush_at = Some(Instant::now() + self.config.flush_interval);
                }
            }
            tokio::select! {
                () = self.wake.notified() => {}
                () = sleep_until_opt(flush_at) => {}
                () = cancel.cancelled() => {}
            }
        }
    }

    /// Send `batch` until every record is delivered or dropped. Returns the
    /// records still undelivered when the drain deadline passed.
    async fn deliver(
        &self,
        mut batch: Vec<Entry>,
        cancel: &CancellationToken,
        drain: &mut Option<Instant>,
    ) -> Vec<Entry> {
        loop {
            if drain.is_some_and(|at| Instant::now() >= at) {
                return batch;
            }
            let before = batch.len();
            match self.attempt(&batch, cancel, drain).await {
                Attempt::DrainExpired => return batch,
                Attempt::NoContext(err) => {
                    tracing::warn!(
                        error = %err,
                        count = batch.len(),
                        "usage delivery postponed: no service identity context"
                    );
                }
                Attempt::Answer(Err(err)) => batch = self.batch_failed(batch, &err),
                Attempt::Answer(Ok(outcomes)) => batch = self.per_record(batch, outcomes),
            }
            self.finish(before - batch.len());
            if batch.is_empty() || !self.pause(cancel, drain).await {
                return batch;
            }
        }
    }

    /// One `create_usage_records` call under the service identity.
    async fn attempt(
        &self,
        batch: &[Entry],
        cancel: &CancellationToken,
        drain: &mut Option<Instant>,
    ) -> Attempt {
        let call = async {
            let identity = match self.identity.context().await {
                Ok(identity) => identity,
                Err(err) => return Attempt::NoContext(err),
            };
            let records = batch.iter().map(|e| e.record.clone()).collect();
            Attempt::Answer(
                self.client
                    .create_usage_records(&identity.ctx, records)
                    .await,
            )
        };
        tokio::pin!(call);
        if drain.is_none() {
            tokio::select! {
                attempt = &mut call => return attempt,
                () = cancel.cancelled() => {
                    *drain = Some(Instant::now() + self.shutdown.usage_drain_reserve);
                }
            }
        }
        match drain {
            Some(at) => tokio::time::timeout_at(*at, call)
                .await
                .unwrap_or(Attempt::DrainExpired),
            None => call.await,
        }
    }

    /// Wait the retry backoff; `false` when the drain deadline is reached.
    /// A cancel during the wait starts the drain and ends the wait.
    async fn pause(&self, cancel: &CancellationToken, drain: &mut Option<Instant>) -> bool {
        let delay = self.backoff();
        if drain.is_none() {
            tokio::select! {
                () = tokio::time::sleep(delay) => return true,
                () = cancel.cancelled() => {
                    *drain = Some(Instant::now() + self.shutdown.usage_drain_reserve);
                    return true;
                }
            }
        }
        let now = Instant::now();
        let wake = drain.map_or(now + delay, |at| at.min(now + delay));
        tokio::time::sleep_until(wake).await;
        drain.is_none_or(|at| Instant::now() < at)
    }

    /// The retry backoff with its jitter applied.
    fn backoff(&self) -> Duration {
        let base = self.config.retry_backoff;
        if self.jitter <= 0.0 {
            return base;
        }
        let factor = rand::rng().random_range((1.0 - self.jitter)..=(1.0 + self.jitter));
        base.mul_f64(factor)
    }

    /// The whole call failed: the records to resend, if any.
    fn batch_failed(&self, batch: Vec<Entry>, err: &UsageCollectorError) -> Vec<Entry> {
        if matches!(classify(err), Verdict::Permanent) {
            let ids: Vec<&str> = batch.iter().map(|e| e.response_id.as_str()).collect();
            tracing::error!(
                error = %err,
                count = ids.len(),
                response_ids = ?ids,
                "usage records dropped: the collector rejected the batch"
            );
            return Vec::new();
        }
        tracing::warn!(error = %err, count = batch.len(), "usage delivery failed, retrying");
        batch
            .into_iter()
            .filter_map(|entry| self.resend(entry, err))
            .collect()
    }

    /// Per-record outcomes: the records to resend (retryable ones, in order).
    fn per_record(
        &self,
        batch: Vec<Entry>,
        outcomes: Vec<Result<UsageRecord, UsageCollectorError>>,
    ) -> Vec<Entry> {
        let mut outcomes = outcomes.into_iter();
        let mut retry = Vec::new();
        for entry in batch {
            let failure = match outcomes.next() {
                Some(Ok(_)) => continue,
                Some(Err(err)) => err,
                None => UsageCollectorError::internal("no outcome for the record"),
            };
            retry.extend(self.resend(entry, &failure));
        }
        retry
    }

    /// `entry` failed with `err`: the entry to resend, or `None` when it is
    /// dropped (rejected for good, or `Internal` retries exhausted; logged).
    fn resend(&self, entry: Entry, err: &UsageCollectorError) -> Option<Entry> {
        match classify(err) {
            Verdict::Transient => Some(entry),
            Verdict::Internal => self.after_internal(entry, err),
            Verdict::Permanent => {
                tracing::error!(
                    response_id = %entry.response_id,
                    error = %err,
                    "usage record dropped: the collector rejected it"
                );
                None
            }
        }
    }

    /// Count an `Internal` answer: the entry to resend, or `None` (logged)
    /// once `max_internal_retries` is exhausted.
    fn after_internal(&self, mut entry: Entry, err: &UsageCollectorError) -> Option<Entry> {
        entry.internal_failures += 1;
        if entry.internal_failures > self.config.max_internal_retries {
            tracing::error!(
                response_id = %entry.response_id,
                error = %err,
                attempts = entry.internal_failures,
                "usage record dropped: the collector kept answering internal errors"
            );
            None
        } else {
            Some(entry)
        }
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Map `report` to the Usage Collector submission (DESIGN `UsageRecord`):
/// usage type, explicit tenant, resource reference
/// (`llm_gateway.response`, response id), user subject, micro-credit value,
/// the response id as idempotency key, `created_at` = `terminal_at`, and the
/// metadata keys `model`, `provider_type`, `provider_request_id` (omitted
/// when absent), `complete`, `estimate`.
///
/// # Errors
///
/// A field the collector's validating newtypes reject (empty response id).
pub fn build_record(
    report: &UsageReport,
    usage_type: &UsageTypeGtsId,
) -> Result<CreateUsageRecord, UsageCollectorError> {
    let mut metadata = BTreeMap::new();
    let mut put = |key: &str, value: String| -> Result<(), UsageCollectorError> {
        metadata.insert(MetadataKey::new(key)?, value);
        Ok(())
    };
    put("model", report.model.clone())?;
    put("provider_type", report.provider_type.clone())?;
    if let Some(id) = &report.provider_request_id {
        put("provider_request_id", id.clone())?;
    }
    put("complete", report.usage.complete.to_string())?;
    put("estimate", report.usage.estimated.to_string())?;
    Ok(CreateUsageRecord {
        gts_id: usage_type.clone(),
        tenant_id: report.tenant_id,
        resource_ref: ResourceRef::new(report.response_id.clone(), RESOURCE_TYPE)?,
        subject_ref: Some(SubjectRef::new(report.user_id.to_string(), None::<String>)?),
        metadata,
        value: Decimal::from(report.quantity_micro_credits),
        idempotency_key: IdempotencyKey::new(report.response_id.clone())?,
        corrects_id: None,
        created_at: time::OffsetDateTime::from(report.terminal_at),
    })
}

#[async_trait]
impl UsageSink for UsageCollectorReporter {
    async fn record(&self, report: UsageReport) {
        let record = match build_record(&report, &self.usage_type) {
            Ok(record) => record,
            Err(err) => {
                tracing::error!(
                    response_id = %report.response_id,
                    error = %err,
                    "usage record dropped: it cannot be mapped to a collector record"
                );
                return;
            }
        };
        {
            let mut buffer = self.buffer();
            if self.closed.load(Ordering::Relaxed) {
                drop(buffer);
                tracing::error!(
                    response_id = %report.response_id,
                    "usage record undelivered: the usage reporter has stopped"
                );
                return;
            }
            if buffer.queue.len() + buffer.in_flight >= self.config.buffer_capacity {
                drop(buffer);
                self.dropped.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    response_id = %report.response_id,
                    "usage record dropped: the delivery buffer is full"
                );
                return;
            }
            buffer.queue.push_back(Entry {
                response_id: report.response_id,
                record,
                internal_failures: 0,
            });
        }
        self.wake.notify_one();
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "usage_collector_tests.rs"]
mod usage_collector_tests;
