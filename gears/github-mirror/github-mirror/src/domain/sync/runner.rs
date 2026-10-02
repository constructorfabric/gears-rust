//! `RepoPhaseRunner` — drives one repository's sync through its phases.
//!
//! A cut-down port of the reference implementation's `RepoPhaseRunner`: the
//! runner knows nothing about *what* a phase does — that is the registered
//! [`Worker`]s' concern — it only enforces phase ordering, bounds concurrency,
//! and tallies outcomes. Discovery runs alone; Indexing and Refinement drain
//! together so an entity can be refined while the rest of its family is still
//! being listed; Verification runs strictly last (once it exists, #4632
//! slice 6 step 5).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::queue::TaskQueue;
use super::task::{ExtractionTask, Lane, NewTask, RunIdentity, TaskKind, TaskPhase, TaskPriority};
use super::worker::{Worker, WorkerContext, WorkerDispatcher};
use crate::domain::error::DomainError;

/// Pending tasks above which the runner stops claiming until in-flight work
/// drains: bounds memory growth when Indexing seeds faster than Refinement
/// consumes (reference `DESIGN_ALGORITHMS` §8 hysteresis, high side only).
const BACKPRESSURE_HIGH: u64 = 10_000;

/// The phases that drain together: Indexing seeds Refinement as pages arrive.
const STREAMED_PHASES: [TaskPhase; 3] = [
    TaskPhase::Indexing,
    TaskPhase::ChangeDetection,
    TaskPhase::Refinement,
];

const VERIFICATION_PHASES: [TaskPhase; 1] = [TaskPhase::Verification];

const TRANSIENT_RETRIES: u32 = 3;
const TRANSIENT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Progress bands in permille, so the whole estimate stays integer.
///
/// DESIGN gives Discovery 2, Indexing 10, `ChangeDetection` 3, Refinement 80
/// and Verification 5. `ChangeDetection` never gets a task of its own here:
/// the change gate runs inline while a listing page is being indexed, so its
/// three points join the listing band instead of sitting in a band that can
/// never move.
const DISCOVERY_SPAN: u64 = 20;
const LISTING_BASE: u64 = DISCOVERY_SPAN;
const LISTING_SPAN: u64 = 130;
const REFINE_BASE: u64 = LISTING_BASE + LISTING_SPAN;
const REFINE_SPAN: u64 = 800;
const VERIFY_BASE: u64 = REFINE_BASE + REFINE_SPAN;
const VERIFY_SPAN: u64 = 50;

/// The finished fraction of `total` mapped onto `span` permille, where
/// `remaining` is what is still pending or running.
fn ramp(span: u64, total: u64, remaining: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    span.saturating_mul(total.saturating_sub(remaining))
        .div_euclid(total)
}

/// How one run went, in tasks.
#[derive(Debug, Default)]
pub struct RunReport {
    pub tasks_done: u64,
    pub failures: Vec<TaskFailure>,
    pub cancelled: bool,
}

impl RunReport {
    #[must_use]
    pub fn tasks_failed(&self) -> u64 {
        u64::try_from(self.failures.len()).unwrap_or(u64::MAX)
    }
}

/// One task that did not finish, with the error it stopped on.
#[derive(Debug)]
pub struct TaskFailure {
    pub kind: Option<TaskKind>,
    pub entity_id: Option<String>,
    pub error: DomainError,
}

impl TaskFailure {
    fn label(&self) -> String {
        let mut label = self
            .kind
            .map_or_else(|| "task".to_owned(), |kind| kind.to_string());
        if let Some(id) = &self.entity_id {
            label.push(' ');
            label.push_str(id);
        }
        label
    }

    #[must_use]
    pub fn public_text(&self) -> String {
        format!("{}: {}", self.label(), self.error.public_text())
    }
}

impl std::fmt::Display for TaskFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.label(), self.error)
    }
}

struct TaskOutcome {
    task: ExtractionTask,
    error: Option<DomainError>,
}

/// Sequential phase driver for a single repository sync.
pub struct RepoPhaseRunner {
    queue: Arc<TaskQueue>,
    dispatcher: Arc<WorkerDispatcher>,
    run: RunIdentity,
    max_concurrent_tasks: usize,
    cancel: CancellationToken,
    progress: Arc<AtomicU8>,
}

