#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cluster_sdk::{ElectionConfig, LeaderStatus, LeaderWatch, LeaderWatchEvent, LeaderWatchSender};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{ElectionTiming, MAX_RESIGN_WAIT, drive, join_bounded};
use crate::domain::error::DomainError;
use crate::domain::leadership::{LeaderWork, LeadershipName};

/// How long a test waits for an event before it fails instead of hanging the job.
const WAIT: Duration = Duration::from_secs(5);

/// The next event, or a failure after [`WAIT`].
async fn next(events: &mut mpsc::UnboundedReceiver<&'static str>) -> Option<&'static str> {
    tokio::time::timeout(WAIT, events.recv())
        .await
        .expect("an event arrives in time")
}

/// No event arrives for a while (a closed channel is quiet too).
async fn quiet(events: &mut mpsc::UnboundedReceiver<&'static str>) {
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
        panic!("no further event, got {event:?}");
    }
}

fn timing(stop: Duration) -> ElectionTiming {
    ElectionTiming::new(
        ElectionConfig::new(Duration::from_secs(30), 2).unwrap(),
        stop,
    )
}

/// Work that reports each start and each stop on `events` and runs until cancelled.
fn reporting_work(events: mpsc::UnboundedSender<&'static str>) -> LeaderWork {
    Arc::new(move |term: CancellationToken| {
        let events = events.clone();
        Box::pin(async move {
            let _sent = events.send("started");
            term.cancelled().await;
            let _sent = events.send("stopped");
        })
    })
}

/// The sender driving the watch, the shutdown token, the work's and the backend's events (the
/// backend reports `resigned`), and the driver task.
type Driven = (
    LeaderWatchSender,
    CancellationToken,
    mpsc::UnboundedReceiver<&'static str>,
    tokio::task::JoinHandle<Result<(), DomainError>>,
);

fn drive_watch(initial: LeaderStatus) -> Driven {
    let (tx, rx) = mpsc::unbounded_channel();
    drive_with(
        initial,
        reporting_work(tx.clone()),
        tx,
        Duration::from_secs(5),
        rx,
    )
}

fn drive_with(
    initial: LeaderStatus,
    work: LeaderWork,
    events: mpsc::UnboundedSender<&'static str>,
    stop: Duration,
    rx: mpsc::UnboundedReceiver<&'static str>,
) -> Driven {
    let (sender, mut resigns, watch) = LeaderWatch::channel(8, initial);
    tokio::spawn(async move {
        while let Some(responder) = resigns.recv().await {
            let _sent = events.send("resigned");
            responder.respond(Ok(()));
        }
    });
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(drive(
        LeadershipName::ExceptionSweep,
        watch,
        shutdown.clone(),
        work,
        timing(stop),
    ));
    (sender, shutdown, rx, task)
}

async fn finished(
    task: tokio::task::JoinHandle<Result<(), DomainError>>,
) -> Result<(), DomainError> {
    tokio::time::timeout(WAIT, task)
        .await
        .expect("the driver returns in time")
        .unwrap()
}

#[tokio::test]
async fn the_work_runs_only_while_leading() {
    let (sender, shutdown, mut events, task) = drive_watch(LeaderStatus::Follower);
    sender.send_status(LeaderStatus::Leader).await.unwrap();
    assert_eq!(next(&mut events).await, Some("started"));
    // A forced loss stops the work.
    sender.send_status(LeaderStatus::Lost).await.unwrap();
    assert_eq!(next(&mut events).await, Some("stopped"));
    // Re-election starts it again.
    sender.send_status(LeaderStatus::Leader).await.unwrap();
    assert_eq!(next(&mut events).await, Some("started"));
    shutdown.cancel();
    // On shutdown the work stops first, then the election is resigned, once.
    assert_eq!(next(&mut events).await, Some("stopped"));
    assert_eq!(next(&mut events).await, Some("resigned"));
    finished(task).await.unwrap();
    quiet(&mut events).await;
}

#[tokio::test]
async fn a_closed_election_stops_the_work_and_fails() {
    let (sender, _shutdown, mut events, task) = drive_watch(LeaderStatus::Follower);
    sender.send_status(LeaderStatus::Leader).await.unwrap();
    assert_eq!(next(&mut events).await, Some("started"));
    drop(sender);
    assert_eq!(next(&mut events).await, Some("stopped"));
    assert!(matches!(
        finished(task).await,
        Err(DomainError::Coordination(..))
    ));
}

