//! Supervised background tasks: every exit path settles the status (SPEC D16, D21).

use std::time::Duration;

use crate::publication::{PendingReason, PublicationState, PublicationStatus, RejectionReason};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{TaskExit, spawn_supervised};

const A: &str = "gts.cf.test.pub.a.v1~";
const B: &str = "gts.cf.test.pub.b.v1~";

fn pending_ab() -> PublicationStatus {
    PublicationStatus::pending([A, B])
}

fn admitted(ids: &[&str]) -> PublicationStatus {
    PublicationStatus {
        entities: ids
            .iter()
            .map(|id| ((*id).to_owned(), PublicationState::Admitted))
            .collect(),
    }
}

fn stopped_with(status: &PublicationStatus, id: &str) -> Option<String> {
    match &status.entities[id] {
        PublicationState::Rejected(RejectionReason::PublisherStopped { message }) => {
            Some(message.clone())
        }
        _ => None,
    }
}

#[tokio::test]
async fn a_reported_status_round_trips_through_the_handle() {
    let (go, wait) = oneshot::channel::<()>();
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        CancellationToken::new(),
        |reporter, _| async move {
            let mut s = pending_ab();
            s.entities.insert(
                A.to_owned(),
                PublicationState::Pending(PendingReason::RegistryUnreachable {
                    message: "connection refused".to_owned(),
                }),
            );
            reporter.report(s);
            wait.await.expect("the test releases the worker");
            reporter.report(admitted(&[A, B]));
            Ok(())
        },
    );

    let mut rx = handle.subscribe();
    rx.wait_for(|s| {
        matches!(
            s.entities[A],
            PublicationState::Pending(PendingReason::RegistryUnreachable { .. })
        )
    })
    .await
    .expect("worker reports");
    go.send(()).expect("worker waits");

    assert_eq!(handle.join().await, TaskExit::Returned);
    let status = rx.borrow().clone();
    assert_eq!(status, admitted(&[A, B]));
}

#[tokio::test]
async fn a_worker_that_returns_with_identifiers_pending_settles_them_as_stopped() {
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        CancellationToken::new(),
        |reporter, _| async move {
            let mut s = pending_ab();
            s.entities.insert(A.to_owned(), PublicationState::Admitted);
            reporter.report(s);
            Ok(())
        },
    );
    let rx = handle.subscribe();

    assert_eq!(handle.join().await, TaskExit::Returned);
    let status = rx.borrow().clone();
    assert!(status.is_terminal());
    assert_eq!(status.entities[A], PublicationState::Admitted);
    assert!(stopped_with(&status, B).is_some());
}

#[tokio::test]
async fn a_failing_worker_reports_a_terminal_status_naming_the_error() {
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        CancellationToken::new(),
        |_, _| async move { Err(anyhow::anyhow!("registry client missing")) },
    );
    let rx = handle.subscribe();

    let exit = handle.join().await;
    assert!(
        matches!(&exit, TaskExit::Failed(m) if m.contains("registry client missing")),
        "{exit:?}"
    );
    let message = stopped_with(&rx.borrow(), A).expect("A is stopped");
    assert!(message.contains("registry client missing"), "{message}");
}

#[tokio::test]
async fn a_panicking_worker_reports_a_terminal_status_not_pending() {
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        CancellationToken::new(),
        |_, _| async move {
            tokio::task::yield_now().await;
            panic!("boom");
        },
    );
    let rx = handle.subscribe();

    assert_eq!(handle.join().await, TaskExit::Panicked("boom".to_owned()));
    let status = rx.borrow().clone();
    assert!(status.is_terminal());
    assert!(!status.satisfies_readiness());
    assert!(
        stopped_with(&status, A)
            .expect("A is stopped")
            .contains("boom")
    );
}

#[tokio::test]
async fn a_panic_before_the_first_await_is_caught_too() {
    let handle = spawn_supervised("test", pending_ab(), CancellationToken::new(), |_, _| {
        panic!("early boom");
        #[expect(
            unreachable_code,
            reason = "the panic must fire before the worker's future is built"
        )]
        async move {
            Ok(())
        }
    });

    assert_eq!(
        handle.join().await,
        TaskExit::Panicked("early boom".to_owned())
    );
}

#[tokio::test]
async fn cancellation_stops_the_worker_and_keeps_settled_outcomes() {
    let cancel = CancellationToken::new();
    let (reported, on_reported) = oneshot::channel::<()>();
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        cancel.clone(),
        |reporter, _| async move {
            let mut s = pending_ab();
            s.entities.insert(A.to_owned(), PublicationState::Admitted);
            reporter.report(s);
            reported.send(()).expect("the test waits for the report");
            std::future::pending::<()>().await;
            Ok(())
        },
    );
    let rx = handle.subscribe();
    on_reported.await.expect("worker reports");

    cancel.cancel();

    assert_eq!(handle.join().await, TaskExit::Cancelled);
    let status = rx.borrow().clone();
    assert_eq!(status.entities[A], PublicationState::Admitted);
    assert!(stopped_with(&status, B).is_some());
}

