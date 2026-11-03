//! Tests for the parts of the service that are not reachable through a
//! repository or the API: the liveness gate that decides whether one process
//! may take another's sync lock.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use super::{ABANDONED_AFTER_SECS, SyncJob, abandoned, telemetry_file_path};
use crate::domain::ports::github::ForceMode;
use crate::domain::repo::{SessionStatus, SyncSessionRecord};
use crate::domain::scope::ScopeConfig;

fn at(seconds_ago: i64, now: DateTime<Utc>) -> String {
    (now - Duration::seconds(seconds_ago)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn session(created_at: String) -> SyncSessionRecord {
    SyncSessionRecord {
        id: Uuid::new_v4(),
        repo_full_name: "acme/widget".to_owned(),
        repo_id: None,
        status: SessionStatus::InProgress,
        progress_percent: 0,
        error: None,
        summary_json: None,
        telemetry_json: None,
        created_at,
        started_at: None,
        ended_at: None,
        updated_at: None,
    }
}

/// The margin is a boundary, not a range: one second short of it the holder
/// still owns its lock, one second past it the lock can be taken.
#[test]
fn the_margin_is_exact() {
    let now = Utc::now();

    let mut just_alive = session(at(ABANDONED_AFTER_SECS, now));
    just_alive.updated_at = Some(at(ABANDONED_AFTER_SECS, now));
    assert!(
        !abandoned(&just_alive, now),
        "exactly at the margin is still alive"
    );

    let mut just_gone = session(at(ABANDONED_AFTER_SECS + 1, now));
    just_gone.updated_at = Some(at(ABANDONED_AFTER_SECS + 1, now));
    assert!(abandoned(&just_gone, now), "one second past it is not");
}

/// A stamp from the future is what a host whose clock runs ahead writes. The
/// subtraction goes negative, which is under the margin, so the holder keeps
/// its lock: a skewed clock must never cost a live run its repository.
#[test]
fn a_stamp_from_the_future_counts_as_alive() {
    let now = Utc::now();
    let mut skewed = session(at(0, now));
    skewed.updated_at = Some(at(-3600, now));

    assert!(!abandoned(&skewed, now));
}

/// A stamp nothing can read says nothing about the process behind it, so it
/// is read as alive. The lock stays where it is and an operator sees the row
/// rather than two processes syncing one repository.
#[test]
fn an_unreadable_stamp_counts_as_alive() {
    let now = Utc::now();

    let mut unreadable = session(at(ABANDONED_AFTER_SECS * 10, now));
    unreadable.updated_at = Some("not a date".to_owned());
    assert!(!abandoned(&unreadable, now));

    let empty = session(String::new());
    assert!(!abandoned(&empty, now));
}

/// Which stamp is read: the heartbeat first, then the run's start, then the
/// moment it was queued. A session that never started is judged on the time
/// it was created, or a queue nobody drains would hold its lock for ever.
#[test]
fn the_first_stamp_the_row_has_is_the_one_read() {
    let now = Utc::now();

    let mut heartbeat_wins = session(at(ABANDONED_AFTER_SECS * 10, now));
    heartbeat_wins.started_at = Some(at(ABANDONED_AFTER_SECS * 5, now));
    heartbeat_wins.updated_at = Some(at(1, now));
    assert!(
        !abandoned(&heartbeat_wins, now),
        "a fresh heartbeat outranks an old creation stamp"
    );

    let mut started_wins = session(at(ABANDONED_AFTER_SECS * 10, now));
    started_wins.started_at = Some(at(1, now));
    assert!(
        !abandoned(&started_wins, now),
        "with no heartbeat yet, the start of the run is what counts"
    );

    let queued_long_ago = session(at(ABANDONED_AFTER_SECS + 1, now));
    assert!(
        abandoned(&queued_long_ago, now),
        "a session that never started is judged on when it was queued"
    );
}

#[test]
fn an_unreadable_heartbeat_is_not_passed_over_for_an_older_start() {
    let now = Utc::now();
    let mut row = session(at(ABANDONED_AFTER_SECS * 10, now));
    row.started_at = Some(at(ABANDONED_AFTER_SECS * 5, now));
    row.updated_at = Some("not a date".to_owned());

    assert!(
        !abandoned(&row, now),
        "the heartbeat is present, so it is the stamp read, and an unreadable one counts as alive"
    );
}

fn job(telemetry_file: Option<&str>) -> SyncJob {
    SyncJob {
        session_id: Uuid::new_v4(),
        ctx: toolkit_security::SecurityContext::anonymous(),
        owner: "rust-lang".to_owned(),
        name: "rust".to_owned(),
        scope: ScopeConfig::default(),
        force: ForceMode::None,
        since: None,
        telemetry_file: telemetry_file.map(ToOwned::to_owned),
        max_concurrent_tasks: None,
        access_scope: toolkit_security::AccessScope::default(),
        claim: None,
    }
}

#[test]
fn the_telemetry_file_defaults_to_the_repository_name_under_its_tenant_and_owner() {
    let tenant = Uuid::new_v4();

    let path = telemetry_file_path(Path::new("/var/lib/gm"), tenant, &job(None));

    assert_eq!(
        path,
        PathBuf::from("/var/lib/gm")
            .join(tenant.to_string())
            .join("rust-lang")
            .join("rust.jsonl")
    );
}

#[test]
fn a_caller_named_telemetry_file_stays_in_the_same_folder() {
    let tenant = Uuid::new_v4();

    let path = telemetry_file_path(
        Path::new("/var/lib/gm"),
        tenant,
        &job(Some("rust-run.jsonl")),
    );

    assert_eq!(
        path,
        PathBuf::from("/var/lib/gm")
            .join(tenant.to_string())
            .join("rust-lang")
            .join("rust-run.jsonl")
    );
}

#[test]
fn two_tenants_never_share_a_telemetry_file() {
    let dir = Path::new("/var/lib/gm");

    assert_ne!(
        telemetry_file_path(dir, Uuid::new_v4(), &job(None)),
        telemetry_file_path(dir, Uuid::new_v4(), &job(None))
    );
}
