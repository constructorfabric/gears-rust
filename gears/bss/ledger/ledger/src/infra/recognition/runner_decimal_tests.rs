//! Actual runner/core/sidecar transactions on freshly migrated databases.
use super::*;
use crate::domain::model::{AccountRow, FiscalPeriodRow};
use crate::domain::ports::metrics::NoopLedgerMetrics;
use crate::infra::posting::retry::retry_transaction;
use crate::infra::posting::service::decimal_tests::money;
use crate::infra::storage::{
    entity::{
        account_balance, chain_state, idempotency_dedup, journal_entry, journal_line,
        recognition_schedule, recognition_segment,
    },
    repo::recognition_repo::{NewSchedule, NewSegment},
};
use sea_orm::EntityTrait;
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, SecureEntityExt, SecureUpdateExt};
use toolkit_db::{ConnectOpts, connect_db};

pub(crate) async fn fixture() -> (Db, RecognitionRunner, Uuid, ReleasableSegment) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    seeded(db, false).await
}
async fn seeded(db: Db, functional: bool) -> (Db, RecognitionRunner, Uuid, ReleasableSegment) {
    if db.backend() == sea_orm::DbBackend::Sqlite {
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            crate::infra::storage::migrations::Migrator::migrations(),
        )
        .await
        .unwrap();
    }
    let tenant = Uuid::now_v7();
    let provider = DBProvider::<DbError>::new(db.clone());
    let publisher = Arc::new(LedgerEventPublisher::noop());
    let runner = RecognitionRunner::new(provider.clone(), publisher, Arc::new(NoopLedgerMetrics));
    let scope = AccessScope::allow_all();
    // Non-ISO codes: the registry row is the only source of their scale, so a test can
    // change it and prove that a reversal keeps the stored scale (an ISO code always keeps
    // its ISO scale).
    for currency in ["TKA", "TKB"] {
        runner
            .reference
            .upsert_currency_scale(crate::domain::model::CurrencyScaleRow {
                tenant_id: tenant,
                currency: currency.into(),
                currency_scale: 2,
                source: "test".into(),
            })
            .await
            .unwrap();
    }
    let mut accounts = Vec::new();
    for (class, normal) in [
        (AccountClass::CashClearing, Side::Debit),
        (AccountClass::ContractLiability, Side::Credit),
        (AccountClass::Revenue, Side::Credit),
    ] {
        let id = Uuid::now_v7();
        accounts.push(id);
        runner
            .reference
            .insert_account(AccountRow {
                account_id: id,
                tenant_id: tenant,
                legal_entity_id: tenant,
                account_class: class.as_str().into(),
                currency: "TKA".into(),
                revenue_stream: class.is_per_stream().then(|| "test".into()),
                normal_side: normal.as_str().into(),
                may_go_negative: false,
                lifecycle_state: "OPEN".into(),
            })
            .await
            .unwrap();
    }
    let repo = runner.recognition.clone();
    let reference = runner.reference.clone();
    retry_transaction(&db, move |txn| {
        let repo = repo.clone();
        let reference = reference.clone();
        Box::pin(async move {
            reference
                .insert_fiscal_period_if_absent_txn(
                    txn,
                    FiscalPeriodRow {
                        tenant_id: tenant,
                        legal_entity_id: tenant,
                        period_id: "202610".into(),
                        fiscal_tz: "UTC".into(),
                        status: "OPEN".into(),
                    },
                )
                .await?;
            repo.insert_schedule(
                txn,
                &AccessScope::allow_all(),
                &NewSchedule {
                    tenant_id: tenant,
                    schedule_id: "schedule".into(),
                    payer_tenant_id: tenant,
                    source_invoice_id: "invoice".into(),
                    source_invoice_item_ref: "item".into(),
                    po_allocation_group: None,
                    subscription_ref: None,
                    revenue_stream: "test".into(),
                    total_deferred: money("1", "TKA", 2),
                    policy_ref: "straight-line".into(),
                    ssp_snapshot_ref: None,
                    vc_estimate_ref: None,
                    vc_method_ref: None,
                },
            )
            .await?;
            repo.insert_segments(
                txn,
                &AccessScope::allow_all(),
                &[NewSegment {
                    tenant_id: tenant,
                    schedule_id: "schedule".into(),
                    segment_no: 1,
                    period_id: "202610".into(),
                    amount: money("1", "TKA", 2),
                }],
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let candidate = ReleasableSegment {
        schedule_id: "schedule".into(),
        segment_no: 1,
        period_id: "202610".into(),
        amount: money("1", "TKA", 2),
        revenue_stream: "test".into(),
    };
    let mut header = posting_header(
        &SecurityContext::anonymous(),
        tenant,
        "202610",
        "TKA",
        "fund".into(),
    );
    header.source_doc_type = SourceDocType::InvoicePost;
    let mut cash = new_line(recognition_line(
        &candidate,
        AccountClass::CashClearing,
        Side::Debit,
    ));
    cash.account_id = accounts[0];
    let mut liability = new_line(recognition_line(
        &candidate,
        AccountClass::ContractLiability,
        Side::Credit,
    ));
    liability.account_id = accounts[1];
    if functional {
        cash.functional_money = Some(money("2", "TKB", 2));
        liability.functional_money = Some(money("2", "TKB", 2));
    }
    runner
        .posting
        .post(
            &SecurityContext::anonymous(),
            &scope,
            header,
            vec![cash, liability],
            None,
        )
        .await
        .unwrap();
    (db, runner, tenant, candidate)
}
/// Full posting and recognition rows; equality verifies values, versions, hashes and stamps.
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
#[tokio::test]
async fn stale_due_money_refreshes_and_completed_replay_ignores_closed_mutable_gates() {
    let (db, runner, tenant, mut candidate) = fixture().await;
    candidate.amount = money("999", "TKB", 3);
    candidate.revenue_stream = "wrong".into();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let first = runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    let original = runner
        .journal
        .find_entry_with_lines(&scope, tenant, first.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        original
            .lines
            .iter()
            .all(|l| l.money == money("1", "TKA", 2))
    );
    let conn = db.conn().unwrap();
    crate::infra::storage::entity::fiscal_period::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            crate::infra::storage::entity::fiscal_period::Column::Status,
            sea_orm::sea_query::Expr::value("CLOSED"),
        )
        .exec(&conn)
        .await
        .unwrap();
    crate::infra::storage::entity::tenant_account::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            crate::infra::storage::entity::tenant_account::Column::LifecycleState,
            sea_orm::sea_query::Expr::value("CLOSED"),
        )
        .exec(&conn)
        .await
        .unwrap();
    let before = snapshot(&db).await;
    let replay = runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.entry_id, first.entry_id);
    assert_eq!(before, snapshot(&db).await);
}
#[tokio::test]
async fn cap_rejection_rolls_back_full_post_and_recognition_state() {
    let (db, runner, tenant, candidate) = fixture().await;
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_schedule::Column::TotalDeferred,
            sea_orm::sea_query::Expr::value("0.5"),
        )
        .exec(&conn)
        .await
        .unwrap();
    let before = snapshot(&db).await;
    assert!(matches!(
        runner
            .release_segment(
                &SecurityContext::anonymous(),
                &scope,
                tenant,
                &candidate,
                Uuid::now_v7()
            )
            .await,
        Err(DomainError::OverRecognition(_))
    ));
    assert_eq!(before, snapshot(&db).await);
}
#[tokio::test]
async fn queue_then_predecessor_release_preserves_target_and_e2_actual_period_reversal() {
    let (db, runner, tenant, mut candidate) = fixture().await;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    let conn = db.conn().unwrap();
    recognition_segment::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_segment::Column::PeriodId,
            sea_orm::sea_query::Expr::value("202609"),
        )
        .exec(&conn)
        .await
        .unwrap();
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_schedule::Column::TotalDeferred,
            sea_orm::sea_query::Expr::value("1.2"),
        )
        .exec(&conn)
        .await
        .unwrap();
    let repo = runner.recognition.clone();
    retry_transaction(&db, move |txn| {
        let repo = repo.clone();
        Box::pin(async move {
            repo.insert_segments(
                txn,
                &AccessScope::allow_all(),
                &[NewSegment {
                    tenant_id: tenant,
                    schedule_id: "schedule".into(),
                    segment_no: 2,
                    period_id: "202610".into(),
                    amount: money("0.2", "TKA", 2),
                }],
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let mut second = candidate.clone();
    second.segment_no = 2;
    assert!(matches!(
        runner
            .release_operation(&ctx, &scope, tenant, &second, "202610", Uuid::now_v7())
            .await
            .unwrap(),
        SegmentOutcome::Queued(_)
    ));
    candidate.period_id = "202609".into();
    let first = runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    let original = runner
        .journal
        .find_entry_with_lines(&scope, tenant, first.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.period_id, "202610");
    let reversal = runner
        .release_reversal(&ctx, &scope, tenant, &candidate)
        .await
        .unwrap();
    let reversed = runner
        .journal
        .find_entry_with_lines(&scope, tenant, reversal.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reversed.reverses_period_id.as_deref(), Some("202610"));
    assert_eq!(reversed.reverses_entry_id, Some(first.entry_id));
    assert_eq!(reversed.lines.len(), original.lines.len());
    for line in &original.lines {
        let inverse = reversed
            .lines
            .iter()
            .find(|l| l.account_id == line.account_id)
            .unwrap();
        assert_eq!(inverse.money, line.money);
        assert_eq!(inverse.functional_money, line.functional_money);
        assert_ne!(inverse.side, line.side);
    }
    let after = snapshot(&db).await;
    assert!(
        runner
            .release_reversal(&ctx, &scope, tenant, &candidate)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(after, snapshot(&db).await);
    let summary = runner
        .run_period(&ctx, &scope, tenant, "202610", Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(summary.released, 1);
    assert_eq!(summary.queued, 0);
}
#[tokio::test]
async fn unauthorized_target_does_not_leak_posted_claim() {
    let (db, runner, tenant, candidate) = fixture().await;
    let ctx = SecurityContext::anonymous();
    runner
        .release_segment(
            &ctx,
            &AccessScope::allow_all(),
            tenant,
            &candidate,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    let before = snapshot(&db).await;
    assert!(matches!(
        runner
            .release_segment(
                &ctx,
                &AccessScope::for_tenant(Uuid::now_v7()),
                tenant,
                &candidate,
                Uuid::now_v7()
            )
            .await,
        Err(DomainError::RecognitionPolicyConflict(_))
    ));
    assert_eq!(before, snapshot(&db).await);
}
#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_actual_runner_release_replay_reversal() {
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    let container = test_containers::postgres().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let raw = sea_orm::Database::connect(&url).await.unwrap();
    crate::infra::storage::migrations::Migrator::up(&raw, None)
        .await
        .unwrap();
    let db = connect_db(
        &format!("{url}?options=-c%20search_path%3Dbss,public"),
        ConnectOpts::default(),
    )
    .await
    .unwrap();
    let (_, runner, tenant, candidate) = seeded(db.clone(), false).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let first = runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    assert!(!first.replayed);
    assert!(
        runner
            .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
            .await
            .unwrap()
            .replayed
    );
    assert!(
        !runner
            .release_reversal(&ctx, &scope, tenant, &candidate)
            .await
            .unwrap()
            .replayed
    );
    assert!(
        runner
            .release_reversal(&ctx, &scope, tenant, &candidate)
            .await
            .unwrap()
            .replayed
    );
    // Actual concurrent invocation on distinct transaction connections. This does
    // not force a particular SSI interleaving or claim retries were observed.
    let (_, runner, tenant, candidate) = seeded(db.clone(), false).await;
    let change = crate::infra::recognition::change_service::RecognitionChangeService::new(
        DBProvider::new(db.clone()),
        Arc::new(LedgerEventPublisher::noop()),
    );
    let command = bss_ledger_sdk::ChangeRecognitionSchedule {
        tenant_id: tenant,
        schedule_id: "schedule".into(),
        change_id: "racing-replace".into(),
        action: "replace".into(),
        treatment: "prospective".into(),
        new_segments: Some(vec![bss_ledger_sdk::ChangeSegment {
            period_id: "202610".into(),
            money: money("1", "TKA", 2),
        }]),
    };
    let (release, replacement) = tokio::join!(
        runner.release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7()),
        change.change(&ctx, &scope, command)
    );
    assert!(release.is_ok() || replacement.is_ok());
    let original = runner
        .recognition
        .read_schedule(&scope, tenant, "schedule")
        .await
        .unwrap()
        .unwrap();
    if release.is_ok() {
        assert_eq!(original.recognized, money("1", "TKA", 2));
        assert!(replacement.is_err());
    } else {
        assert_eq!(original.status, "REPLACED");
        assert_eq!(original.recognized, money("0", "TKA", 2));
        assert!(replacement.is_ok());
    }
}

#[tokio::test]
async fn forward_functional_recognition_is_explicitly_unsupported_without_effects() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    let (db, runner, tenant, candidate) = seeded(db, true).await;
    let before = snapshot(&db).await;
    assert!(matches!(
        runner
            .release_segment(
                &SecurityContext::anonymous(),
                &AccessScope::allow_all(),
                tenant,
                &candidate,
                Uuid::now_v7()
            )
            .await,
        Err(DomainError::FxOperationUnsupported(_))
    ));
    assert_eq!(before, snapshot(&db).await);
}

#[tokio::test]
async fn historical_functional_release_reversal_restores_full_dimensions_despite_registry_change() {
    use crate::infra::storage::repo::fx_repo::{FxRepo, NewFxRate, NewRateSnapshot};
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    let (db, runner, tenant, candidate) = seeded(db, true).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let fx = FxRepo::new(DBProvider::<DbError>::new(db.clone()));
    let rate_id = fx
        .insert_snapshot(
            &scope,
            &NewRateSnapshot {
                tenant_id: tenant,
                base_currency: money("1", "TKA", 2).currency().clone(),
                quote_currency: money("2", "TKB", 2).currency().clone(),
                rate: bss_ledger_sdk::parse_decimal("2").unwrap(),
                as_of: OffsetDateTime::now_utc(),
                provider: "test".into(),
                stale: false,
                fallback_order: 0,
                triangulated_via: None,
            },
        )
        .await
        .unwrap();
    let svc = runner.clone();
    let candidate_txn = candidate.clone();
    // Existing historical release evidence is posted by the actual core seam,
    // not synthesized by the forward runner or an invented S6 allocation rule.
    let first = retry_transaction(&db, move |txn| {
        let svc = svc.clone();
        let candidate = candidate_txn.clone();
        Box::pin(async move {
            let (schedule, segment) = svc
                .observed(
                    txn,
                    &AccessScope::allow_all(),
                    tenant,
                    &candidate.schedule_id,
                    candidate.segment_no,
                )
                .await?;
            let chart =
                load_chart_in(&svc.reference, txn, &AccessScope::allow_all(), tenant).await?;
            let bound = build_recognition_entry(&SecurityContext::anonymous(), tenant, &candidate);
            let mut lines: Vec<_> = bound.lines.into_iter().map(new_line).collect();
            for line in &mut lines {
                line.account_id = chart
                    .resolve(line.account_class, "TKA", Some("test"))
                    .unwrap();
                line.functional_money = Some(money("2", "TKB", 2));
                line.gl_code = Some("historical-gl".into());
                line.invoice_item_ref = Some("historical-item".into());
                line.price_id = Some("historical-price".into());
                line.po_allocation_group = Some("historical-po".into());
            }
            let sidecar: Arc<dyn PostSidecar> = Arc::new(RecognitionStampSidecar {
                tenant_id: tenant,
                schedule_id: schedule.schedule_id.clone(),
                segment_no: segment.segment_no,
                period_id: "202610".into(),
                amount: segment.amount.clone(),
                revenue_stream: "test".into(),
                expected_schedule_version: schedule.version,
                expected_segment_version: segment.version,
                run_id: Uuid::now_v7(),
                recognition_repo: svc.recognition.clone(),
                publisher: svc.publisher.clone(),
                ctx: SecurityContext::anonymous(),
            });
            let mut header = posting_header(
                &SecurityContext::anonymous(),
                tenant,
                "202610",
                "TKA",
                "schedule:1".into(),
            );
            header.rate_snapshot_ref = Some(rate_id);
            header.rounding_evidence =
                serde_json::json!({"original": "stored-functional-boundary"});
            svc.posting
                .post_once(
                    &SecurityContext::anonymous(),
                    txn,
                    &AccessScope::allow_all(),
                    header,
                    lines,
                    Some(sidecar),
                    ClaimSpec::fresh_with_request_hash(release_hash(&schedule, &segment)),
                )
                .await
        })
    })
    .await
    .unwrap();
    let conn = db.conn().unwrap();
    crate::infra::storage::entity::currency_scale_registry::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            crate::infra::storage::entity::currency_scale_registry::Column::CurrencyScale,
            sea_orm::sea_query::Expr::value(3_i16),
        )
        .exec(&conn)
        .await
        .unwrap();
    fx.upsert_rate(&NewFxRate {
        tenant_id: tenant,
        base_currency: "TKA".into(),
        quote_currency: "TKB".into(),
        provider: "test".into(),
        rate: bss_ledger_sdk::parse_decimal("7").unwrap(),
        as_of: OffsetDateTime::now_utc(),
        fallback_order: 0,
    })
    .await
    .unwrap();
    let original = runner
        .journal
        .find_entry_with_lines(&scope, tenant, first.entry_id)
        .await
        .unwrap()
        .unwrap();
    let reverse = runner
        .release_reversal(&ctx, &scope, tenant, &candidate)
        .await
        .unwrap();
    let reversed = runner
        .journal
        .find_entry_with_lines(&scope, tenant, reverse.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reversed.rounding_evidence, original.rounding_evidence);
    for line in &original.lines {
        let actual = reversed
            .lines
            .iter()
            .find(|l| l.account_id == line.account_id)
            .unwrap();
        let mut expected = line.clone();
        expected.line_id = actual.line_id;
        expected.entry_id = actual.entry_id;
        expected.side = actual.side.clone();
        assert_ne!(actual.side, line.side);
        assert_eq!(&expected, actual);
    }
    let rows = journal_line::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    assert!(
        rows.iter()
            .filter(|r| r.entry_id == first.entry_id || r.entry_id == reverse.entry_id)
            .all(|r| r.rate_snapshot_ref == Some(rate_id))
    );
    assert_eq!(
        runner
            .recognition
            .list_segments(&scope, tenant, "schedule")
            .await
            .unwrap()[0]
            .status,
        "DONE"
    );
}

#[tokio::test]
#[cfg(feature = "test-support")]
async fn metrics_count_only_fresh_committed_releases() {
    let (db, mut runner, tenant, candidate) = fixture().await;
    let metrics = crate::infra::metrics::test_harness::MetricsHarness::new();
    runner.metrics = Arc::new(metrics.metrics());
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    runner
        .release_reversal(&ctx, &scope, tenant, &candidate)
        .await
        .unwrap();
    let before = snapshot(&db).await;
    let mut missing = candidate;
    missing.segment_no = 99;
    assert!(
        runner
            .release_segment(&ctx, &scope, tenant, &missing, Uuid::now_v7())
            .await
            .is_err()
    );
    assert_eq!(before, snapshot(&db).await);
    metrics.force_flush();
    assert_eq!(
        metrics.counter_value("ledger_revenue_recognized_total", &[("stream", "test")]),
        1
    );
}

#[tokio::test]
async fn stale_actual_discovery_refreshes_changed_segment_and_replaced_schedule_cannot_release() {
    let (db, runner, tenant, _) = fixture().await;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    let due = runner
        .recognition
        .list_due_pending_segments(&scope, tenant, "202610")
        .await
        .unwrap();
    let candidate = ReleasableSegment::from(due[0].clone());
    let conn = db.conn().unwrap();
    recognition_segment::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_segment::Column::Amount,
            sea_orm::sea_query::Expr::value("0.6"),
        )
        .col_expr(
            recognition_segment::Column::Version,
            sea_orm::sea_query::Expr::value(1_i64),
        )
        .exec(&conn)
        .await
        .unwrap();
    let posting = runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    let original = runner
        .journal
        .find_entry_with_lines(&scope, tenant, posting.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        original
            .lines
            .iter()
            .all(|l| l.money == money("0.6", "TKA", 2))
    );
    let (db, runner, tenant, candidate) = fixture().await;
    let service = crate::infra::recognition::change_service::RecognitionChangeService::new(
        DBProvider::new(db.clone()),
        Arc::new(LedgerEventPublisher::noop()),
    );
    service
        .change(
            &ctx,
            &scope,
            bss_ledger_sdk::ChangeRecognitionSchedule {
                tenant_id: tenant,
                schedule_id: "schedule".into(),
                change_id: "replacement".into(),
                action: "replace".into(),
                treatment: "prospective".into(),
                new_segments: Some(vec![bss_ledger_sdk::ChangeSegment {
                    period_id: "202610".into(),
                    money: money("1", "TKA", 2),
                }]),
            },
        )
        .await
        .unwrap();
    let before = snapshot(&db).await;
    assert!(matches!(
        runner
            .release_operation(&ctx, &scope, tenant, &candidate, "202610", Uuid::now_v7())
            .await
            .unwrap(),
        SegmentOutcome::Skipped
    ));
    assert_eq!(before, snapshot(&db).await);
}

#[tokio::test]
async fn resource_scoped_completed_replay_authorizes_schedule_before_claim_conflict() {
    use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};
    let (db, runner, tenant, candidate) = fixture().await;
    let ctx = SecurityContext::anonymous();
    let first = runner
        .release_segment(
            &ctx,
            &AccessScope::allow_all(),
            tenant,
            &candidate,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    let make_scope = |resource: &str| {
        AccessScope::single(ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
            ScopeFilter::eq(pep_properties::RESOURCE_ID, resource),
        ]))
    };
    let scope = make_scope("schedule");
    assert_eq!(
        runner
            .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
            .await
            .unwrap()
            .entry_id,
        first.entry_id
    );
    let conn = db.conn().unwrap();
    recognition_segment::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .col_expr(
            recognition_segment::Column::Amount,
            sea_orm::sea_query::Expr::value("0.5"),
        )
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        runner
            .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
            .await,
        Err(DomainError::IdempotencyConflict(_))
    ));
    assert!(matches!(
        runner
            .release_segment(
                &ctx,
                &make_scope("excluded"),
                tenant,
                &candidate,
                Uuid::now_v7()
            )
            .await,
        Err(DomainError::RecognitionPolicyConflict(_))
    ));
}

