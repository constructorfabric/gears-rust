use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    BACKPRESSURE_HIGH, DISCOVERY_SPAN, LISTING_BASE, LISTING_SPAN, REFINE_BASE, REFINE_SPAN,
    RepoPhaseRunner, TRANSIENT_RETRIES, TRANSIENT_RETRY_DELAY, VERIFY_BASE, ramp,
};
use crate::domain::error::DomainError;
use crate::domain::sync::task::{
    Entity, ExtractionTask, Family, NewTask, RunIdentity, TaskKind, TaskPhase, TaskPriority,
};
use crate::domain::sync::worker::{Worker, WorkerContext};

fn runner() -> RepoPhaseRunner {
    RepoPhaseRunner::new(
        Vec::new(),
        RunIdentity {
            session_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
        },
        NonZeroUsize::MIN,
        CancellationToken::new(),
        Arc::new(AtomicU8::new(0)),
    )
}

fn seed(runner: &RepoPhaseRunner, kind: TaskKind, entity_id: &str) {
    runner.queue.enqueue_task(&NewTask {
        run: runner.run,
        kind,
        entity_id: Some(entity_id.to_owned()),
        priority: TaskPriority::NORMAL,
        attempt: 0,
    });
}

fn finish_one(runner: &RepoPhaseRunner, phase: TaskPhase) {
    let task = runner
        .queue
        .claim_next_task_in(runner.run.session_id, &[phase])
        .expect("a task of that phase must be pending");
    runner.queue.complete_task(task.id);
}

#[test]
fn ramp_credits_the_finished_share_and_nothing_for_an_empty_phase() {
    assert_eq!(ramp(REFINE_SPAN, 0, 0), 0);
    assert_eq!(ramp(REFINE_SPAN, 4, 4), 0);
    assert_eq!(ramp(REFINE_SPAN, 4, 1), 600);
    assert_eq!(ramp(REFINE_SPAN, 4, 0), REFINE_SPAN);
}

#[test]
fn the_estimate_walks_the_bands_in_order_and_never_goes_back() {
    let runner = runner();
    let mut seen = vec![runner.estimate_permille()];
    let mut expect = |value: u64| {
        seen.push(runner.estimate_permille());
        assert_eq!(*seen.last().unwrap(), value, "after step {}", seen.len());
    };

    seed(&runner, TaskKind::Discover, "acme/widget");
    expect(0);
    finish_one(&runner, TaskPhase::Discovery);
    expect(DISCOVERY_SPAN);

    seed(&runner, TaskKind::Index(Family::Issues), "issues");
    seed(&runner, TaskKind::Index(Family::PullRequests), "pulls");
    expect(LISTING_BASE);
    finish_one(&runner, TaskPhase::Indexing);
    for number in 1..=4 {
        seed(
            &runner,
            TaskKind::Refine(Entity::Issue),
            &number.to_string(),
        );
    }
    expect(LISTING_BASE + LISTING_SPAN.div_euclid(2));
    finish_one(&runner, TaskPhase::Indexing);
    expect(REFINE_BASE);

    finish_one(&runner, TaskPhase::Refinement);
    expect(REFINE_BASE + REFINE_SPAN.div_euclid(4));
    for _ in 0..3 {
        finish_one(&runner, TaskPhase::Refinement);
    }
    expect(VERIFY_BASE);

    seed(&runner, TaskKind::Verify(Entity::Issue), "1");
    expect(VERIFY_BASE);
    finish_one(&runner, TaskPhase::Verification);
    expect(1000);

    assert!(
        seen.windows(2).all(|pair| pair[0] <= pair[1]),
        "the estimate must never go back: {seen:?}"
    );
}

fn locked() -> DomainError {
    DomainError::Database(toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(
        "database is locked (code: 5)".to_owned(),
    )))
}

struct FlakyDiscovery {
    transient_failures: u32,
    then_permanent: bool,
    cancel_on_first_call: Option<CancellationToken>,
    attempts: Mutex<Vec<u32>>,
}

impl FlakyDiscovery {
    fn new(transient_failures: u32) -> Arc<Self> {
        Arc::new(Self {
            transient_failures,
            then_permanent: false,
            cancel_on_first_call: None,
            attempts: Mutex::new(Vec::new()),
        })
    }

    fn attempts(&self) -> Vec<u32> {
        self.attempts.lock().unwrap().clone()
    }
}

#[async_trait]
impl Worker for FlakyDiscovery {
    fn handles(&self, kind: TaskKind) -> bool {
        kind == TaskKind::Discover
    }

    async fn execute(
        &self,
        _ctx: &WorkerContext,
        task: &ExtractionTask,
    ) -> Result<(), DomainError> {
        self.attempts.lock().unwrap().push(task.retries);
        if let Some(cancel) = &self.cancel_on_first_call {
            cancel.cancel();
        }
        if task.retries < self.transient_failures {
            return Err(locked());
        }
        if self.then_permanent {
            return Err(DomainError::internal("GitHub answered 500"));
        }
        Ok(())
    }
}

