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
#[path = "task_set_tests.rs"]
mod tests;
