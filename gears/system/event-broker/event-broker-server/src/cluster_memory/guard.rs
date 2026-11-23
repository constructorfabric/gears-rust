//! Owned workers for SDK lock guards.

use std::convert::Infallible;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cluster_sdk::lease::LeaseToken;
use cluster_sdk::lock::{LockCommandReceiver, LockGuard, LockRequest};
use tokio::sync::{Notify, OnceCell};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction, WorkerBuilder};

use super::lease::{CacheLeaseStore, ClaimedLease};

const COMMAND_BUFFER: usize = 4;
const WORKER_NAME: &str = "eb-memory-lock-guard";

/// Owns guard tasks from registration through revoke.
pub(super) struct GuardWorkers {
    shutdown: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
    drained: OnceCell<()>,
}

impl GuardWorkers {
    pub(super) fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tasks: Mutex::new(JoinSet::new()),
            drained: OnceCell::new(),
        }
    }

    pub(super) fn shutdown(&self) -> &CancellationToken {
        &self.shutdown
    }

    fn tasks(&self) -> MutexGuard<'_, JoinSet<()>> {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.tasks().len()
    }

    pub(super) fn attach(
        &self,
        leases: Arc<CacheLeaseStore>,
        claimed: ClaimedLease,
    ) -> Result<LockGuard, ClaimedLease> {
        let mut tasks = self.tasks();
        if self.shutdown.is_cancelled() {
            return Err(claimed);
        }
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::warn!(%error, "lock guard worker failed");
            }
        }

        let token = claimed.token;
        let (receiver, guard) = LockGuard::channel(token.name.clone(), COMMAND_BUFFER);
        let wake = Arc::new(Notify::new());
        // Taskward starts idle. This permit starts the command receiver's single pass.
        wake.notify_one();
        let worker = WorkerBuilder::new(WORKER_NAME, self.shutdown.child_token())
            .notifier(wake)
            .build(GuardCommands {
                leases,
                token,
                receiver,
            });
        tasks.spawn(worker.run());
        Ok(guard)
    }

    pub(super) async fn revoke(&self) {
        {
            // Admission and revocation use the same registry lock.
            let _tasks = self.tasks();
            self.shutdown.cancel();
        }
        // One caller polls the JoinSet at a time. A cancelled caller leaves the tasks
        // in the registry so another caller can finish joining them.
        self.drained
            .get_or_init(|| async {
                while let Some(result) = poll_fn(|cx| self.tasks().poll_join_next(cx)).await {
                    if let Err(error) = result {
                        tracing::warn!(%error, "lock guard worker failed");
                    }
                }
            })
            .await;
    }
}

struct GuardCommands {
    leases: Arc<CacheLeaseStore>,
    token: LeaseToken,
    receiver: LockCommandReceiver,
}

impl WorkerAction for GuardCommands {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        loop {
            let request = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                request = self.receiver.recv() => request,
            };
            match request {
                Some(LockRequest::Renew { new_ttl, responder }) => {
                    responder.respond(self.leases.renew(&self.token, new_ttl).await);
                }
                Some(LockRequest::Release { responder }) => {
                    responder.respond(self.leases.release(&self.token).await);
                    break;
                }
                None => break,
            }
        }
        cancel.cancel();
        Ok(Directive::idle())
    }
}
