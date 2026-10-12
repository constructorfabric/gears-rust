//! Actual orchestration status, lease and summary tests on migrated SQLite.
use super::super::runner::decimal_tests::fixture;
use super::*;
use crate::domain::ports::metrics::NoopLedgerMetrics;

#[tokio::test]
async fn trigger_brackets_actual_runner_and_same_key_replays_without_releases() {
    let (db, _, tenant, _) = fixture().await;
    let service = RecognitionRunService::new(
        DBProvider::new(db),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(NoopLedgerMetrics),
    );
    let run = Uuid::now_v7();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let first = service
        .trigger(&ctx, &scope, tenant, "202610", Some(run))
        .await
        .unwrap();
    assert!(matches!(
        first,
        RecognitionRunOutcome::Ran(RecognitionRunRef {
            released: 1,
            replayed: false,
            ..
        })
    ));
    let row = service
        .recognition
        .read_run(&scope, tenant, "202610", run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "DONE");
    let replay = service
        .trigger(&ctx, &scope, tenant, "202610", Some(run))
        .await
        .unwrap();
    assert!(matches!(
        replay,
        RecognitionRunOutcome::Ran(RecognitionRunRef {
            released: 0,
            replayed: true,
            ..
        })
    ));
    let guard = service
        .lease
        .acquire(
            &format!("recognition-run:{tenant}:202610"),
            RECOGNITION_LEASE_TTL,
        )
        .await
        .unwrap();
    let held = service
        .trigger(&ctx, &scope, tenant, "202610", Some(Uuid::now_v7()))
        .await
        .unwrap();
    assert!(matches!(
        held,
        RecognitionRunOutcome::Ran(RecognitionRunRef {
            released: 0,
            replayed: true,
            ..
        })
    ));
    guard.release().await.unwrap();
}

#[tokio::test]
async fn failed_release_finishes_failed_and_releases_lease() {
    use crate::infra::storage::entity::recognition_schedule;
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureUpdateExt;
    let (db, _, tenant, _) = fixture().await;
    let scope = AccessScope::allow_all();
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
    let service = RecognitionRunService::new(
        DBProvider::new(db),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(NoopLedgerMetrics),
    );
    let run = Uuid::now_v7();
    assert!(matches!(
        service
            .trigger(
                &SecurityContext::anonymous(),
                &scope,
                tenant,
                "202610",
                Some(run)
            )
            .await,
        Err(DomainError::OverRecognition(_))
    ));
    assert_eq!(
        service
            .recognition
            .read_run(&scope, tenant, "202610", run)
            .await
            .unwrap()
            .unwrap()
            .status,
        "FAILED"
    );
    service
        .lease
        .acquire(
            &format!("recognition-run:{tenant}:202610"),
            RECOGNITION_LEASE_TTL,
        )
        .await
        .unwrap()
        .release()
        .await
        .unwrap();
}
