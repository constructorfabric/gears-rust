use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::domain::error::DomainError;
use crate::domain::record_intake::{ReceivedRecord, RecordHandOff};

/// [`RecordHandOff`] onto a bounded queue in memory. A full queue refuses the record, so intake keeps no identity of
/// it and the connector can send it again. A record still queued when the gear stops is lost without an event.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-queue:p1
pub struct QueuedHandOff {
    sender: mpsc::Sender<ReceivedRecord>,
}

/// The receiving end of the queue, which [`serve`] drains.
pub struct RecordQueue {
    receiver: mpsc::Receiver<ReceivedRecord>,
}

/// A queue that holds at most `capacity` records.
#[must_use]
pub fn record_queue(capacity: NonZeroUsize) -> (QueuedHandOff, RecordQueue) {
    let (sender, receiver) = mpsc::channel(capacity.get());
    (QueuedHandOff { sender }, RecordQueue { receiver })
}

impl RecordHandOff for QueuedHandOff {
    fn hand_off(&self, record: ReceivedRecord) -> Result<(), DomainError> {
        self.sender.try_send(record).map_err(|error| {
            DomainError::Unavailable(
                match error {
                    TrySendError::Full(_) => "the processing queue is full",
                    TrySendError::Closed(_) => "processing has stopped",
                }
                .to_owned(),
            )
        })
    }
}

/// Runs `process` on each queued record, on at most `concurrency` records at once. It returns when `cancel` fires,
/// stopping the records in flight, also while it waits for them after the queue is closed, or when the queue is closed
/// and the records in flight are done.
pub async fn serve<P, F>(
    mut queue: RecordQueue,
    process: P,
    concurrency: NonZeroUsize,
    cancel: CancellationToken,
) where
    P: Fn(ReceivedRecord) -> F,
    F: Future<Output = ()> + Send + 'static,
{
    let slots = Arc::new(Semaphore::new(concurrency.get()));
    let mut running = JoinSet::new();
    loop {
        let slot = tokio::select! {
            () = cancel.cancelled() => break,
            slot = Arc::clone(&slots).acquire_owned() => slot,
        };
        let Ok(slot) = slot else { break };
        let record = tokio::select! {
            () = cancel.cancelled() => break,
            record = queue.receiver.recv() => record,
        };
        let Some(record) = record else {
            tokio::select! {
                () = cancel.cancelled() => break,
                () = async { while running.join_next().await.is_some() {} } => return,
            }
        };
        let processed = process(record);
        running.spawn(async move {
            processed.await;
            drop(slot);
        });
        while running.try_join_next().is_some() {}
    }
    running.shutdown().await;
}
