//! What a failed run means for its key (`docs/DESIGN.md` §4.9): the error's
//! [`Class`], which each worker turns into how it settles its lease, and the
//! log line it leaves.

use std::fmt;
use std::time::Duration;

use super::prioritizer::Key;

/// A [`Class::Medium`] failure, handed to the worker so it backs off.
#[derive(Debug)]
pub struct MediumDown {
    job: &'static str,
    error: String,
}

impl MediumDown {
    /// `job` found the database down with `err`.
    pub fn new(job: &'static str, err: &sqlx::Error) -> Self {
        Self {
            job,
            error: err.to_string(),
        }
    }
}

impl fmt::Display for MediumDown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: the database is unavailable: {}",
            self.job, self.error
        )
    }
}

/// What kind of failure an error is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The database is unreachable or refusing everyone: every key fails
    /// alike, so the worker backs off instead of the key.
    Medium,
    /// Another transaction held what the job needed.
    Contended,
    /// The partition's table is gone.
    Gone,
    /// The catalog is ahead of what the job read, on a key's first attempt:
    /// a retry reads it again. On a later attempt it is a [`Class::Bug`].
    Stale,
    /// The run failed, and may succeed later.
    Failed(Loudness),
    /// The plugin's own state does not add up: a new bound the rows break,
    /// or a stale catalog seen twice in a row.
    Bug,
    /// Only an operator fixes it: privileges, schema, credentials.
    Permanent,
}

/// How loudly a [`Class::Failed`] is logged each time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loudness {
    /// `debug`: the queue reports it once it repeats.
    Quiet,
    /// `warn`: worth a look each time.
    Warn,
    /// `error`: needs an operator or a fix.
    Error,
}

/// The class of `err` on a run after `attempt` postponements of its key.
pub fn class(err: &sqlx::Error, attempt: u32) -> Class {
    match class_of(err) {
        Class::Stale if attempt > 0 => Class::Bug,
        class => class,
    }
}

fn class_of(err: &sqlx::Error) -> Class {
    match err {
        sqlx::Error::Database(db) => db.code().map_or(Class::Failed(Loudness::Warn), |code| {
            class_of_sqlstate(&code)
        }),
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => Class::Medium,
        _ => Class::Failed(Loudness::Warn),
    }
}

/// The class of a `PostgreSQL` SQLSTATE. `55P03` on the retention job's
/// `NOWAIT` row lock means another instance is on the partition; that job
/// answers `Busy` itself and never hands it here.
pub fn class_of_sqlstate(code: &str) -> Class {
    match code {
        // admin shutdown, crash shutdown, cannot connect now, too many
        // connections, read-only transaction (a failover to a standby)
        "57P01" | "57P02" | "57P03" | "53300" | "25006" => Class::Medium,
        // lock_not_available, deadlock_detected, serialization_failure
        "55P03" | "40P01" | "40001" => Class::Contended,
        // query_canceled (statement_timeout)
        "57014" => Class::Failed(Loudness::Quiet),
        // idle_in_transaction_session_timeout, out of memory, too complex
        "25P03" | "53200" | "54001" => Class::Failed(Loudness::Warn),
        // disk_full
        "53100" => Class::Failed(Loudness::Error),
        // undefined_table
        "42P01" => Class::Gone,
        // duplicate_table, invalid_object_definition (overlapping ranges)
        "42P07" | "42P17" => Class::Stale,
        // check_violation: rows outside a new bound
        "23514" => Class::Bug,
        // insufficient_privilege, invalid_schema_name, invalid_catalog_name
        "42501" | "3F000" | "3D000" => Class::Permanent,
        code if code.starts_with("08") => Class::Medium,
        code if code.starts_with("28") => Class::Permanent,
        _ => Class::Failed(Loudness::Warn),
    }
}

/// Logs `job`'s failure on `key` at `loudness`.
#[allow(
    clippy::cognitive_complexity,
    reason = "three one-line tracing macros, one per level; each expansion counts as branches"
)]
pub fn log(job: &'static str, (topic, partition): Key, err: &sqlx::Error, loudness: Loudness) {
    match loudness {
        Loudness::Quiet => {
            tracing::debug!(job, %topic, %partition, error = %err, "event-broker maintenance: postponed");
        }
        Loudness::Warn => {
            tracing::warn!(job, %topic, %partition, error = %err, "event-broker maintenance: failed");
        }
        Loudness::Error => {
            tracing::error!(job, %topic, %partition, error = %err, "event-broker maintenance: failed");
        }
    }
}

/// The `lock_timeout` of a run after `attempt` postponements: waits longer
/// each time, so a busy partition is still reached.
pub fn lock_timeout_for(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::from_millis(500),
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(2),
        _ => Duration::from_secs(5),
    }
}