#[tokio::test]
async fn the_worker_sees_the_cancellation_token() {
    let cancel = CancellationToken::new();
    let (started, on_started) = oneshot::channel::<CancellationToken>();
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        cancel.clone(),
        |_, token| async move {
            started.send(token).expect("the test reads it");
            std::future::pending::<()>().await;
            Ok(())
        },
    );
    let token = on_started.await.expect("worker starts");
    assert!(!token.is_cancelled());

    cancel.cancel();

    assert!(
        token.is_cancelled(),
        "the worker's token follows the caller's"
    );
    assert_eq!(handle.join().await, TaskExit::Cancelled);
}

#[test]
fn a_runtime_shutdown_under_the_worker_still_settles_the_status() {
    for polled in [true, false] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        let rx = runtime.block_on(async {
            let (started, on_started) = oneshot::channel::<()>();
            let handle = spawn_supervised(
                "test",
                pending_ab(),
                CancellationToken::new(),
                |_, _| async move {
                    started.send(()).expect("the test waits for the start");
                    std::future::pending::<()>().await;
                    Ok(())
                },
            );
            if polled {
                on_started.await.expect("worker starts");
            }
            // Return without yielding so the current-thread runtime never polls the task.
            handle.subscribe()
        });

        drop(runtime);

        let status = rx.borrow().clone();
        assert!(status.is_terminal(), "polled={polled}: {status:?}");
        assert!(
            stopped_with(&status, A).is_some(),
            "polled={polled}: {status:?}"
        );
    }
}

#[tokio::test]
async fn a_terminal_status_is_never_overwritten_back_to_pending() {
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        CancellationToken::new(),
        |reporter, _| async move {
            assert!(reporter.report(admitted(&[A, B])));
            assert!(
                !reporter.report(pending_ab()),
                "a report after terminal is ignored"
            );
            Ok(())
        },
    );
    let rx = handle.subscribe();

    assert_eq!(handle.join().await, TaskExit::Returned);
    let status = rx.borrow().clone();
    assert_eq!(status, admitted(&[A, B]));
}

#[tokio::test]
async fn a_dropped_handle_detaches_without_cancelling_and_still_settles_the_status() {
    for panics in [false, true] {
        let (go, wait) = oneshot::channel::<()>();
        let handle = spawn_supervised(
            "test",
            pending_ab(),
            CancellationToken::new(),
            move |_, _| async move {
                wait.await.expect("the test releases the worker");
                assert!(!panics, "late boom");
                Ok(())
            },
        );
        let mut rx = handle.subscribe();
        drop(handle);

        go.send(())
            .expect("dropping the handle does not cancel the worker");
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            rx.wait_for(PublicationStatus::is_terminal),
        )
        .await
        .expect("status settles")
        .expect("supervisor keeps the sender until it settles")
        .clone();
        assert!(
            stopped_with(&status, A).is_some(),
            "panics={panics}: {status:?}"
        );
    }
}

#[tokio::test]
async fn a_panic_while_dropping_a_cancelled_worker_still_settles_the_status() {
    struct PanicsOnDrop;
    impl Drop for PanicsOnDrop {
        fn drop(&mut self) {
            panic!("drop boom");
        }
    }

    let cancel = CancellationToken::new();
    let (reported, on_reported) = oneshot::channel::<()>();
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        cancel.clone(),
        |reporter, _| async move {
            let _guard = PanicsOnDrop;
            let mut s = pending_ab();
            s.entities.insert(A.to_owned(), PublicationState::Admitted);
            reporter.report(s);
            reported.send(()).expect("the test waits for the report");
            std::future::pending::<()>().await;
            Ok(())
        },
    );
    let rx = handle.subscribe();
    on_reported.await.expect("worker reports");

    cancel.cancel();

    assert_eq!(
        handle.join().await,
        TaskExit::Panicked("drop boom".to_owned())
    );
    let status = rx.borrow().clone();
    assert_eq!(status.entities[A], PublicationState::Admitted);
    assert!(
        stopped_with(&status, B)
            .expect("B is stopped")
            .contains("drop boom")
    );
}

#[tokio::test]
async fn the_worker_gets_a_child_token_and_cannot_cancel_the_caller() {
    let parent = CancellationToken::new();
    let handle = spawn_supervised(
        "test",
        pending_ab(),
        parent.clone(),
        |_, token| async move {
            token.cancel();
            std::future::pending::<()>().await;
            Ok(())
        },
    );

    assert_eq!(handle.join().await, TaskExit::Cancelled);
    assert!(!parent.is_cancelled(), "the caller's token must stay live");
}