fn runner_with(worker: Arc<FlakyDiscovery>, cancel: CancellationToken) -> RepoPhaseRunner {
    RepoPhaseRunner::new(
        vec![worker],
        RunIdentity {
            session_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
        },
        NonZeroUsize::MIN,
        cancel,
        Arc::new(AtomicU8::new(0)),
    )
}

#[tokio::test(start_paused = true)]
async fn a_transient_error_is_retried_with_a_growing_delay_until_it_succeeds() {
    let worker = FlakyDiscovery::new(2);
    let started = tokio::time::Instant::now();

    let report = runner_with(Arc::clone(&worker), CancellationToken::new())
        .run()
        .await;

    assert_eq!(worker.attempts(), [0, 1, 2]);
    assert_eq!(report.tasks_done, 1);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(
        started.elapsed(),
        TRANSIENT_RETRY_DELAY + TRANSIENT_RETRY_DELAY * 2,
        "the wait grows with the attempt number"
    );
}

#[tokio::test(start_paused = true)]
async fn retries_stop_at_the_bound_and_the_task_fails() {
    let worker = FlakyDiscovery::new(u32::MAX);
    let started = tokio::time::Instant::now();

    let report = runner_with(Arc::clone(&worker), CancellationToken::new())
        .run()
        .await;

    let expected_attempts: Vec<u32> = (0..=TRANSIENT_RETRIES).collect();
    assert_eq!(worker.attempts(), expected_attempts);
    assert_eq!(report.tasks_done, 0);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].kind, Some(TaskKind::Discover));
    assert!(report.failures[0].error.is_transient());
    let waited: Duration = (1..=TRANSIENT_RETRIES)
        .map(|attempt| TRANSIENT_RETRY_DELAY * attempt)
        .sum();
    assert_eq!(started.elapsed(), waited);
}

