use std::collections::VecDeque;

use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot};

use crate::grpc_registration::RegisterGrpcServiceFn;

/// Installers for a specific gear (gear name + service installers).
#[derive(Default)]
pub struct GearInstallers {
    pub gear_name: String,
    pub installers: Vec<RegisterGrpcServiceFn>,
}

/// Grouped installers for all gears in the process.
#[derive(Default)]
pub struct GrpcInstallerData {
    pub gears: Vec<GearInstallers>,
    /// Whether a later batch may follow ([`GrpcInstallerStore::publish_late`]):
    /// the application group's services, installed once it has started. An
    /// empty initial batch with more to come still needs a listener.
    pub more_batches: bool,
}

/// A batch of installers published after the hub started serving. The hub
/// installs it on its running listener, then [`ack`](Self::ack)s it.
pub struct LateBatch {
    pub gears: Vec<GearInstallers>,
    ack: oneshot::Sender<anyhow::Result<()>>,
}

impl LateBatch {
    /// Report whether the batch is installed and callable.
    pub fn ack(self, result: anyhow::Result<()>) {
        // The publisher stopped waiting: nothing to report to.
        drop(self.ack.send(result));
    }
}

/// Runtime-owned gRPC installers: the hub takes system services before serving,
/// then installs and acknowledges application services on the same listener.
/// Each batch runs once; earlier installers are never rerun.
pub struct GrpcInstallerStore {
    inner: Mutex<Inner>,
    late_published: Notify,
}

#[derive(Default)]
struct Inner {
    initial: Option<GrpcInstallerData>,
    initial_set: bool,
    late: VecDeque<LateBatch>,
    closed: bool,
}

impl GrpcInstallerStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            late_published: Notify::new(),
        }
    }

    /// Set the initial batch once. Fails if already set.
    ///
    /// # Errors
    /// Returns an error if the initial batch has already been set.
    pub fn set(&self, data: GrpcInstallerData) -> anyhow::Result<()> {
        let mut guard = self.inner.lock();
        if guard.initial_set {
            anyhow::bail!("gRPC installers already initialized");
        }
        guard.initial_set = true;
        guard.initial = Some(data);
        Ok(())
    }

    /// Consume and return the initial batch.
    pub fn take(&self) -> Option<GrpcInstallerData> {
        self.inner.lock().initial.take()
    }

    /// Check if the initial batch is absent (optional helper).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().initial.is_none()
    }

    /// Publish a late batch for the running hub. The receiver resolves with
    /// the hub's [`LateBatch::ack`]; it errs when the hub drops the batch
    /// without installing it.
    ///
    /// # Errors
    /// The store is closed: no more batches are accepted.
    pub fn publish_late(
        &self,
        gears: Vec<GearInstallers>,
    ) -> anyhow::Result<oneshot::Receiver<anyhow::Result<()>>> {
        let (ack, installed) = oneshot::channel();
        {
            let mut guard = self.inner.lock();
            anyhow::ensure!(!guard.closed, "gRPC installer store is closed");
            guard.late.push_back(LateBatch { gears, ack });
        }
        self.late_published.notify_waiters();
        Ok(installed)
    }

    /// The next late batch; `None` once the store is closed and drained.
    // cancel-safe: a batch is popped and returned under the lock with no await
    // in between, so a future dropped at its await loses no batch.
    pub async fn next_late(&self) -> Option<LateBatch> {
        loop {
            let published = self.late_published.notified();
            tokio::pin!(published);
            published.as_mut().enable();
            {
                let mut guard = self.inner.lock();
                if let Some(batch) = guard.late.pop_front() {
                    return Some(batch);
                }
                if guard.closed {
                    return None;
                }
            }
            published.await;
        }
    }

    /// No more late batches: [`next_late`](Self::next_late) ends once the
    /// queued ones are taken.
    pub fn close(&self) {
        self.inner.lock().closed = true;
        self.late_published.notify_waiters();
    }
}

impl Default for GrpcInstallerStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "grpc_installers_tests.rs"]
mod grpc_installers_tests;
