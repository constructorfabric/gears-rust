//! Exhausted contention answers 409, injected through the toolkit's retry classifier.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

fn reason(error: &CanonicalError) -> Option<String> {
    match error {
        CanonicalError::Aborted { ctx, .. } => Some(ctx.reason.clone()),
        _ => None,
    }
}
fn driver(message: &str) -> TxError {
    TxError::Repo(RepoError::Driver {
        context: "test".into(),
        source: sea_orm::DbErr::Custom(message.into()),
    })
}
const SQLITE_BUSY: &str = "error returned from database: (code: 5) database is locked";
const PG_SERIALIZATION: &str = "could not serialize access due to concurrent update";

#[tokio::test]
async fn contention_that_outlasts_the_retries_is_409_not_500() {
    let db = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .unwrap();
    let attempts = Arc::new(AtomicU32::new(0));
    let seen = attempts.clone();
    let error = db
        .transaction_with_retry(
            toolkit_db::secure::TxConfig::default(),
            contention_db_err,
            move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Err::<(), _>(driver(SQLITE_BUSY)) })
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS,
        "the classifier retried it"
    );
    let error = tx_to_canonical(error);
    assert_eq!(error.status_code(), 409);
    assert_eq!(reason(&error).as_deref(), Some("CONTENDED"));
}

#[test]
fn every_door_names_its_contention_code() {
    for (error, unit, code) in [
        (driver(SQLITE_BUSY), false, "CONTENDED"),
        (driver(PG_SERIALIZATION), false, "CONTENDED"),
        (driver(SQLITE_BUSY), true, "UNIT_CONTENDED"),
        (
            TxError::ApprovalDb(sea_orm::DbErr::Custom(PG_SERIALIZATION.into())),
            true,
            "UNIT_CONTENDED",
        ),
        (
            TxError::ApprovalDb(sea_orm::DbErr::Custom(SQLITE_BUSY.into())),
            false,
            "CONTENDED",
        ),
    ] {
        let error = if unit {
            unit_tx_to_canonical(error)
        } else {
            tx_to_canonical(error)
        };
        assert_eq!(error.status_code(), 409, "{code}");
        assert_eq!(reason(&error).as_deref(), Some(code));
    }
    let other = tx_to_canonical(driver(
        "error returned from database: (code: 1) no such table",
    ));
    assert_eq!(
        other.status_code(),
        500,
        "only contention becomes a conflict"
    );
}

/// The approval-unit doors answer `UNIT_CONTENDED`; every other door keeps `CONTENDED`.
#[test]
fn the_approval_unit_doors_map_their_transactions_as_unit_doors() {
    for (source, door) in [
        (include_str!("rest/approval_units.rs"), "async fn vote("),
        (include_str!("rest/sku_governance.rs"), "async fn execute("),
    ] {
        let start = source.find(door).unwrap();
        let rest = &source[start + door.len()..];
        let body = &rest[..rest.find("\nasync fn ").unwrap_or(rest.len())];
        assert!(
            body.contains("Err(e) => Err(unit_tx_to_canonical(e))"),
            "{door} must answer UNIT_CONTENDED"
        );
    }
}