#[tokio::test(start_paused = true)]
async fn a_permanent_error_is_not_retried() {
    let worker = Arc::new(FlakyDiscovery {
        transient_failures: 0,
        then_permanent: true,
        cancel_on_first_call: None,
        attempts: Mutex::new(Vec::new()),
    });

    let report = runner_with(Arc::clone(&worker), CancellationToken::new())
        .run()
        .await;

    assert_eq!(worker.attempts(), [0]);
    assert_eq!(report.failures.len(), 1);
    assert!(!report.failures[0].error.is_transient());
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_run_does_not_retry_a_transient_error() {
    let cancel = CancellationToken::new();
    let worker = Arc::new(FlakyDiscovery {
        transient_failures: u32::MAX,
        then_permanent: false,
        cancel_on_first_call: Some(cancel.clone()),
        attempts: Mutex::new(Vec::new()),
    });

    let report = runner_with(Arc::clone(&worker), cancel).run().await;

    assert_eq!(worker.attempts(), [0], "no retry once the run is cancelled");
    assert_eq!(report.failures.len(), 1);
    assert!(report.cancelled);
}

struct FlakyRepair {
    seen: Mutex<Vec<(u32, u32)>>,
}

#[async_trait]
impl Worker for FlakyRepair {
    fn handles(&self, kind: TaskKind) -> bool {
        matches!(kind, TaskKind::Discover | TaskKind::Verify(_))
    }

    async fn execute(&self, ctx: &WorkerContext, task: &ExtractionTask) -> Result<(), DomainError> {
        if task.kind == TaskKind::Discover {
            ctx.queue.enqueue_task(&NewTask {
                run: task.run,
                kind: TaskKind::Verify(Entity::PullRequest),
                entity_id: Some("13".to_owned()),
                priority: TaskPriority::HIGH,
                attempt: 2,
            });
            return Ok(());
        }
        self.seen.lock().unwrap().push((task.attempt, task.retries));
        if task.retries < 2 {
            return Err(locked());
        }
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_transient_retry_of_a_repair_pass_keeps_its_attempt_and_counts_its_retries() {
    let worker = Arc::new(FlakyRepair {
        seen: Mutex::new(Vec::new()),
    });
    let runner = RepoPhaseRunner::new(
        vec![Arc::clone(&worker) as Arc<dyn Worker>],
        RunIdentity {
            session_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
        },
        NonZeroUsize::MIN,
        CancellationToken::new(),
        Arc::new(AtomicU8::new(0)),
    );

    let report = runner.run().await;

    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.tasks_done, 2);
    assert_eq!(
        *worker.seen.lock().unwrap(),
        [(2, 0), (2, 1), (2, 2)],
        "the repair pass stays 2 while its retries go up"
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Started {
        kind: TaskKind,
        in_flight: usize,
        pending: u64,
        after_cancel: bool,
    },
    Finished(TaskKind),
}

/// A worker that plays the whole engine: Discovery seeds two Index tasks,
/// each Index task seeds `refines_per_index` Refine tasks, and a chosen
/// Refine task may seed a Verify task or cancel the run.
struct ScriptedWorker {
    refines_per_index: usize,
    verify_from: Option<&'static str>,
    cancel_from: Option<(&'static str, CancellationToken)>,
    pause: Option<Duration>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    events: Mutex<Vec<Event>>,
}

impl ScriptedWorker {
    fn new(refines_per_index: usize, pause: Option<Duration>) -> Self {
        Self {
            refines_per_index,
            verify_from: None,
            cancel_from: None,
            pause,
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn seed(ctx: &WorkerContext, task: &ExtractionTask, kind: TaskKind, entity_id: Option<String>) {
        ctx.queue.enqueue_task(&NewTask {
            run: task.run,
            kind,
            entity_id,
            priority: TaskPriority::NORMAL,
            attempt: 0,
        });
    }
}

#[async_trait]
impl Worker for ScriptedWorker {
    fn handles(&self, _kind: TaskKind) -> bool {
        true
    }

    async fn execute(&self, ctx: &WorkerContext, task: &ExtractionTask) -> Result<(), DomainError> {
        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(in_flight, Ordering::SeqCst);
        self.events.lock().unwrap().push(Event::Started {
            kind: task.kind,
            in_flight,
            pending: ctx.queue.pending_count(task.run.session_id),
            after_cancel: ctx.cancel.is_cancelled(),
        });

        match task.kind {
            TaskKind::Discover => {
                Self::seed(ctx, task, TaskKind::Index(Family::Issues), None);
                Self::seed(ctx, task, TaskKind::Index(Family::PullRequests), None);
            }
            TaskKind::Index(family) => {
                let entity = match family {
                    Family::PullRequests => Entity::PullRequest,
                    _ => Entity::Issue,
                };
                for n in 0..self.refines_per_index {
                    Self::seed(
                        ctx,
                        task,
                        TaskKind::Refine(entity),
                        Some(format!("{family:?}-{n}")),
                    );
                }
            }
            TaskKind::Refine(_) => {
                let id = task.entity_id.as_deref().unwrap_or_default();
                if self.verify_from == Some(id) {
                    Self::seed(
                        ctx,
                        task,
                        TaskKind::Verify(Entity::Issue),
                        Some("1".to_owned()),
                    );
                }
                if let Some((at, cancel)) = &self.cancel_from
                    && *at == id
                {
                    cancel.cancel();
                }
            }
            TaskKind::Verify(_) => {}
        }

        match self.pause {
            Some(pause) => tokio::time::sleep(pause).await,
            None => tokio::task::yield_now().await,
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.events.lock().unwrap().push(Event::Finished(task.kind));
        Ok(())
    }
}

fn runner_over(
    worker: Arc<ScriptedWorker>,
    lanes: usize,
    cancel: CancellationToken,
) -> RepoPhaseRunner {
    RepoPhaseRunner::new(
        vec![worker],
        RunIdentity {
            session_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
        },
        NonZeroUsize::new(lanes).unwrap(),
        cancel,
        Arc::new(AtomicU8::new(0)),
    )
}

fn position(events: &[Event], wanted: impl Fn(&Event) -> bool) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| wanted(event))
        .map(|(index, _)| index)
        .collect()
}

fn starts_of(events: &[Event], phase: TaskPhase) -> Vec<usize> {
    position(
        events,
        |event| matches!(event, Event::Started { kind, .. } if kind.phase() == phase),
    )
}

fn ends_of(events: &[Event], phase: TaskPhase) -> Vec<usize> {
    position(
        events,
        |event| matches!(event, Event::Finished(kind) if kind.phase() == phase),
    )
}

#[tokio::test(start_paused = true)]
async fn phases_drain_in_order_with_three_tasks_in_flight() {
    let worker = Arc::new(ScriptedWorker {
        verify_from: Some("Issues-0"),
        ..ScriptedWorker::new(4, Some(Duration::from_millis(1)))
    });
    let runner = runner_over(Arc::clone(&worker), 3, CancellationToken::new());

    let report = runner.run().await;

    assert_eq!(report.tasks_done, 1 + 2 + 8 + 1, "{:?}", report.failures);
    assert!(report.failures.is_empty());
    assert!(!report.cancelled);
    assert_eq!(runner.progress.load(Ordering::Relaxed), 100);
    assert_eq!(
        worker.max_in_flight.load(Ordering::SeqCst),
        3,
        "the refinement flood must use every lane"
    );

    let events = worker.events();
    let discovery_done = ends_of(&events, TaskPhase::Discovery)[0];
    assert!(
        starts_of(&events, TaskPhase::Indexing)
            .iter()
            .all(|&start| start > discovery_done),
        "nothing is indexed before discovery has finished"
    );
    let last_refine_done = *ends_of(&events, TaskPhase::Refinement).last().unwrap();
    assert!(
        starts_of(&events, TaskPhase::Verification)
            .iter()
            .all(|&start| start > last_refine_done),
        "verification waits for every refinement, even one seeded mid-way"
    );
}

#[tokio::test(start_paused = true)]
async fn above_the_backlog_bound_the_runner_claims_one_task_at_a_time() {
    let per_index = usize::try_from(BACKPRESSURE_HIGH).unwrap().div_euclid(2) + 1;
    let worker = Arc::new(ScriptedWorker::new(per_index, None));
    let runner = runner_over(Arc::clone(&worker), 3, CancellationToken::new());

    let report = runner.run().await;

    assert_eq!(
        report.tasks_done,
        1 + 2 + 2 * u64::try_from(per_index).unwrap()
    );
    let starts: Vec<(u64, usize)> = worker
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Started {
                pending, in_flight, ..
            } => Some((pending, in_flight)),
            Event::Finished(_) => None,
        })
        .collect();
    let throttled: Vec<&(u64, usize)> = starts
        .iter()
        .filter(|(pending, _)| *pending >= BACKPRESSURE_HIGH)
        .collect();
    assert!(
        throttled.len() >= 2,
        "the backlog must have crossed the bound"
    );
    assert!(
        throttled.iter().all(|(_, in_flight)| *in_flight == 1),
        "while the backlog is above the bound only one task runs at a time"
    );
    assert!(
        starts.iter().any(|(_, in_flight)| *in_flight == 3),
        "once the backlog drains the lanes fill up again"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_run_stops_claiming_and_lets_running_tasks_finish() {
    let cancel = CancellationToken::new();
    let worker = Arc::new(ScriptedWorker {
        verify_from: Some("Issues-0"),
        cancel_from: Some(("Issues-1", cancel.clone())),
        ..ScriptedWorker::new(4, Some(Duration::from_millis(1)))
    });
    let runner = runner_over(Arc::clone(&worker), 3, cancel);

    let report = runner.run().await;

    assert!(report.cancelled);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let events = worker.events();
    let started = starts_of(&events, TaskPhase::Refinement).len()
        + starts_of(&events, TaskPhase::Indexing).len()
        + 1;
    assert_eq!(
        report.tasks_done,
        u64::try_from(started).unwrap(),
        "every task that started was allowed to finish"
    );
    assert!(
        report.tasks_done < 1 + 2 + 8,
        "the rest of the refinements were never claimed"
    );
    let after_cancel = position(&events, |event| {
        matches!(
            event,
            Event::Started {
                after_cancel: true,
                ..
            }
        )
    });
    assert!(
        after_cancel.len() < 3,
        "only tasks already spawned alongside the cancelling one may still start: {after_cancel:?}"
    );
    assert!(
        starts_of(&events, TaskPhase::Verification).is_empty(),
        "a cancelled run never reaches verification"
    );
}

struct PanickingDiscovery;

#[async_trait]
impl Worker for PanickingDiscovery {
    fn handles(&self, kind: TaskKind) -> bool {
        kind == TaskKind::Discover
    }

    async fn execute(
        &self,
        _ctx: &WorkerContext,
        _task: &ExtractionTask,
    ) -> Result<(), DomainError> {
        panic!("a worker fell over");
    }
}

/// A task that panics never reaches its own bookkeeping: the queue entry is
/// left `Running` and the join handle comes back as a bare `JoinError` that
/// does not say which task it was. The side map is what puts both right, so
/// the run ends instead of waiting for a task nobody will finish.
#[tokio::test(start_paused = true)]
async fn a_task_that_panics_is_accounted_for_and_the_run_still_ends() {
    let session_id = Uuid::new_v4();
    let runner = RepoPhaseRunner::new(
        vec![Arc::new(PanickingDiscovery)],
        RunIdentity {
            session_id,
            tenant_id: Uuid::new_v4(),
        },
        NonZeroUsize::MIN,
        CancellationToken::new(),
        Arc::new(AtomicU8::new(0)),
    );

    let report = runner.run().await;

    assert_eq!(report.tasks_done, 0);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(
        report.failures[0].kind,
        Some(TaskKind::Discover),
        "without the side map a JoinError cannot say which task died"
    );
    assert!(
        report.failures[0]
            .error
            .to_string()
            .contains("did not finish cleanly"),
        "{}",
        report.failures[0].error
    );
    assert_eq!(
        runner
            .queue
            .remaining_count_for_phase(session_id, TaskPhase::Discovery),
        0,
        "the panicked task's queue entry must be failed, not left running"
    );
}
