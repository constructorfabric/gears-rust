#![allow(clippy::unwrap_used, clippy::use_debug)]

//! Wake on event, with a timer as the safety net.
//!
//! A gear processes jobs from a queue. Polling it on a timer makes every job
//! wait up to a full interval; waking only on a notification loses any job
//! whose notification never arrives - written by another process, or by a
//! code path that forgot to notify.
//!
//! A worker can listen to several notifiers and wakes on whichever fires first:
//!
//! - the producer's `Notify` - jobs run within milliseconds of arriving;
//! - a slow [`poker`] - the fallback that picks up anything unannounced.
//!
//! `Notify` stores one permit when nobody is waiting, so a notification sent
//! while the worker is busy is not lost: the worker's next wait returns
//! immediately.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `wake_on_event`

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction, WorkerBuilder, poker};

type Queue = Arc<Mutex<VecDeque<(&'static str, Instant)>>>;

struct JobRunner {
    queue: Queue,
    started: Instant,
}

impl WorkerAction for JobRunner {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        // Drain everything queued, then wait for the next wakeup.
        while let Some((job, queued_at)) = self.queue.lock().unwrap().pop_front() {
            println!(
                "{:>5.0?}  ran {job:<18} waited {:.0?}",
                self.started.elapsed(),
                queued_at.elapsed(),
            );
        }
        Ok(Directive::idle())
    }
}

#[tokio::main]
async fn main() {
    let started = Instant::now();
    let cancel = CancellationToken::new();
    let queue: Queue = Arc::default();

    let job_arrived = Arc::new(Notify::new());
    let (fallback, _poker) = poker(Duration::from_millis(500), cancel.clone());

    let worker = WorkerBuilder::new("job-runner", cancel.clone())
        .notifier(Arc::clone(&job_arrived))
        .notifier(fallback)
        .build(JobRunner {
            queue: Arc::clone(&queue),
            started,
        });
    let handle = tokio::spawn(worker.run());

    let enqueue = |job: &'static str, announce: bool| {
        queue.lock().unwrap().push_back((job, Instant::now()));
        if announce {
            job_arrived.notify_one();
        }
    };

    tokio::time::sleep(Duration::from_millis(100)).await;
    println!(
        "{:>5.0?}  enqueue resize-image   (notified)",
        started.elapsed()
    );
    enqueue("resize-image", true);

    tokio::time::sleep(Duration::from_millis(100)).await;
    println!(
        "{:>5.0?}  enqueue send-receipt   (no notification)",
        started.elapsed()
    );
    enqueue("send-receipt", false);

    tokio::time::sleep(Duration::from_millis(150)).await;
    println!(
        "{:>5.0?}  enqueue refresh-cache  (notified)",
        started.elapsed()
    );
    enqueue("refresh-cache", true);

    // send-receipt was already waiting when refresh-cache's notification
    // woke the worker, so it ran early. Without that, the 500ms fallback
    // would have picked it up.
    tokio::time::sleep(Duration::from_millis(250)).await;
    println!(
        "{:>5.0?}  enqueue rebuild-index  (no notification)",
        started.elapsed()
    );
    enqueue("rebuild-index", false);

    // Nothing else will notify: rebuild-index waits for the 1000ms fallback
    // tick - the fallback interval is the worst-case delay.
    tokio::time::sleep(Duration::from_millis(550)).await;
    cancel.cancel();
    handle.await.unwrap();
}
