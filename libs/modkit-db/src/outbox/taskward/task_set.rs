use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

struct NamedTask {
    name: String,
    handle: JoinHandle<()>,
}

/// A collection of named spawned tasks with structured shutdown.
///
/// Replaces ad-hoc `Vec<JoinHandle<()>>` with named tasks and per-task
/// error reporting on shutdown.
pub struct TaskSet {
    cancel: CancellationToken,
    tasks: Vec<NamedTask>,
}

impl TaskSet {
    #[must_use]
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            tasks: Vec::new(),
        }
    }

    pub fn spawn(
        &mut self,
        name: impl Into<String>,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let name = name.into();
        let handle = tokio::spawn(future);
        self.tasks.push(NamedTask { name, handle });
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Gracefully shut down all tasks.
    ///
    /// 1. Cancels the shared token
    /// 2. Joins each handle in spawn order
    /// 3. Logs per-task outcome
    pub async fn shutdown(mut self) {
        self.cancel.cancel();

        for task in std::mem::take(&mut self.tasks) {
            match task.handle.await {
                Ok(()) => {
                    debug!(name = task.name, "task stopped");
                }
                Err(e) if e.is_panic() => {
                    error!(name = task.name, error = %e, "task panicked");
                }
                Err(e) => {
                    warn!(name = task.name, error = %e, "task join error");
                }
            }
        }
    }
}

impl Drop for TaskSet {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
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
}
