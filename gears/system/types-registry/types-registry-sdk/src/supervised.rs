//! Supervised status settles on every exit (SPEC D16, D21).
//! Unfinished status becomes [`SupervisedStatus::stopped`]. Dropping [`Supervised`] detaches;
//! cancel through the token and join on shutdown.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;
use tokio_util::sync::CancellationToken;
use toolkit::tokio;
use toolkit::tokio::sync::watch;
use toolkit::tokio::task::JoinHandle;

/// A status a supervised worker reports, with its terminal form.
pub trait SupervisedStatus: Clone + Send + Sync + 'static {
    /// The worker has nothing left to do; later reports are ignored.
    fn is_terminal(&self) -> bool;

    /// Must return terminal status, preserving already settled outcomes.
    #[must_use]
    fn stopped(self, exit: &TaskExit) -> Self;
}

/// How a supervised worker ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskExit {
    Returned,
    /// The worker returned an error; carries its rendered chain.
    Failed(String),
    /// The worker panicked; carries the panic message.
    Panicked(String),
    /// The cancellation token fired; the worker was dropped at its next await point.
    Cancelled,
}

impl fmt::Display for TaskExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Returned => f.write_str("returned before its status was terminal"),
            Self::Failed(e) => write!(f, "failed: {e}"),
            Self::Panicked(m) => write!(f, "panicked: {m}"),
            Self::Cancelled => f.write_str("was cancelled"),
        }
    }
}

pub(crate) struct StatusReporter<S> {
    tx: Arc<watch::Sender<S>>,
}

impl<S: SupervisedStatus> StatusReporter<S> {
    /// Publish only while nonterminal; return whether status changed.
    pub(crate) fn report(&self, status: S) -> bool {
        self.tx.send_if_modified(|current| {
            if current.is_terminal() {
                false
            } else {
                *current = status;
                true
            }
        })
    }

    /// The status last published.
    #[must_use]
    pub(crate) fn current(&self) -> S {
        self.tx.borrow().clone()
    }
}

/// Status and sole join handle; dropping detaches without cancelling.
#[derive(Debug)]
pub struct Supervised<S> {
    status: watch::Receiver<S>,
    task: JoinHandle<TaskExit>,
}

impl<S: SupervisedStatus> Supervised<S> {
    #[must_use]
    pub fn status(&self) -> S {
        self.status.borrow().clone()
    }

    /// A receiver of status changes that outlives this handle.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<S> {
        self.status.clone()
    }

    /// Wait for task exit; status is terminal before returning.
    pub async fn join(self) -> TaskExit {
        // Worker panics are caught; runtime cancellation settles status through the Settle guard.
        self.task.await.unwrap_or_else(|e| {
            if e.is_cancelled() {
                TaskExit::Cancelled
            } else {
                TaskExit::Panicked(format!("supervisor task lost: {e}"))
            }
        })
    }
}

/// Run the worker with a child token; settle unfinished status on any exit.
/// Catch panics in construction, polling, and cancellation drop; name identifies logs.
///
/// # Panics
/// Called outside a Tokio runtime.
pub(crate) fn spawn_supervised<S, F, Fut>(
    name: impl Into<String>,
    initial: S,
    cancel: CancellationToken,
    worker: F,
) -> Supervised<S>
where
    S: SupervisedStatus,
    F: FnOnce(StatusReporter<S>, CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let name = name.into();
    let (tx, status) = watch::channel(initial);
    let tx = Arc::new(tx);
    let reporter = StatusReporter { tx: tx.clone() };

    // Construct before spawn so runtime shutdown settles even a never-polled task.
    let mut settle = Settle { tx: Some(tx) };
    let task = tokio::spawn(async move {
        // Catch construction/poll/drop panics; the child token protects the caller from cancellation.
        let cancel = cancel.child_token();
        let token = cancel.clone();
        let supervised = AssertUnwindSafe(async move {
            tokio::select! {
                biased;
                () = token.cancelled() => TaskExit::Cancelled,
                outcome = worker(reporter, cancel) => match outcome {
                    Ok(()) => TaskExit::Returned,
                    Err(e) => TaskExit::Failed(format!("{e:#}")),
                },
            }
        });
        let exit = supervised
            .catch_unwind()
            .await
            .unwrap_or_else(|payload| TaskExit::Panicked(panic_message(payload.as_ref())));

        let settled = settle.settle(&exit);
        match &exit {
            TaskExit::Failed(_) | TaskExit::Panicked(_) => {
                tracing::error!(task = %name, exit = %exit, "supervised task ended abnormally");
            }
            TaskExit::Returned if settled => {
                tracing::warn!(task = %name, exit = %exit, "supervised task ended early");
            }
            TaskExit::Returned | TaskExit::Cancelled => {
                tracing::debug!(task = %name, exit = %exit, "supervised task ended");
            }
        }
        exit
    });

    Supervised { status, task }
}

/// Settle once with the observed exit, or Cancelled on runtime drop before completion.
struct Settle<S: SupervisedStatus> {
    tx: Option<Arc<watch::Sender<S>>>,
}

impl<S: SupervisedStatus> Settle<S> {
    /// Replaces a non-terminal status with its stopped form; returns whether it did.
    fn settle(&mut self, exit: &TaskExit) -> bool {
        let Some(tx) = self.tx.take() else {
            return false;
        };
        tx.send_if_modified(|current| {
            if current.is_terminal() {
                false
            } else {
                *current = current.clone().stopped(exit);
                debug_assert!(
                    current.is_terminal(),
                    "SupervisedStatus::stopped must be terminal"
                );
                true
            }
        })
    }
}

impl<S: SupervisedStatus> Drop for Settle<S> {
    fn drop(&mut self) {
        self.settle(&TaskExit::Cancelled);
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(ToString::to_string)
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

impl SupervisedStatus for crate::publication::PublicationStatus {
    fn is_terminal(&self) -> bool {
        Self::is_terminal(self)
    }

    fn stopped(self, exit: &TaskExit) -> Self {
        Self::stopped(self, &format!("publisher {exit}"))
    }
}

#[cfg(test)]
#[path = "supervised_tests.rs"]
mod supervised_tests;
