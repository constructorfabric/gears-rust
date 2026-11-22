//! A long-lived thread that runs jobs one at a time from a bounded queue.
//!
//! The thread is not one of Tokio's blocking-pool threads: the runtime never
//! joins it, so a job that does not return (a pdfium call cannot be
//! interrupted) holds up neither runtime shutdown nor process exit. Callers
//! wait asynchronously, so a queued job costs no thread.

use std::panic::{AssertUnwindSafe, catch_unwind};

use tokio::sync::mpsc::{self, error::TrySendError};

/// The sending side of a worker thread.
pub(super) struct Worker<J> {
    jobs: mpsc::Sender<J>,
}

/// Why a job was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Submit {
    /// `capacity` jobs already wait.
    Full,
    /// The thread has stopped.
    Gone,
}

impl<J: Send + 'static> Worker<J> {
    /// Starts thread `name`, which hands each job to `run` in order. At most
    /// `capacity` jobs wait besides the one running.
    pub(super) fn spawn(
        name: &str,
        capacity: usize,
        mut run: impl FnMut(J) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (jobs, mut queue) = mpsc::channel(capacity);
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Some(job) = queue.blocking_recv() {
                    // A job reports its own failures; a panic must not end
                    // the thread for every job after it.
                    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| run(job))) {
                        let what = payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_owned())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_default();
                        tracing::error!(
                            worker = std::thread::current().name().unwrap_or(""),
                            panic = %what,
                            "worker job panicked"
                        );
                    }
                }
            })?;
        // Detached: nothing waits for the thread.
        drop(thread);
        Ok(Self { jobs })
    }

    /// Queues `job` without waiting.
    pub(super) fn submit(&self, job: J) -> Result<(), Submit> {
        self.jobs.try_send(job).map_err(|e| match e {
            TrySendError::Full(_) => Submit::Full,
            TrySendError::Closed(_) => Submit::Gone,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::sync::mpsc as std_mpsc;
    use std::time::Duration;

    use super::*;

    enum Job {
        /// Signals `started`, then blocks until `release` is sent or dropped.
        Hold {
            started: std_mpsc::Sender<()>,
            release: std_mpsc::Receiver<()>,
        },
        Record(u32),
        Panic,
    }

    fn worker(capacity: usize) -> (Worker<Job>, std_mpsc::Receiver<u32>) {
        let (done, seen) = std_mpsc::channel();
        let worker = Worker::spawn("test-worker", capacity, move |job| match job {
            Job::Hold { started, release } => {
                started.send(()).expect("the test waits");
                release.recv().unwrap_or_default();
            }
            Job::Record(n) => drop(done.send(n)),
            Job::Panic => panic!("job panics"),
        })
        .expect("thread starts");
        (worker, seen)
    }

    /// Holds the worker busy; the job is running, not queued, on return.
    fn hold(worker: &Worker<Job>) -> std_mpsc::Sender<()> {
        let (started, running) = std_mpsc::channel();
        let (release, held) = std_mpsc::channel();
        worker
            .submit(Job::Hold {
                started,
                release: held,
            })
            .expect("queued");
        running.recv().expect("hold job runs");
        release
    }

    #[test]
    fn a_full_queue_refuses_and_frees_up() {
        let (worker, seen) = worker(2);
        let release = hold(&worker);
        assert_eq!(worker.submit(Job::Record(1)), Ok(()));
        assert_eq!(worker.submit(Job::Record(2)), Ok(()));
        assert_eq!(worker.submit(Job::Record(3)), Err(Submit::Full));
        drop(release);
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)), Ok(1));
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)), Ok(2));
        assert_eq!(worker.submit(Job::Record(4)), Ok(()));
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)), Ok(4));
    }

    #[test]
    fn a_panicking_job_does_not_stop_the_thread() {
        let (worker, seen) = worker(4);
        worker.submit(Job::Panic).expect("queued");
        worker.submit(Job::Record(7)).expect("queued");
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)), Ok(7));
    }

    #[test]
    fn a_stopped_thread_is_reported() {
        let (jobs, queue) = mpsc::channel::<Job>(1);
        drop(queue);
        let worker = Worker { jobs };
        assert_eq!(worker.submit(Job::Record(1)), Err(Submit::Gone));
    }
}
