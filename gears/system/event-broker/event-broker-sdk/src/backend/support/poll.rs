//! Turns a [`Watch::Poll`](crate::backend::Watch::Poll) interval into the
//! same stream of heads a native watch yields.

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::api::Position;
use crate::backend::{Backend, BackendError, Head, TopicPartition};
use crate::sequence::Sequence;

/// Items buffered between the polling task and a slow consumer of the stream.
const BUFFER: usize = 256;

/// Heads of `partitions`, found by asking `resolve(Latest)` every `interval`:
/// first the current head of each partition, then one head per partition
/// whose `Latest` rose since the previous tick.
///
/// Polling runs in a task of its own, started at once. Errors arrive as
/// stream items and do not end the stream; dropping the stream stops the task.
pub fn poll_heads(
    backend: Arc<dyn Backend>,
    partitions: &[TopicPartition],
    interval: Duration,
) -> BoxStream<'static, Result<Head, BackendError>> {
    let (tx, rx) = mpsc::channel(BUFFER);
    let partitions = partitions.to_vec();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut last: Vec<Option<Sequence>> = vec![None; partitions.len()];
        loop {
            tokio::select! {
                () = tx.closed() => return,
                _ = ticker.tick() => {}
            }
            for (seen, at) in last.iter_mut().zip(&partitions) {
                let item = match backend.resolve(at, Position::Latest).await {
                    Ok(head) if seen.is_some_and(|seen| head <= seen) => continue,
                    Ok(head) => {
                        *seen = Some(head);
                        Ok(Head {
                            at: at.clone(),
                            sequence: head,
                        })
                    }
                    Err(err) => Err(err),
                };
                if tx.send(item).await.is_err() {
                    return;
                }
            }
        }
    });
    stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
    .boxed()
}