struct Unsatisfied;
#[async_trait::async_trait]
impl ObligationStateResolver for Unsatisfied {
    async fn resolve(
        &self,
        _: &ObligationContext,
    ) -> crate::domain::ports::obligation_state::ObligationState {
        crate::domain::ports::obligation_state::ObligationState::NotSatisfied
    }
}
#[tokio::test]
async fn obligation_and_future_target_leave_all_financial_state_untouched() {
    let (db, mut runner, tenant, candidate) = fixture().await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let before = snapshot(&db).await;
    runner.obligation = Arc::new(Unsatisfied);
    let summary = runner
        .run_period(&ctx, &scope, tenant, "202610", Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(summary.skipped, 1);
    assert_eq!(before, snapshot(&db).await);
    runner.obligation = Arc::new(AlwaysSatisfiedObligationState);
    assert!(matches!(
        runner
            .release_operation(&ctx, &scope, tenant, &candidate, "202609", Uuid::now_v7())
            .await
            .unwrap(),
        SegmentOutcome::Skipped
    ));
    assert_eq!(before, snapshot(&db).await);
}
#[tokio::test]
async fn an_unsatisfied_obligation_still_replays_an_already_posted_release() {
    let (_db, mut runner, tenant, candidate) = fixture().await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let first = runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    assert!(!first.replayed);
    runner.obligation = Arc::new(Unsatisfied);
    let replay = runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .expect("the posted release replays whatever the obligation answers now");
    assert!(replay.replayed);
    assert_eq!(replay.entry_id, first.entry_id);
}
#[tokio::test]
async fn terminal_schedule_negative_delta_succeeds_but_negative_result_rolls_back() {
    let (db, runner, tenant, candidate) = fixture().await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_schedule::Column::Status,
            sea_orm::sea_query::Expr::value("CANCELLED"),
        )
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    runner
        .release_reversal(&ctx, &scope, tenant, &candidate)
        .await
        .unwrap();
    let state = runner
        .recognition
        .read_schedule(&scope, tenant, "schedule")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.recognized, money("0", "TKA", 2));
    assert_eq!(state.status, "CANCELLED");
    assert_eq!(
        runner
            .recognition
            .list_segments(&scope, tenant, "schedule")
            .await
            .unwrap()[0]
            .status,
        "DONE"
    );
    let (db, runner, tenant, candidate) = fixture().await;
    runner
        .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
        .await
        .unwrap();
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_schedule::Column::Status,
            sea_orm::sea_query::Expr::value("CANCELLED"),
        )
        .col_expr(
            recognition_schedule::Column::Recognized,
            sea_orm::sea_query::Expr::value("0"),
        )
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    let before = snapshot(&db).await;
    assert!(matches!(
        runner
            .release_reversal(&ctx, &scope, tenant, &candidate)
            .await,
        Err(DomainError::OverRecognition(_))
    ));
    assert_eq!(before, snapshot(&db).await);
}