#[tokio::test]
async fn a_gap_stops_the_work_and_resumes_it_only_while_the_snapshot_reads_leader() {
    let (mut sender, _shutdown, mut events, _task) = drive_watch(LeaderStatus::Follower);
    sender.send_status(LeaderStatus::Leader).await.unwrap();
    assert_eq!(next(&mut events).await, Some("started"));
    // A reset while the snapshot still reads `Leader`: stop, then resume.
    assert!(sender.try_send(LeaderWatchEvent::Reset));
    assert_eq!(next(&mut events).await, Some("stopped"));
    assert_eq!(next(&mut events).await, Some("started"));
    // A gap that hides a loss: the gap arrives first while the snapshot already reads
    // `Follower`, so the work stops and does not resume.
    assert!(sender.try_send(LeaderWatchEvent::Lagged { dropped: 1 }));
    assert!(sender.try_send_status(LeaderStatus::Follower));
    assert_eq!(next(&mut events).await, Some("stopped"));
    quiet(&mut events).await;
}

#[tokio::test]
async fn work_that_ends_while_leading_gives_the_leadership_up() {
    let (tx, rx) = mpsc::unbounded_channel();
    let panicking: LeaderWork = Arc::new(|_term| {
        Box::pin(async {
            panic!("the sweeper failed");
        })
    });
    let (sender, _shutdown, mut events, task) = drive_with(
        LeaderStatus::Follower,
        panicking,
        tx,
        Duration::from_secs(5),
        rx,
    );
    sender.send_status(LeaderStatus::Leader).await.unwrap();
    // The instance resigns instead of holding the claim with no work running.
    assert_eq!(next(&mut events).await, Some("resigned"));
    let ended = finished(task).await;
    assert!(
        matches!(ended, Err(DomainError::LeaderWorkEnded(ref detail, Some(_))) if detail.contains("panicked")),
        "{ended:?}"
    );
    // The panic's message survives as the error's source, so the log says where it panicked.
    let source = ended
        .as_ref()
        .err()
        .and_then(std::error::Error::source)
        .map(ToString::to_string);
    assert!(
        source
            .as_deref()
            .is_some_and(|s| s.contains("the sweeper failed")),
        "{source:?}"
    );
}

#[tokio::test]
async fn work_that_ignores_cancellation_is_aborted_after_the_budget() {
    let started = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&started);
    let stubborn: LeaderWork = Arc::new(move |_term| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        })
    });
    let (tx, rx) = mpsc::unbounded_channel();
    let (sender, shutdown, mut events, task) = drive_with(
        LeaderStatus::Follower,
        stubborn,
        tx,
        Duration::from_millis(50),
        rx,
    );
    sender.send_status(LeaderStatus::Leader).await.unwrap();
    tokio::time::timeout(WAIT, async {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the work starts");
    shutdown.cancel();
    finished(task).await.unwrap();
    assert_eq!(next(&mut events).await, Some("resigned"));
}

#[tokio::test(start_paused = true)]
async fn a_join_the_backend_never_answers_ends_after_one_ttl() {
    let shutdown = CancellationToken::new();
    let joined = join_bounded(
        LeadershipName::ExceptionSweep,
        std::future::pending(),
        &shutdown,
        Duration::from_secs(30),
    )
    .await;
    assert!(matches!(joined, Err(DomainError::Coordination(..))));
}

#[tokio::test]
async fn shutdown_ends_a_join_the_backend_never_answers() {
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    let joined = join_bounded(
        LeadershipName::ExceptionSweep,
        std::future::pending(),
        &shutdown,
        Duration::from_secs(3600),
    )
    .await;
    assert!(matches!(joined, Ok(None)));
}

#[test]
fn the_resign_budget_never_exceeds_the_ttl() {
    let short = ElectionTiming::new(
        ElectionConfig::new(Duration::from_secs(3), 2).unwrap(),
        Duration::from_secs(1),
    );
    assert_eq!(short.resign_timeout(), Duration::from_secs(3));
    assert_eq!(
        timing(Duration::from_secs(1)).resign_timeout(),
        MAX_RESIGN_WAIT
    );
    assert_eq!(
        timing(Duration::from_secs(1)).join_timeout(),
        Duration::from_secs(30)
    );
}
