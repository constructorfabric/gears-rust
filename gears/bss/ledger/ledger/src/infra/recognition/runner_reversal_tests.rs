//! Runner paths with no prior coverage: reversing a segment that was never
//! released, and the invariant alarm the runner (not the core post path) emits
//! when a release is refused by the over-recognition cap.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::decimal_tests::fixture;
use super::*;
use crate::infra::storage::entity::{
    account_balance, chain_state, idempotency_dedup, journal_entry, journal_line,
    recognition_schedule, recognition_segment,
};
use sea_orm::EntityTrait;
use toolkit_db::secure::{Db, SecureEntityExt, SecureUpdateExt};

/// Every posting and recognition row, rendered for an exact before/after compare.
async fn snapshot(db: &Db) -> String {
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let mut rows = Vec::new();
    macro_rules! collect {
        ($entity:ident) => {
            for row in $entity::Entity::find()
                .secure()
                .scope_with(&scope)
                .all(&conn)
                .await
                .unwrap()
            {
                rows.push(format!("{}:{row:?}", stringify!($entity)));
            }
        };
    }
    collect!(account_balance);
    collect!(chain_state);
    collect!(idempotency_dedup);
    collect!(journal_entry);
    collect!(journal_line);
    collect!(recognition_schedule);
    collect!(recognition_segment);
    rows.sort();
    rows.join("\n")
}

/// A reversal of a segment that was never released has no original claim: it is
/// a policy conflict and writes nothing.
#[tokio::test]
async fn reversing_an_unreleased_segment_is_a_policy_conflict_with_no_effects() {
    let (db, runner, tenant, candidate) = fixture().await;
    let before = snapshot(&db).await;
    let result = runner
        .release_reversal(
            &SecurityContext::anonymous(),
            &AccessScope::allow_all(),
            tenant,
            &candidate,
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::RecognitionPolicyConflict(_))),
        "{result:?}"
    );
    assert_eq!(before, snapshot(&db).await);
}

/// The runner is the only source of the recognition invariant alarms: a release
/// refused by the over-recognition cap raises exactly one alarm, and a clean
/// release raises none.
#[tokio::test]
#[cfg(feature = "test-support")]
async fn over_recognition_raises_one_invariant_alarm_and_a_clean_release_none() {
    let metrics = crate::infra::metrics::test_harness::MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();

    let (_db, mut clean, tenant, candidate) = fixture().await;
    clean.publisher = Arc::new(LedgerEventPublisher::with_metrics(Arc::new(
        metrics.metrics(),
    )));
    clean
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    let category = crate::infra::events::payloads::AlarmCategory::OverRecognition;
    let severity = crate::infra::events::alarm_catalog::severity(category);
    let labels = [
        ("category", category.as_str()),
        ("severity", severity.as_str()),
    ];
    metrics.force_flush();
    assert_eq!(metrics.counter_value("ledger_alarm_total", &labels), 0);

    let (db, mut capped, tenant, candidate) = fixture().await;
    capped.publisher = Arc::new(LedgerEventPublisher::with_metrics(Arc::new(
        metrics.metrics(),
    )));
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_schedule::Column::TotalDeferred,
            sea_orm::sea_query::Expr::value("0.5"),
        )
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        capped
            .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
            .await,
        Err(DomainError::OverRecognition(_))
    ));
    metrics.force_flush();
    assert_eq!(metrics.counter_value("ledger_alarm_total", &labels), 1);
}