impl RepoPhaseRunner {
    #[must_use]
    pub fn new(
        workers: Vec<Arc<dyn Worker>>,
        run: RunIdentity,
        max_concurrent_tasks: std::num::NonZeroUsize,
        cancel: CancellationToken,
        progress: Arc<AtomicU8>,
    ) -> Self {
        let mut dispatcher = WorkerDispatcher::new();
        for worker in workers {
            dispatcher.register(worker);
        }
        Self {
            queue: Arc::new(TaskQueue::new()),
            dispatcher: Arc::new(dispatcher),
            run,
            max_concurrent_tasks: max_concurrent_tasks.get(),
            cancel,
            progress,
        }
    }

    /// Work out the current percentage and publish it, keeping the published
    /// value monotonically non-decreasing (PRD `cpt-cf-github-mirror-fr-progress`).
    fn publish_progress(&self) {
        let percent = u8::try_from(self.estimate_permille().div_euclid(10).min(100)).unwrap_or(100);
        self.progress.fetch_max(percent, Ordering::Relaxed);
    }

    /// Where the run is, in permille, from the live queue counts.
    ///
    /// A band is only credited once its own count is final, so the estimate
    /// never over-reports:
    ///
    /// - Discovery `[0, 20]` ramps on the one discovery task.
    /// - Listing `[20, 150]` covers Indexing and `ChangeDetection` together.
    ///   While it drains, Refinement is still being seeded page by page, so its
    ///   total is not yet known and nothing above the listing band is credited.
    /// - Refinement `[150, 950]` starts only once listing has drained and the
    ///   seeded total is final. That total is exact for a first sync and for an
    ///   incremental one, where the change gate seeds only what changed.
    /// - Verification `[950, 1000]` follows.
    ///
    /// A phase with no task in this run takes no share: a repeat sync of an
    /// unchanged repository seeds nothing to refine, so the estimate steps from
    /// the listing band to the verification band the moment listing drains,
    /// which is the first instant that emptiness can be known.
    fn estimate_permille(&self) -> u64 {
        let phase_counts = |phase| {
            (
                self.queue.count_for_phase(self.run.session_id, phase),
                self.queue
                    .remaining_count_for_phase(self.run.session_id, phase),
            )
        };

        let (discovery_total, discovery_remaining) = phase_counts(TaskPhase::Discovery);
        if discovery_total == 0 || discovery_remaining > 0 {
            return ramp(DISCOVERY_SPAN, discovery_total, discovery_remaining);
        }

        let (indexing_total, indexing_remaining) = phase_counts(TaskPhase::Indexing);
        let (gate_total, gate_remaining) = phase_counts(TaskPhase::ChangeDetection);
        let listing_total = indexing_total + gate_total;
        let listing_remaining = indexing_remaining + gate_remaining;
        if listing_total == 0 || listing_remaining > 0 {
            return LISTING_BASE + ramp(LISTING_SPAN, listing_total, listing_remaining);
        }

        let (refine_total, refine_remaining) = phase_counts(TaskPhase::Refinement);
        if refine_remaining > 0 {
            return REFINE_BASE + ramp(REFINE_SPAN, refine_total, refine_remaining);
        }

        let (verify_total, verify_remaining) = phase_counts(TaskPhase::Verification);
        VERIFY_BASE + ramp(VERIFY_SPAN, verify_total, verify_remaining)
    }

    /// Seed the Discovery task and drain every phase in order.
    pub async fn run(&self) -> RunReport {
        self.queue.enqueue_task(&NewTask {
            run: self.run,
            kind: TaskKind::Discover,
            entity_id: None,
            priority: TaskPriority::NORMAL,
            attempt: 0,
        });

        let mut report = RunReport::default();
        for phases in [
            &[TaskPhase::Discovery][..],
            &STREAMED_PHASES[..],
            &VERIFICATION_PHASES[..],
        ] {
            self.drain(phases, &mut report).await;
            // Discovery is the one task everything else hangs off: without
            // the repository row there is nothing to index.
            if report.cancelled || (phases.len() == 1 && !report.failures.is_empty()) {
                break;
            }
        }
        report
    }

