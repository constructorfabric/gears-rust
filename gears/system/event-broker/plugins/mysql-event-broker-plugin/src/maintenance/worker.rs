//! The maintenance worker: takes the partitions offered to it and runs
//! [`steps::run`] on each, one at a time. A partition whose run failed comes
//! back after a pause that doubles with each failure in a row, and waits
//! longer for its DDL's metadata lock each time.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use sqlx::MySqlPool;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};

use super::steps::{self, Outcome};
use crate::error;
use crate::heads::Key;
use crate::shared::Shared;

/// The pause before a failed partition's first retry.
const RETRY_FIRST: Duration = Duration::from_millis(500);
/// The longest pause between retries.
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Failures in a row after which a partition is reported at `error`.
const WARN_AFTER: u32 = 5;

/// A failure every partition shares: the database is unreachable, so the
/// worker backs off instead.
#[derive(Debug)]
pub struct MediumDown(String);

impl fmt::Display for MediumDown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "maintenance: the database is unavailable: {}", self.0)
    }
}

/// One partition waiting for a retry.
struct Retry {
    attempt: u32,
    at: Instant,
}

/// Runs the maintenance of offered partitions, retrying failed ones with
/// backoff.
pub struct Maintainer {
    shared: Arc<Shared>,
    pool: MySqlPool,
    retries: HashMap<Key, Retry>,
}

impl Maintainer {
    /// A worker over `pool` with no retry pending.
    pub fn new(shared: Arc<Shared>, pool: MySqlPool) -> Self {
        Self {
            shared,
            pool,
            retries: HashMap::new(),
        }
    }

    /// The partitions to run now, each with its attempt.
    fn due(&mut self) -> Vec<(Key, u32)> {
        let now = Instant::now();
        let mut due: Vec<(Key, u32)> = self
            .shared
            .maintenance
            .queue
            .take()
            .into_iter()
            .map(|key| (key, self.retries.remove(&key).map_or(0, |r| r.attempt)))
            .collect();
        let ready: Vec<Key> = self
            .retries
            .iter()
            .filter(|(_, retry)| retry.at <= now)
            .map(|(key, _)| *key)
            .collect();
        for key in ready {
            if let Some(retry) = self.retries.remove(&key) {
                due.push((key, retry.attempt));
            }
        }
        due
    }

    fn postpone(&mut self, key: Key, attempt: u32) {
        let pause = RETRY_FIRST
            .saturating_mul(1 << attempt.min(6))
            .min(RETRY_MAX);
        self.retries.insert(
            key,
            Retry {
                attempt: attempt + 1,
                at: Instant::now() + pause,
            },
        );
    }
}

impl WorkerAction for Maintainer {
    type Payload = ();
    type Error = MediumDown;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, MediumDown> {
        let due = self.due();
        let mut queue = due.into_iter();
        while let Some((key, attempt)) = queue.next() {
            if cancel.is_cancelled() {
                return Ok(Directive::idle());
            }
            let hub = &self.shared.maintenance;
            match steps::run(&self.shared, &self.pool, key, attempt, 0).await {
                Ok(Outcome::Done(layout)) => hub.replace(key, layout),
                // The holder does the work; the rescan offers it again if not.
                Ok(Outcome::Busy) => {}
                Ok(Outcome::Gone) => hub.forget(key),
                Err(err) if error::is_connectivity(&err) => {
                    // Every partition fails alike: keep them, back off.
                    self.postpone(key, attempt);
                    for (key, attempt) in queue {
                        self.retries.insert(
                            key,
                            Retry {
                                attempt,
                                at: Instant::now(),
                            },
                        );
                    }
                    return Err(MediumDown(error::kind(&err).to_owned()));
                }
                Err(err) => {
                    report(key, attempt, &err);
                    self.postpone(key, attempt);
                }
            }
        }
        Ok(match self.retries.values().map(|r| r.at).min() {
            Some(at) => Directive::sleep(at.saturating_duration_since(Instant::now())),
            None => Directive::idle(),
        })
    }
}

/// Logs the `attempt`-th failure in a row of `key`'s run, at `error` once it
/// keeps failing.
fn report((topic, partition): Key, attempt: u32, err: &sqlx::Error) {
    let number = error::number(err);
    let kind = error::kind(err);
    if attempt + 1 >= WARN_AFTER {
        tracing::error!(%topic, %partition, attempt, ?number, kind,
            "event-broker maintenance: a partition keeps failing");
    } else {
        tracing::warn!(%topic, %partition, attempt, ?number, kind,
            "event-broker maintenance: postponed");
    }
}