#[tokio::test]
async fn later_release_failure_keeps_prior_segment_committed() {
    let (db, mut runner, tenant, _) = fixture().await;
    let scope = AccessScope::allow_all();
    let repo = runner.recognition.clone();
    retry_transaction(&db, move |txn| {
        let repo = repo.clone();
        Box::pin(async move {
            repo.insert_schedule(
                txn,
                &AccessScope::allow_all(),
                &NewSchedule {
                    tenant_id: tenant,
                    schedule_id: "z-second".into(),
                    payer_tenant_id: tenant,
                    source_invoice_id: "second-invoice".into(),
                    source_invoice_item_ref: "second-item".into(),
                    po_allocation_group: None,
                    subscription_ref: None,
                    revenue_stream: "test".into(),
                    total_deferred: money("0.2", "TKA", 2),
                    policy_ref: "straight-line".into(),
                    ssp_snapshot_ref: None,
                    vc_estimate_ref: None,
                    vc_method_ref: None,
                },
            )
            .await?;
            repo.insert_segments(
                txn,
                &AccessScope::allow_all(),
                &[NewSegment {
                    tenant_id: tenant,
                    schedule_id: "z-second".into(),
                    segment_no: 1,
                    period_id: "202610".into(),
                    amount: money("0.2", "TKA", 2),
                }],
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    #[cfg(feature = "test-support")]
    let metrics = crate::infra::metrics::test_harness::MetricsHarness::new();
    #[cfg(feature = "test-support")]
    {
        runner.metrics = Arc::new(metrics.metrics());
    }
    assert!(matches!(
        runner
            .run_period(
                &SecurityContext::anonymous(),
                &scope,
                tenant,
                "202610",
                Uuid::now_v7()
            )
            .await,
        Err(DomainError::NegativeBalance(_))
    ));
    let first = runner
        .recognition
        .read_schedule(&scope, tenant, "schedule")
        .await
        .unwrap()
        .unwrap();
    let second = runner
        .recognition
        .read_schedule(&scope, tenant, "z-second")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.recognized, money("1", "TKA", 2));
    assert_eq!(first.status, "COMPLETED");
    assert_eq!(second.recognized, money("0", "TKA", 2));
    assert_eq!(
        runner
            .recognition
            .list_segments(&scope, tenant, "schedule")
            .await
            .unwrap()[0]
            .status,
        "DONE"
    );
    assert_eq!(
        runner
            .recognition
            .list_segments(&scope, tenant, "z-second")
            .await
            .unwrap()[0]
            .status,
        "PENDING"
    );
    #[cfg(feature = "test-support")]
    {
        metrics.force_flush();
        assert_eq!(
            metrics.counter_value("ledger_revenue_recognized_total", &[("stream", "test")]),
            1
        );
    }
}

#[tokio::test]
async fn historical_stored_spec_disagreement_is_internal_and_preserves_all_financial_state() {
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    for (code, scale) in [("TKB", 2_i16), ("TKA", 3_i16)] {
        let (db, runner, tenant, candidate) = fixture().await;
        runner
            .release_segment(&ctx, &scope, tenant, &candidate, Uuid::now_v7())
            .await
            .unwrap();
        let conn = db.conn().unwrap();
        // Coherent schedule/segment metadata still decodes, but no longer
        // agrees with the original complete journal's persisted TKA/scale2.
        recognition_schedule::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(
                recognition_schedule::Column::Currency,
                sea_orm::sea_query::Expr::value(code),
            )
            .col_expr(
                recognition_schedule::Column::CurrencyScale,
                sea_orm::sea_query::Expr::value(scale),
            )
            .exec(&conn)
            .await
            .unwrap();
        recognition_segment::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(
                recognition_segment::Column::Currency,
                sea_orm::sea_query::Expr::value(code),
            )
            .col_expr(
                recognition_segment::Column::CurrencyScale,
                sea_orm::sea_query::Expr::value(scale),
            )
            .exec(&conn)
            .await
            .unwrap();
        let schedule = runner
            .recognition
            .read_schedule(&scope, tenant, "schedule")
            .await
            .unwrap()
            .unwrap();
        let segments = runner
            .recognition
            .list_segments(&scope, tenant, "schedule")
            .await
            .unwrap();
        assert_eq!(schedule.total_deferred.currency().code(), code);
        assert_eq!(
            schedule.total_deferred.currency().scale(),
            u8::try_from(scale).unwrap()
        );
        assert_eq!(
            segments[0].amount.currency(),
            schedule.total_deferred.currency()
        );
        let before = snapshot(&db).await;
        let error = runner
            .release_reversal(&ctx, &scope, tenant, &candidate)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, DomainError::Internal(detail)
            if detail == "stored original release metadata is inconsistent"),
            "wrong stored-corruption classification: {error:?}"
        );
        assert_eq!(before, snapshot(&db).await);
    }
}
