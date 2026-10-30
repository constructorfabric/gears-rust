//! One measured run against a fresh topic: writers, consumers, then the
//! check.

use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{Backend, BackendError, Flush};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::args::Settings;
use crate::check::{Drain, Verdict, judge, settle_heads};
use crate::consume::{self, ReadPlan, ReadTally, Samples};
use crate::matrix::ReadMode;
use crate::produce::{Pacing, Until, WriteContext, WritePlan, WriteTally, write};
use crate::workload::{Clock, KeyPools, PARTITIONS, provision_topic};

/// The longest a B3 prefill may run.
const PREFILL_DEADLINE: Duration = Duration::from_mins(10);

/// How long consumers may take to reach the heads once writers stop.
const DRAIN_DEADLINE: Duration = Duration::from_mins(2);

/// Readers of a B1 sweep: as many as partitions, the largest read batch.
const SWEEP: ReadPlan = ReadPlan {
    consumers: PARTITIONS as usize,
    max_events: 1000,
    mode: ReadMode::Sync,
    samples: Samples::ReadCalls,
};

/// Who reads while the writers write.
#[derive(Debug, Clone)]
pub enum Readers {
    /// Nobody; once the writers stop, a sweep reads everything back for the
    /// check.
    AfterwardsOnly,
    /// These consumers, from the start.
    Follow(ReadPlan),
}

/// What one pipeline phase runs.
#[derive(Debug, Clone)]
pub struct PipelinePlan {
    /// Names the phase's topic.
    pub label: &'static str,
    /// How many writers.
    pub writers: usize,
    /// Events each append carries.
    pub events_per_append: usize,
    /// Whether appends wait for their events to be readable.
    pub flush: Flush,
    /// Closed or open loop.
    pub pacing: Pacing,
    /// Who reads while the writers write.
    pub readers: Readers,
    /// Unmeasured time before the measured window.
    pub warmup: Duration,
    /// Length of the measured window.
    pub duration: Duration,
}

impl PipelinePlan {
    /// The most writers and readers calling the backend at once: the writers
    /// with their consumers, or the check's sweep after the writers alone.
    #[must_use]
    pub fn callers(&self) -> usize {
        match &self.readers {
            Readers::Follow(read) => self.writers + read.consumers,
            Readers::AfterwardsOnly => self.writers.max(SWEEP.consumers),
        }
    }
}

/// What one phase measured.
#[derive(Debug)]
pub struct PhaseOutcome {
    /// What the writers did.
    pub write: WriteTally,
    /// What the consumers did and saw.
    pub read: ReadTally,
    /// What the rates are taken over.
    pub measured: Duration,
    /// Whether `--max-events-per-phase` ended the phase before its duration.
    pub capped: bool,
    /// The phase's correctness check.
    pub verdict: Verdict,
}

fn context(
    backend: &Arc<dyn Backend>,
    settings: &Settings,
    topic: &event_broker_sdk::GtsInstanceId,
    clock: Clock,
) -> WriteContext {
    WriteContext {
        backend: Arc::clone(backend),
        topic: topic.clone(),
        clock,
        padding: Arc::from("x".repeat(settings.payload_bytes)),
        pools: Arc::new(KeyPools::new(settings.keys)),
    }
}

/// Writers for `plan.warmup + plan.duration` or until they have appended
/// `settings.max_events_per_phase`, consumers alongside them, then the check.
///
/// # Errors
/// What provisioning the topic fails with.
pub async fn run_pipeline(
    backend: &Arc<dyn Backend>,
    settings: &Settings,
    plan: PipelinePlan,
) -> Result<PhaseOutcome, BackendError> {
    let spec = provision_topic(backend.as_ref(), plan.label).await?;
    let clock = Clock::starting(plan.warmup, plan.duration);
    if let Readers::Follow(ReadPlan {
        samples: Samples::PerSecond(windows),
        ..
    }) = &plan.readers
    {
        windows.start(clock.from());
    }
    let (targets_tx, targets) = watch::channel(None);
    let cancel = CancellationToken::new();
    let followers = match &plan.readers {
        Readers::Follow(read) => {
            consume::spawn(backend, &spec.topic, &clock, read, &targets, &cancel)
        }
        Readers::AfterwardsOnly => Vec::new(),
    };
    let write = write(
        &context(backend, settings, &spec.topic, clock.clone()),
        &WritePlan {
            writers: plan.writers,
            events_per_append: plan.events_per_append,
            flush: plan.flush,
            pacing: plan.pacing,
            until: Until::new(clock.to, settings.max_events_per_phase),
        },
    )
    .await;
    let heads = settle_heads(backend.as_ref(), &spec.topic, &write).await;
    targets_tx.send_replace(Some(Arc::new(heads)));
    let readers = match plan.readers {
        Readers::Follow(_) => followers,
        Readers::AfterwardsOnly => {
            consume::spawn(backend, &spec.topic, &clock, &SWEEP, &targets, &cancel)
        }
    };
    let read = consume::join(readers, Instant::now() + DRAIN_DEADLINE, &cancel).await;
    let verdict = judge(&write, &read, Drain::Full);
    Ok(PhaseOutcome {
        write,
        read,
        measured: clock.measured(),
        capped: clock.capped(),
        verdict,
    })
}

/// Prefills `events` (at most `settings.max_events_per_phase`) with 16
/// batching writers, then drains the topic from the start with `read` for at
/// most `settings.duration`.
///
/// # Errors
/// What provisioning the topic fails with.
pub async fn run_catchup(
    backend: &Arc<dyn Backend>,
    settings: &Settings,
    events: u64,
    read: ReadPlan,
) -> Result<PhaseOutcome, BackendError> {
    let spec = provision_topic(backend.as_ref(), "catchup").await?;
    let prefill_clock = Clock::starting(Duration::ZERO, PREFILL_DEADLINE);
    let write = write(
        &context(backend, settings, &spec.topic, prefill_clock.clone()),
        &WritePlan {
            writers: PARTITIONS as usize,
            events_per_append: 100,
            flush: Flush::Sync,
            pacing: Pacing::Closed,
            until: Until::new(prefill_clock.to, events.min(settings.max_events_per_phase)),
        },
    )
    .await;
    let heads = settle_heads(backend.as_ref(), &spec.topic, &write).await;
    let (_targets_tx, targets) = watch::channel(Some(Arc::new(heads)));
    let cancel = CancellationToken::new();
    let clock = Clock::starting(Duration::ZERO, settings.duration);
    let readers = consume::spawn(backend, &spec.topic, &clock, &read, &targets, &cancel);
    let read = consume::join(readers, clock.to, &cancel).await;
    let (drain, measured) = match read.drained_at {
        Some(at) => (Drain::Full, at.saturating_duration_since(clock.from())),
        None => (Drain::Partial, clock.measured()),
    };
    let verdict = judge(&write, &read, drain);
    Ok(PhaseOutcome {
        write,
        read,
        measured,
        capped: prefill_clock.capped(),
        verdict,
    })
}
