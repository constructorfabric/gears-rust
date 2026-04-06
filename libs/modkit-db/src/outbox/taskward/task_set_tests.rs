use std::sync::Arc;

use super::*;

#[tokio::test]
async fn spawn_and_shutdown() {
    let cancel = CancellationToken::new();
    let mut tasks = TaskSet::new(cancel.clone());

    for i in 0..3 {
        let c = cancel.clone();
        tasks.spawn(format!("task-{i}"), async move {
            c.cancelled().await;
        });
    }

    assert_eq!(tasks.len(), 3);
    tasks.shutdown().await;
}

#[tokio::test]
async fn panicking_task_reported() {
    let cancel = CancellationToken::new();
    let mut tasks = TaskSet::new(cancel.clone());

    tasks.spawn("panicker", async {
        panic!("intentional panic");
    });

    // Give the spawned task time to panic
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // shutdown should complete without propagating the panic
    tasks.shutdown().await;
}

#[tokio::test]
async fn empty_shutdown() {
    let cancel = CancellationToken::new();
    let tasks = TaskSet::new(cancel);
    tasks.shutdown().await;
}

#[tokio::test]
async fn all_spawned_tasks_complete_on_shutdown() {
    let cancel = CancellationToken::new();
    let mut tasks = TaskSet::new(cancel.clone());
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));

    for label in ["A", "B", "C"] {
        let c = cancel.clone();
        let order_c = order.clone();
        let label = label.to_owned();
        tasks.spawn(label.clone(), async move {
            c.cancelled().await;
            order_c.lock().unwrap().push(label);
        });
    }

    tasks.shutdown().await;

    // All tasks complete after cancel — order is nondeterministic
    let mut result = order.lock().unwrap().clone();
    result.sort();
    assert_eq!(result, vec!["A", "B", "C"]);
}

#[tokio::test]
async fn drop_without_shutdown_cancels_token() {
    let cancel = CancellationToken::new();
    let child = cancel.child_token();

    {
        let mut tasks = TaskSet::new(cancel);
        let c = child.clone();
        tasks.spawn("worker", async move {
            c.cancelled().await;
        });
        // TaskSet dropped here without calling shutdown()
    }

    // The token should be cancelled by Drop
    assert!(child.is_cancelled());
}

#[tokio::test]
async fn drop_after_shutdown_is_idempotent() {
    let cancel = CancellationToken::new();
    let mut tasks = TaskSet::new(cancel.clone());

    let c = cancel.clone();
    tasks.spawn("worker", async move {
        c.cancelled().await;
    });

    // shutdown() cancels the token and joins handles
    tasks.shutdown().await;
    // Drop runs here on the consumed `self` — but shutdown takes ownership,
    // so Drop actually ran on the moved-out value. This test confirms
    // no double-panic or issue from the cancel-then-drop path.
    assert!(cancel.is_cancelled());
}

#[tokio::test]
async fn len_tracks_count() {
    let cancel = CancellationToken::new();
    let mut tasks = TaskSet::new(cancel.clone());

    assert_eq!(tasks.len(), 0);
    tasks.spawn("a", async {});
    assert_eq!(tasks.len(), 1);
    tasks.spawn("b", async {});
    tasks.spawn("c", async {});
    assert_eq!(tasks.len(), 3);

    tasks.shutdown().await;
}
