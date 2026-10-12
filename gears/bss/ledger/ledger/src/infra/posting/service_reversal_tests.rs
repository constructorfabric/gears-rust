//! `post_reversal_once` rejections on real SQLite: a reversal that names no
//! original, an original the caller cannot see, or originals with mixed rate
//! snapshots is refused before anything is written.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::decimal_tests::{counts, setup};
use super::*;
use crate::infra::posting::retry::retry_transaction;
use crate::infra::storage::entity::journal_line;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{Db, SecureEntityExt, SecureUpdateExt};

/// Run one reversal attempt through the shared retry boundary.
async fn reverse(
    svc: &PostingService,
    db: &Db,
    scope: &AccessScope,
    reversal: NewEntry,
) -> Result<PostingRef, DomainError> {
    let svc = svc.clone();
    let scope = scope.clone();
    retry_transaction(db, move |txn| {
        let svc = svc.clone();
        let reversal = reversal.clone();
        let scope = scope.clone();
        Box::pin(async move {
            svc.post_reversal_once(
                &SecurityContext::anonymous(),
                txn,
                &scope,
                reversal,
                None,
                ClaimSpec::fresh(),
            )
            .await
        })
    })
    .await
}

/// A posted original and a reversal header pointing at it.
async fn posted_original() -> (PostingService, Db, NewEntry, NewEntry) {
    let (svc, db, entry, lines) = setup().await;
    svc.post(
        &SecurityContext::anonymous(),
        &AccessScope::allow_all(),
        entry.clone(),
        lines,
        None,
    )
    .await
    .unwrap();
    let mut reversal = entry.clone();
    reversal.entry_id = Uuid::now_v7();
    reversal.source_business_id = "reversal".to_owned();
    reversal.reverses_entry_id = Some(entry.entry_id);
    reversal.reverses_period_id = Some(entry.period_id.clone());
    (svc, db, entry, reversal)
}

#[tokio::test]
async fn a_reversal_without_original_references_is_rejected_without_writes() {
    let (svc, db, _entry, reversal) = posted_original().await;
    let before = counts(&db).await;
    let mut no_entry = reversal.clone();
    no_entry.reverses_entry_id = None;
    let mut no_period = reversal;
    no_period.reverses_period_id = None;
    for header in [no_entry, no_period] {
        let error = reverse(&svc, &db, &AccessScope::allow_all(), header)
            .await
            .unwrap_err();
        assert!(matches!(error, DomainError::InvalidRequest(_)), "{error:?}");
    }
    assert_eq!(counts(&db).await, before);
}

#[tokio::test]
async fn a_wrong_period_or_a_foreign_scope_finds_no_original_lines() {
    let (svc, db, _entry, reversal) = posted_original().await;
    let before = counts(&db).await;
    let mut wrong_period = reversal.clone();
    wrong_period.reverses_period_id = Some("202611".to_owned());
    let foreign = AccessScope::for_tenant(Uuid::now_v7());
    for (header, scope) in [
        (wrong_period, AccessScope::allow_all()),
        (reversal, foreign),
    ] {
        let error = reverse(&svc, &db, &scope, header).await.unwrap_err();
        assert!(
            matches!(&error, DomainError::InvalidRequest(d) if d.contains("no lines")),
            "{error:?}"
        );
    }
    assert_eq!(counts(&db).await, before);
}

#[tokio::test]
async fn originals_with_mixed_rate_snapshots_are_internal_without_writes() {
    let (svc, db, entry, reversal) = posted_original().await;
    // SQLite keeps no append-only trigger or FK on the line, so one line can be
    // pointed at a snapshot the other does not share.
    let one_line = journal_line::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(journal_line::Column::EntryId.eq(entry.entry_id)))
        .one(&db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    journal_line::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .col_expr(
            journal_line::Column::RateSnapshotRef,
            Expr::value(Uuid::now_v7()),
        )
        .filter(Condition::all().add(journal_line::Column::LineId.eq(one_line.line_id)))
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    let before = counts(&db).await;
    let error = reverse(&svc, &db, &AccessScope::allow_all(), reversal)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DomainError::Internal(d) if d.contains("inconsistent rate snapshots")),
        "{error:?}"
    );
    assert_eq!(counts(&db).await, before);
}
