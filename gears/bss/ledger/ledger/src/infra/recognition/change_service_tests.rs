//! Migrated SQLite, real service/repository/claim source; no broker delivery proof.
use super::*;
use crate::infra::posting::retry::retry_transaction;
use crate::infra::posting::service::decimal_tests::{money, setup};
use crate::infra::storage::{
    entity::{idempotency_dedup, recognition_schedule, recognition_segment},
    repo::recognition_repo::NewSchedule,
};
use sea_orm::EntityTrait;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_db::secure::{Db, SecureEntityExt};

async fn fixture() -> (Db, RecognitionChangeService, RecognitionRepo, Uuid) {
    let (_, db, entry, _) = setup().await;
    seeded_fixture(db, entry.tenant_id).await
}

async fn seeded_fixture(
    db: Db,
    tenant: Uuid,
) -> (Db, RecognitionChangeService, RecognitionRepo, Uuid) {
    let provider = DBProvider::new(db.clone());
    let repo = RecognitionRepo::new(provider.clone());
    retry_transaction(&db, |txn| {
        let repo = repo.clone();
        Box::pin(async move {
            repo.insert_schedule(
                txn,
                &AccessScope::allow_all(),
                &NewSchedule {
                    tenant_id: tenant,
                    schedule_id: "original".into(),
                    payer_tenant_id: tenant,
                    source_invoice_id: "invoice".into(),
                    source_invoice_item_ref: "item".into(),
                    po_allocation_group: Some("po".into()),
                    subscription_ref: Some("subscription".into()),
                    revenue_stream: "test".into(),
                    total_deferred: money("3", "EUR", 2),
                    policy_ref: "straight-line".into(),
                    ssp_snapshot_ref: Some("ssp".into()),
                    vc_estimate_ref: Some("vc".into()),
                    vc_method_ref: Some("method".into()),
                },
            )
            .await?;
            repo.insert_segments(
                txn,
                &AccessScope::allow_all(),
                &[
                    NewSegment {
                        tenant_id: tenant,
                        schedule_id: "original".into(),
                        segment_no: 1,
                        period_id: "202610".into(),
                        amount: money("1", "EUR", 2),
                    },
                    NewSegment {
                        tenant_id: tenant,
                        schedule_id: "original".into(),
                        segment_no: 2,
                        period_id: "202611".into(),
                        amount: money("2", "EUR", 2),
                    },
                ],
            )
            .await?;
            repo.add_recognized(
                txn,
                &AccessScope::allow_all(),
                tenant,
                "original",
                &money("1", "EUR", 2),
            )
            .await?;
            repo.stamp_segment_done(
                txn,
                &AccessScope::allow_all(),
                tenant,
                "original",
                1,
                Uuid::now_v7(),
                time::OffsetDateTime::now_utc(),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let service = RecognitionChangeService::new(provider, Arc::new(LedgerEventPublisher::noop()));
    (db, service, repo, tenant)
}
fn command(tenant: Uuid) -> ChangeRecognitionSchedule {
    ChangeRecognitionSchedule {
        tenant_id: tenant,
        schedule_id: "original".into(),
        change_id: "change".into(),
        action: "replace".into(),
        treatment: "prospective".into(),
        new_segments: Some(vec![
            ChangeSegment {
                period_id: "202611".into(),
                money: money("1", "EUR", 2),
            },
            ChangeSegment {
                period_id: "202612".into(),
                money: money("1", "EUR", 2),
            },
        ]),
    }
}
async fn counts(db: &Db) -> (u64, u64, u64) {
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    (
        recognition_schedule::Entity::find()
            .secure()
            .scope_with(&scope)
            .count(&conn)
            .await
            .unwrap(),
        recognition_segment::Entity::find()
            .secure()
            .scope_with(&scope)
            .count(&conn)
            .await
            .unwrap(),
        idempotency_dedup::Entity::find()
            .secure()
            .scope_with(&scope)
            .count(&conn)
            .await
            .unwrap(),
    )
}
async fn change(
    service: &RecognitionChangeService,
    cmd: ChangeRecognitionSchedule,
) -> Result<ScheduleChangeRef, DomainError> {
    service
        .change(
            &SecurityContext::anonymous(),
            &AccessScope::allow_all(),
            cmd,
        )
        .await
}

#[tokio::test]
async fn partial_replace_preserves_economics_dimensions_periods_and_replays_canonical_spelling() {
    let (db, service, repo, tenant) = fixture().await;
    let cmd = command(tenant);
    let first = change(&service, cmd.clone()).await.unwrap();
    let id = first.new_schedule_id.clone().unwrap();
    let next = repo
        .read_schedule(&AccessScope::allow_all(), tenant, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.total_deferred, money("2", "EUR", 2));
    assert_eq!(next.recognized, money("0", "EUR", 2));
    assert_eq!(next.po_allocation_group.as_deref(), Some("po"));
    assert_eq!(next.subscription_ref.as_deref(), Some("subscription"));
    assert_eq!(next.policy_ref, "straight-line");
    assert_eq!(next.ssp_snapshot_ref.as_deref(), Some("ssp"));
    assert_eq!(next.vc_estimate_ref.as_deref(), Some("vc"));
    assert_eq!(next.vc_method_ref.as_deref(), Some("method"));
    assert_eq!(next.source_invoice_id, "invoice");
    assert_eq!(next.source_invoice_item_ref, "item");
    assert_eq!(next.payer_tenant_id, tenant);
    let old_segments = repo
        .list_segments(&AccessScope::allow_all(), tenant, "original")
        .await
        .unwrap();
    assert_eq!(old_segments[0].status, "DONE");
    assert_eq!(old_segments[0].amount, money("1", "EUR", 2));
    let segments = repo
        .list_segments(&AccessScope::allow_all(), tenant, &id)
        .await
        .unwrap();
    assert_eq!(
        segments
            .iter()
            .map(|s| s.period_id.as_str())
            .collect::<Vec<_>>(),
        ["202611", "202612"]
    );
    let mut equivalent = cmd.clone();
    equivalent.new_segments.as_mut().unwrap()[0].money = money("1.00", "EUR", 2);
    assert_eq!(change(&service, equivalent).await.unwrap(), first);
    for kind in 0..6 {
        let mut modified = cmd.clone();
        match kind {
            0 => modified.new_segments.as_mut().unwrap()[0].money = money("1.01", "EUR", 2),
            1 => modified.new_segments.as_mut().unwrap()[0].money = money("1", "USD", 2),
            2 => modified.new_segments.as_mut().unwrap()[0].money = money("1", "EUR", 3),
            3 => modified.treatment = "separate_contract".into(),
            4 => modified.action = "cancel".into(),
            _ => modified.new_segments.as_mut().unwrap().reverse(),
        }
        assert!(matches!(
            change(&service, modified).await,
            Err(DomainError::IdempotencyConflict(_))
        ));
    }
    assert_eq!(counts(&db).await, (2, 4, 1));
    assert_eq!(
        crate::infra::posting::service::decimal_tests::counts(&db)
            .await
            .0,
        0
    );
}

#[tokio::test]
async fn cancel_keeps_liability_and_treatment_gate_has_no_claim() {
    let (db, service, repo, tenant) = fixture().await;
    let mut cmd = command(tenant);
    cmd.action = "cancel".into();
    cmd.new_segments = None;
    cmd.treatment = "catch_up".into();
    assert!(matches!(
        change(&service, cmd.clone()).await,
        Err(DomainError::ModificationTreatmentReview(_))
    ));
    assert_eq!(counts(&db).await, (1, 2, 0));
    cmd.treatment = "separate_contract".into();
    let result = change(&service, cmd.clone()).await.unwrap();
    assert_eq!(change(&service, cmd).await.unwrap(), result);
    let old = repo
        .read_schedule(&AccessScope::allow_all(), tenant, "original")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.status, "CANCELLED");
    assert_eq!(old.total_deferred, money("3", "EUR", 2));
    assert_eq!(old.recognized, money("1", "EUR", 2));
    assert_eq!(
        crate::infra::posting::service::decimal_tests::counts(&db)
            .await
            .0,
        0
    );
}

#[tokio::test]
async fn malformed_period_floor_sum_negative_shape_and_zero_metadata_reject_without_effects() {
    let (db, service, _, tenant) = fixture().await;
    for period in [
        "202610",
        "2026+1",
        "+12301",
        "-00101",
        "202613",
        "２０２６１１",
    ] {
        let mut cmd = command(tenant);
        cmd.new_segments.as_mut().unwrap()[0].period_id = period.into();
        assert!(
            matches!(
                change(&service, cmd).await,
                Err(DomainError::InvalidRequest(_))
            ),
            "{period}"
        );
    }
    for kind in 0..7 {
        let mut cmd = command(tenant);
        match kind {
            0 => cmd.new_segments = None,
            1 => cmd.new_segments = Some(vec![]),
            2 => cmd.new_segments.as_mut().unwrap()[0].money = money("-1", "EUR", 2),
            3 => cmd.new_segments.as_mut().unwrap()[0].money = money("0.99", "EUR", 2),
            4 => cmd.new_segments.as_mut().unwrap()[1].period_id = "202611".into(),
            5 => cmd.new_segments.as_mut().unwrap()[0].money = money("0", "USD", 2),
            _ => cmd.new_segments.as_mut().unwrap()[0].money = money("0", "EUR", 3),
        }
        let error = change(&service, cmd).await.unwrap_err();
        assert!(matches!(
            (kind, error),
            (5, DomainError::CurrencyMismatch(_))
                | (6, DomainError::InconsistentScale(_))
                | (0..=4, DomainError::InvalidRequest(_))
        ));
    }
    assert_eq!(counts(&db).await, (1, 2, 0));
    assert!(is_well_formed_period("000001"));
    assert!(is_well_formed_period("202612"));
}

#[tokio::test]
async fn late_failure_rolls_back_claim_status_successor_and_all_segments() {
    let (db, service, repo, tenant) = fixture().await;
    let result: Result<(), DomainError> = retry_transaction(&db, |txn| {
        let cmd = command(tenant);
        let repo = repo.clone();
        let publisher = service.publisher.clone();
        Box::pin(async move {
            change_in_txn(
                txn,
                &repo,
                &IdempotencyGate::new(),
                &publisher,
                &SecurityContext::anonymous(),
                &AccessScope::allow_all(),
                &cmd,
                ChangeAction::Replace,
            )
            .await?;
            Err(DomainError::InvalidRequest("late rejection after segment insert".into()).into())
        })
    })
    .await;
    assert!(matches!(result, Err(DomainError::InvalidRequest(_))));
    assert_eq!(counts(&db).await, (1, 2, 0));
    assert_eq!(
        repo.read_schedule(&AccessScope::allow_all(), tenant, "original")
            .await
            .unwrap()
            .unwrap()
            .status,
        "ACTIVE"
    );
    assert!(change(&service, command(tenant)).await.is_ok());
}

#[tokio::test]
async fn deterministic_conflict_restarts_body_and_refreshes_remaining_not_stale_success() {
    let (db, service, repo, tenant) = fixture().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let result = retry_transaction(&db, |txn| {
        let cmd = command(tenant);
        let repo = repo.clone();
        let publisher = service.publisher.clone();
        let attempts = attempts.clone();
        Box::pin(async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 1 {
                repo.add_recognized(
                    txn,
                    &AccessScope::allow_all(),
                    tenant,
                    "original",
                    &money("1", "EUR", 2),
                )
                .await?;
            }
            let result = change_in_txn(
                txn,
                &repo,
                &IdempotencyGate::new(),
                &publisher,
                &SecurityContext::anonymous(),
                &AccessScope::allow_all(),
                &cmd,
                ChangeAction::Replace,
            )
            .await?;
            if attempt == 0 {
                return Err(AttemptError::Conflict);
            }
            Ok(result)
        })
    })
    .await;
    assert!(matches!(result, Err(DomainError::InvalidRequest(_))));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(counts(&db).await, (1, 2, 0));
    assert_eq!(
        repo.read_schedule(&AccessScope::allow_all(), tenant, "original")
            .await
            .unwrap()
            .unwrap()
            .recognized,
        money("1", "EUR", 2)
    );
}

#[tokio::test]
async fn cross_version_done_floor_survives_another_replacement() {
    let (_, service, _, tenant) = fixture().await;
    let first = change(&service, command(tenant)).await.unwrap();
    let mut second = command(tenant);
    second.schedule_id = first.new_schedule_id.unwrap();
    second.change_id = "second".into();
    second.new_segments.as_mut().unwrap()[0].period_id = "202610".into();
    assert!(matches!(
        change(&service, second).await,
        Err(DomainError::InvalidRequest(_))
    ));
}

/// Durable result identity survives later successor mutation and completion.
#[tokio::test]
async fn replay_retains_original_id_after_successor_money_mutation_or_completion() {
    for complete in [false, true] {
        let (db, service, repo, tenant) = fixture().await;
        let cmd = command(tenant);
        let first = change(&service, cmd.clone()).await.unwrap();
        let successor = first.new_schedule_id.clone().unwrap();
        retry_transaction(&db, |txn| {
            let repo = repo.clone();
            let successor = successor.clone();
            Box::pin(async move {
                repo.add_recognized(
                    txn,
                    &AccessScope::allow_all(),
                    tenant,
                    &successor,
                    &money(if complete { "2" } else { "1" }, "EUR", 2),
                )
                .await?;
                if complete {
                    assert!(
                        repo.complete_schedule_if_drained(
                            txn,
                            &AccessScope::allow_all(),
                            tenant,
                            &successor
                        )
                        .await?
                    );
                }
                Ok(())
            })
        })
        .await
        .unwrap();
        let replay = change(&service, cmd).await.unwrap();
        assert_eq!(replay, first);
        assert_eq!(counts(&db).await, (2, 4, 1));
    }
}

#[tokio::test]
async fn invalid_stored_money_remains_internal_and_rolls_back_claim() {
    use sea_orm::sea_query::Expr;
    use toolkit_db::secure::SecureUpdateExt;
    let (db, service, _, tenant) = fixture().await;
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .col_expr(
            recognition_schedule::Column::Recognized,
            Expr::value("database is locked"),
        )
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        change(&service, command(tenant)).await,
        Err(DomainError::Internal(_))
    ));
    assert_eq!(counts(&db).await, (1, 2, 0));
}

#[tokio::test]
async fn manual_replacement_keeps_existing_count_policy_above_builder_default() {
    let (_, service, _, tenant) = fixture().await;
    let mut cmd = command(tenant);
    cmd.new_segments = Some(
        (0..121)
            .map(|i| {
                let month = 2026 * 12 + 10 + i;
                ChangeSegment {
                    period_id: format!("{:04}{:02}", month / 12, month % 12 + 1),
                    money: money(if i == 120 { "2" } else { "0" }, "EUR", 2),
                }
            })
            .collect(),
    );
    assert!(change(&service, cmd).await.is_ok());
    assert!(matches!(
        check_segment_count(usize::MAX),
        Err(DomainError::ScheduleTooLong(_))
    ));
}

/// Fresh actual PostgreSQL migration and immutable result evidence; no concurrency claim.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_durable_change_result_survives_completion_and_second_replacement() {
    use sea_orm_migration::MigratorTrait;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = sea_orm::Database::connect(&url).await.unwrap();
    crate::infra::storage::migrations::Migrator::up(&raw, None)
        .await
        .unwrap();
    let scoped_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let db = toolkit_db::connect_db(&scoped_url, toolkit_db::ConnectOpts::default())
        .await
        .unwrap();
    let (db, service, repo, tenant) = seeded_fixture(db, Uuid::now_v7()).await;
    let cmd = command(tenant);
    let first = change(&service, cmd.clone()).await.unwrap();
    let successor = first.new_schedule_id.clone().unwrap();
    let mut second = cmd.clone();
    second.schedule_id = successor;
    second.change_id = "next-change".into();
    let second_result = change(&service, second.clone()).await.unwrap();
    assert_eq!(change(&service, cmd.clone()).await.unwrap(), first);
    let next = second_result.new_schedule_id.clone().unwrap();
    retry_transaction(&db, |txn| {
        let repo = repo.clone();
        let next = next.clone();
        Box::pin(async move {
            repo.add_recognized(
                txn,
                &AccessScope::allow_all(),
                tenant,
                &next,
                &money("2", "EUR", 2),
            )
            .await?;
            assert!(
                repo.complete_schedule_if_drained(txn, &AccessScope::allow_all(), tenant, &next)
                    .await?
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    assert_eq!(change(&service, second).await.unwrap(), second_result);
    assert_eq!(change(&service, cmd).await.unwrap(), first);
    assert_eq!(counts(&db).await, (3, 6, 2));
    let scope = AccessScope::allow_all();
    let rows = idempotency_dedup::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(rows.iter().all(|row| row.result_entry_id.is_none()
        && row.result_schedule_id.is_some()
        && row.status == "CLAIMED"));
}

#[tokio::test]
async fn foreign_scope_cannot_read_replay_result_or_changed_request_conflict() {
    let (db, service, _, tenant) = fixture().await;
    let cmd = command(tenant);
    let first = change(&service, cmd.clone()).await.unwrap();
    let foreign_scope = AccessScope::for_tenant(Uuid::now_v7());
    for changed in [false, true] {
        let mut request = cmd.clone();
        if changed {
            request.new_segments.as_mut().unwrap()[0].money = money("1.01", "EUR", 2);
        }
        assert!(matches!(
            service
                .change(&SecurityContext::anonymous(), &foreign_scope, request)
                .await,
            Err(DomainError::CrossTenantAccessDenied(_))
        ));
    }
    assert_eq!(change(&service, cmd).await.unwrap(), first);
    assert_eq!(counts(&db).await, (2, 4, 1));
}

fn resource_scope(tenant: Uuid, schedule: &str) -> AccessScope {
    use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};
    AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
        ScopeFilter::eq(pep_properties::RESOURCE_ID, schedule),
    ]))
}

#[tokio::test]
async fn resource_scoped_cancel_and_terminal_original_replay_share_the_original_result() {
    let (db, service, _, tenant) = fixture().await;
    let mut cmd = command(tenant);
    cmd.action = "cancel".into();
    cmd.new_segments = None;
    assert_ne!(cmd.schedule_id, cmd.change_id);
    let scope = resource_scope(tenant, &cmd.schedule_id);
    let first = service
        .change(&SecurityContext::anonymous(), &scope, cmd.clone())
        .await
        .unwrap();
    assert_eq!(first.status, "CANCELLED");
    assert_eq!(
        service
            .change(&SecurityContext::anonymous(), &scope, cmd)
            .await
            .unwrap(),
        first
    );
    assert_eq!(counts(&db).await, (1, 2, 1));
}

#[tokio::test]
async fn same_tenant_scope_excluding_target_cannot_probe_replay_or_conflict() {
    let (db, service, _, tenant) = fixture().await;
    let cmd = command(tenant);
    let first = change(&service, cmd.clone()).await.unwrap();
    let excluded = resource_scope(tenant, "another-schedule");
    for changed in [false, true] {
        let mut request = cmd.clone();
        if changed {
            request.new_segments.as_mut().unwrap()[0].money = money("1.01", "EUR", 2);
        }
        assert!(matches!(
            service
                .change(&SecurityContext::anonymous(), &excluded, request)
                .await,
            Err(DomainError::InvalidRequest(_))
        ));
    }
    assert_eq!(change(&service, cmd).await.unwrap(), first);
    assert_eq!(counts(&db).await, (2, 4, 1));
}
