use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Exclusive, OpenError, on_abandonable_thread};
use std::time::Duration;

/// A build that never returns is abandoned at the deadline and named as
/// the hang it is, rather than waited on for ever.
#[tokio::test]
async fn a_build_that_hangs_is_abandoned_at_the_deadline() {
    let started = std::time::Instant::now();
    let outcome = on_abandonable_thread("onnx-test-hang", Duration::from_millis(100), || {
        std::thread::sleep(Duration::from_secs(5));
        Ok::<(), OpenError>(())
    })
    .await;
    assert!(
        matches!(outcome, Err(OpenError::RuntimeHung { .. })),
        "{outcome:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the caller is released at the deadline, not when the thread ends"
    );
}

/// A build that panics is a refusal with a reason, not a hang.
#[tokio::test]
async fn a_build_that_panics_is_a_refusal() {
    let outcome = on_abandonable_thread("onnx-test-panic", Duration::from_secs(5), || {
        panic!("the runtime blew up");
        #[allow(unreachable_code)]
        Ok::<(), OpenError>(())
    })
    .await;
    assert!(
        matches!(&outcome, Err(OpenError::Session(reason)) if reason.contains("without a result")),
        "{outcome:?}"
    );
}

/// A build that answers in time is handed over as it was built.
#[tokio::test]
async fn a_build_in_time_is_returned() {
    let outcome = on_abandonable_thread("onnx-test-ok", Duration::from_secs(5), || {
        Ok::<u32, OpenError>(7)
    })
    .await;
    assert_eq!(outcome.ok(), Some(7));
}

/// On a multi-threaded runtime the work runs under `block_in_place`, so a
/// long call does not hold the worker another task is scheduled on.
///
/// The call is made from a spawned task, so it occupies the runtime's one
/// worker: the test body itself runs on the `block_on` thread, which a
/// worker is not. A task spawned beside it can only run while the call is
/// busy if the call has handed the worker back.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn long_work_does_not_hold_the_only_worker() {
    let shared = Arc::new(Exclusive::new(0_u32));
    let seen = tokio::spawn(async move {
        let other_ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&other_ran);
        let other = tokio::spawn(async move { flag.store(true, Ordering::SeqCst) });
        let seen = shared
            .run(|value| {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !other_ran.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                *value += 1;
                other_ran.load(Ordering::SeqCst)
            })
            .await;
        other.await.expect("the other task completes");
        seen
    })
    .await
    .expect("the caller completes");
    assert!(
        seen,
        "the other task ran while the work held the session, on a runtime with one worker"
    );
}

/// On a current-thread runtime `block_in_place` would panic; the work runs
/// directly there.
#[tokio::test(flavor = "current_thread")]
async fn work_runs_on_a_current_thread_runtime() {
    let shared = Exclusive::new(1_u32);
    assert_eq!(shared.run(|value| *value + 1).await, 2);
    shared.wait_until_idle().await;
}

/// Callers take the session one at a time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_take_the_session_one_at_a_time() {
    let shared = Arc::new(Exclusive::new(Vec::<u32>::new()));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let shared = Arc::clone(&shared);
        tasks.push(tokio::spawn(async move {
            shared
                .run(|log| {
                    log.push(index);
                    std::thread::sleep(Duration::from_millis(5));
                    log.push(index);
                })
                .await;
        }));
    }
    for task in tasks {
        task.await.expect("each caller completes");
    }
    let log = shared.run_blocking_for_test();
    for pair in log.chunks(2) {
        assert_eq!(pair[0], pair[1], "no two callers interleaved: {log:?}");
    }
}

impl<T: Clone> Exclusive<T> {
    fn run_blocking_for_test(&self) -> T {
        self.value.try_lock().expect("nobody holds it").clone()
    }
}
