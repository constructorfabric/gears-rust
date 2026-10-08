//! Exhausted contention answers the door's 409, injected through the retry classifier.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use toolkit::api::canonical_prelude::Problem;

async fn db() -> Db {
    toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .unwrap()
}
fn driver(message: &str) -> DoorError {
    DoorError::Repo(RepoError::Driver {
        context: "test".into(),
        source: sea_orm::DbErr::Custom(message.into()),
    })
}
/// The body fails every attempt with `error`; answer the door's error and the attempt count.
async fn run(unit: bool, error: fn() -> DoorError) -> (CanonicalError, u32) {
    let db = db().await;
    let attempts = Arc::new(AtomicU32::new(0));
    let seen = attempts.clone();
    let result = if unit {
        // The unit doors run their events' transaction (D-455), over a sink of their own.
        let (provider, _, _, _dsn) = crate::test_support::test_db().await;
        let state = crate::api::rest::authoring::AuthoringState::new(
            provider,
            Arc::new(toolkit::ClientHub::default()),
        )
        .await
        .unwrap();
        let db = state.db.db();
        retry_unit_capture(&db, || {
            unit_transaction_observed_with_events(&db, &state.outbox, |_, _| {
                seen.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Err::<(), _>(error()) })
            })
        })
        .await
        .map_err(Into::into)
    } else {
        transaction(&db, move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Err::<(), _>(error()) })
        })
        .await
    };
    (result.unwrap_err(), attempts.load(Ordering::SeqCst))
}
fn reason(error: &CanonicalError) -> Option<String> {
    match error {
        CanonicalError::Aborted { ctx, .. } => Some(ctx.reason.clone()),
        _ => None,
    }
}
#[tokio::test]
async fn contention_that_outlasts_the_retries_is_409_contended() {
    let busy = || driver("error returned from database: (code: 5) database is locked");
    let (error, attempts) = run(false, busy).await;
    assert_eq!(
        attempts,
        toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS,
        "it was retried"
    );
    assert_eq!(error.status_code(), 409);
    assert_eq!(reason(&error).as_deref(), Some("CONTENDED"));
    // Phase 4 second review B-1: the classified contention is a typed conflict, which an authoring
    // door renders exactly as before — the same 409 problem, on the price book's type.
    let body = |error: CanonicalError| serde_json::to_value(Problem::from(error)).unwrap();
    assert_eq!(body(error), body(conflict(CONTENDED)));
    let (error, _) = run(true, busy).await;
    assert_eq!(error.status_code(), 409);
    assert_eq!(reason(&error).as_deref(), Some("UNIT_CONTENDED"));
    assert_eq!(body(error), body(conflict(UNIT_CONTENDED)));
}
#[tokio::test]
async fn any_other_driver_failure_stays_a_500() {
    let broken = || driver("error returned from database: (code: 1) no such table: x");
    let (error, attempts) = run(false, broken).await;
    assert_eq!(attempts, 1, "not retried");
    assert_eq!(error.status_code(), 500);
    let (error, _) = run(true, broken).await;
    assert_eq!(error.status_code(), 500);
}
/// Exhausted contention is a typed conflict with the door's code (phase 4 second review B-1), so
/// each door names its own resource: the authoring doors through `From<DoorError>`, the read
/// doors through `read_failure`. Another backend's contention text stays a driver failure.
#[test]
fn the_classifier_follows_the_backend() {
    let pg = || driver("could not serialize access due to concurrent update");
    assert!(matches!(
        exhausted_contention(sea_orm::DbBackend::Postgres, CONTENDED, pg()),
        DoorError::Repo(RepoError::Conflict { code: CONTENDED })
    ));
    assert!(matches!(
        exhausted_contention(sea_orm::DbBackend::Sqlite, CONTENDED, pg()),
        DoorError::Repo(RepoError::Driver { .. })
    ));
}
/// The unit doors answer `UNIT_CONTENDED`; every other mutation door keeps `CONTENDED`. Each of
/// them enqueues `ApprovalUnitDecided` when it decides, so each runs the transaction that wakes
/// the outbox's sequencer after its commit (D-455).
///
/// The roster is derived from the file (PT-01): every `pub async fn` of `approvals.rs` whose body
/// records a unit (`record`, `record_prices`) or calls the engine is a unit door, and the derived
/// roster is pinned, so a new unit door cannot be skipped.
#[test]
fn every_approval_unit_door_runs_a_unit_transaction() {
    let code = crate::source_scan::blank_comments_and_literals(include_str!("approvals.rs"));
    let doors: Vec<(&str, &str)> = code
        .split("\npub async fn ")
        .skip(1)
        .map(|door| (&door[..door.find('(').unwrap()], door))
        .filter(|(_, body)| {
            ["record(", "record_prices(", "Engine::"]
                .iter()
                .any(|call| body.contains(call))
        })
        .collect();
    assert_eq!(
        doors.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["submit_price", "submit_revision", "publish", "vote"],
        "the unit doors of approvals.rs"
    );
    for (door, body) in doors {
        assert!(
            body.contains("support::unit_transaction"),
            "{door} must run a unit transaction"
        );
        assert!(
            body.contains("_with_events("),
            "{door} must carry its events' wakes to the commit"
        );
        assert!(
            !body.contains("support::transaction(") && !body.contains("support::transaction_door("),
            "{door} must not run a plain transaction"
        );
    }
}
#[test]
fn a_forbidden_answer_keeps_its_code_and_names_its_cause() {
    let error = forbidden_because("NOT_DRAFT_AUTHOR", "price 42 is a draft of another author");
    assert_eq!(error.status_code(), 403);
    assert_eq!(error.detail(), "price 42 is a draft of another author");
    let problem = toolkit::api::canonical_prelude::Problem::from(error);
    assert_eq!(problem.context["reason"], "NOT_DRAFT_AUTHOR");
    assert_eq!(
        problem.context["resource_type"],
        Problem::from(forbidden("NOT_DRAFT_AUTHOR")).context["resource_type"]
    );
}
/// Rename L1: the subject's entry lookup names its own code, and the door answers it as a
/// missing entry, never as a missing price.
#[test]
fn a_subject_entry_not_found_is_a_missing_entry() {
    let problem = |error: DoorError| Problem::from(CanonicalError::from(error));
    let entry = problem(approval_failure(
        bss_approval::ApprovalError::InvalidSubmit {
            code: "ENTRY_NOT_FOUND",
            field: "price".into(),
            detail: "entry 1".into(),
        },
    ));
    assert_eq!(entry.status, Some(404));
    assert_eq!(entry.context["resource_name"], "price_book_entry");
    assert!(entry.detail.starts_with("ENTRY_NOT_FOUND"), "{entry:?}");
    let price = problem(approval_failure(
        bss_approval::ApprovalError::InvalidSubmit {
            code: "PRICE_NOT_FOUND",
            field: "price".into(),
            detail: "price 1".into(),
        },
    ));
    assert_eq!(price.status, Some(404));
    assert_eq!(price.context["resource_name"], "price");
}