    /// Claim and dispatch tasks of `phases` until none remain and nothing is
    /// in flight, at most `max_concurrent_tasks` at a time.
    async fn drain(&self, phases: &[TaskPhase], report: &mut RunReport) {
        let ctx = WorkerContext {
            queue: Arc::clone(&self.queue),
            cancel: self.cancel.clone(),
        };
        let mut in_flight: JoinSet<TaskOutcome> = JoinSet::new();
        // What each spawned task is working on. A task that panics or is
        // aborted comes back as a bare `JoinError`, and this is the only way
        // left to say which task that was.
        let mut running: HashMap<tokio::task::Id, ExtractionTask> = HashMap::new();
        let mut next_lane = 0usize;

        loop {
            if self.cancel.is_cancelled() {
                report.cancelled = true;
                break;
            }
            while let Some(outcome) = in_flight.try_join_next_with_id() {
                self.account(outcome, &mut running, report);
            }
            self.publish_progress();

            let saturated = in_flight.len() >= self.max_concurrent_tasks
                || (self.queue.pending_count(self.run.session_id) >= BACKPRESSURE_HIGH
                    && !in_flight.is_empty());
            if saturated {
                if let Some(outcome) = in_flight.join_next_with_id().await {
                    self.account(outcome, &mut running, report);
                }
                continue;
            }

            match self.claim_round_robin(phases, &mut next_lane) {
                Some(task) => self.spawn(task, &ctx, &mut in_flight, &mut running),
                // Nothing claimable right now: an in-flight Indexing task may
                // still seed more, so wait for one to finish before deciding
                // the phase is drained.
                None => match in_flight.join_next_with_id().await {
                    Some(outcome) => self.account(outcome, &mut running, report),
                    None => break,
                },
            }
        }

        while let Some(outcome) = in_flight.join_next_with_id().await {
            self.account(outcome, &mut running, report);
        }
        self.publish_progress();
    }

    /// Take one task, starting from the lane after the last one served, so a
    /// long pull-request queue cannot starve issues.
    fn claim_round_robin(
        &self,
        phases: &[TaskPhase],
        next_lane: &mut usize,
    ) -> Option<ExtractionTask> {
        for step in 0..Lane::ALL.len() {
            let lane = Lane::ALL[(*next_lane + step) % Lane::ALL.len()];
            if let Some(task) =
                self.queue
                    .claim_next_task_in_lane(self.run.session_id, phases, lane)
            {
                *next_lane = (*next_lane + step + 1) % Lane::ALL.len();
                return Some(task);
            }
        }
        None
    }

    fn spawn(
        &self,
        task: ExtractionTask,
        ctx: &WorkerContext,
        in_flight: &mut JoinSet<TaskOutcome>,
        running: &mut HashMap<tokio::task::Id, ExtractionTask>,
    ) {
        let queue = Arc::clone(&self.queue);
        let dispatcher = Arc::clone(&self.dispatcher);
        let ctx = ctx.clone();
        let spawned = task.clone();
        let handle = in_flight.spawn(async move {
            let mut task = task;
            let error = loop {
                match dispatcher.dispatch(&ctx, &task).await {
                    Ok(()) => {
                        queue.complete_task(task.id);
                        break None;
                    }
                    Err(e)
                        if e.is_transient()
                            && task.retries < TRANSIENT_RETRIES
                            && !ctx.cancel.is_cancelled() =>
                    {
                        task.retries += 1;
                        tracing::warn!(
                            kind = %task.kind,
                            entity_id = ?task.entity_id,
                            retry = task.retries,
                            error = %e,
                            "sync task hit a transient database error; retrying"
                        );
                        tokio::time::sleep(TRANSIENT_RETRY_DELAY * task.retries).await;
                    }
                    Err(e) => {
                        queue.fail_task(task.id);
                        break Some(e);
                    }
                }
            };
            TaskOutcome { task, error }
        });
        running.insert(handle.id(), spawned);
    }

    /// Record one finished task, and take it out of the queue if it did not
    /// take itself out.
    ///
    /// A task that panicked or was aborted never reached its own bookkeeping,
    /// so `running` says which task it was and the queue entry is failed here
    /// instead of sitting as `Running` for the rest of the run.
    fn account(
        &self,
        joined: Result<(tokio::task::Id, TaskOutcome), tokio::task::JoinError>,
        running: &mut HashMap<tokio::task::Id, ExtractionTask>,
        report: &mut RunReport,
    ) {
        let failure = match joined {
            Ok((id, outcome)) => {
                running.remove(&id);
                match outcome {
                    TaskOutcome { error: None, .. } => {
                        report.tasks_done += 1;
                        return;
                    }
                    TaskOutcome {
                        task,
                        error: Some(error),
                    } => TaskFailure {
                        kind: Some(task.kind),
                        entity_id: task.entity_id,
                        error,
                    },
                }
            }
            Err(join_error) => {
                let task = running.remove(&join_error.id());
                if let Some(task) = &task {
                    self.queue.fail_task(task.id);
                }
                TaskFailure {
                    kind: task.as_ref().map(|task| task.kind),
                    entity_id: task.and_then(|task| task.entity_id),
                    error: DomainError::internal(format!(
                        "sync task did not finish cleanly: {join_error}"
                    )),
                }
            }
        };
        tracing::warn!(%failure, "sync task failed");
        report.failures.push(failure);
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod runner_tests;
