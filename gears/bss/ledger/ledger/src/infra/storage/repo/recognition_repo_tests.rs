//! Real migrated SQLite checks, including deliberately stale observations and rollback.
use super::*;
use crate::infra::posting::retry::{AttemptError, retry_transaction};
use bss_ledger_sdk::MoneyError;
use bss_ledger_sdk::parse_decimal;
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

fn specified(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
fn money(text: &str) -> PostedMoney {
    specified(text, "EUR", 2)
}
async fn setup() -> (RecognitionRepo, Db, Uuid) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    (
        RecognitionRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
    )
}
fn schedule(tenant: Uuid, id: &str, amount: PostedMoney) -> NewSchedule {
    NewSchedule {
        tenant_id: tenant,
        schedule_id: id.into(),
        payer_tenant_id: tenant,
        source_invoice_id: "invoice".into(),
        source_invoice_item_ref: id.into(),
        po_allocation_group: None,
        subscription_ref: None,
        revenue_stream: "service".into(),
        total_deferred: amount,
        policy_ref: "straight-line".into(),
        ssp_snapshot_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
    }
}
fn segment(tenant: Uuid, id: &str, no: i32, amount: PostedMoney) -> NewSegment {
    NewSegment {
        tenant_id: tenant,
        schedule_id: id.into(),
        segment_no: no,
        period_id: format!("2026{no:02}"),
        amount,
    }
}

#[tokio::test]
async fn schedule_and_segment_money_lifecycle_lineage_and_scope() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("0.3")))
                .await?;
            r.insert_segments(
                tx,
                &s,
                &[
                    segment(tenant, "s", 1, money("0.1")),
                    segment(tenant, "s", 2, money("0.2")),
                ],
            )
            .await?;
            assert!(matches!(
                r.stamp_segment_done(
                    tx,
                    &s,
                    tenant,
                    "s",
                    2,
                    Uuid::now_v7(),
                    OffsetDateTime::now_utc()
                )
                .await,
                Err(RepoError::RecognitionPolicyConflict(_))
            ));
            r.add_pending_segment_amount(tx, &s, tenant, "s", 1, &money("0.1"))
                .await?;
            r.increase_total_deferred(tx, &s, tenant, "s", &money("0.1"))
                .await?;
            assert!(!r.complete_schedule_if_drained(tx, &s, tenant, "s").await?);
            r.add_recognized(tx, &s, tenant, "s", &money("0.2")).await?;
            r.stamp_segment_done(
                tx,
                &s,
                tenant,
                "s",
                1,
                Uuid::now_v7(),
                OffsetDateTime::now_utc(),
            )
            .await?;
            r.mark_segment_queued_in(tx, &s, tenant, "s", 2).await?;
            r.mark_segment_queued_in(tx, &s, tenant, "s", 2).await?;
            assert!(matches!(
                r.add_pending_segment_amount(tx, &s, tenant, "s", 2, &money("0.01"))
                    .await,
                Err(RepoError::RecognitionPolicyConflict(_))
            ));
            r.add_recognized(tx, &s, tenant, "s", &money("0.2")).await?;
            r.stamp_segment_done(
                tx,
                &s,
                tenant,
                "s",
                2,
                Uuid::now_v7(),
                OffsetDateTime::now_utc(),
            )
            .await?;
            let row = r.required_schedule(tx, &s, tenant, "s").await?;
            assert_eq!(row.version, 3);
            assert_eq!(row.total_deferred, money("0.4"));
            assert!(r.complete_schedule_if_drained(tx, &s, tenant, "s").await?);
            assert!(!r.complete_schedule_if_drained(tx, &s, tenant, "s").await?);
            assert_eq!(r.required_schedule(tx, &s, tenant, "s").await?.version, 3);
            // Reversal semantics preserve terminal status and allow signed deltas.
            r.add_recognized(tx, &s, tenant, "s", &money("-0.1"))
                .await?;
            r.reduce_deferred(tx, &s, tenant, "s", &money("-0.1"))
                .await?;
            let row = r.required_schedule(tx, &s, tenant, "s").await?;
            assert_eq!((row.status.as_str(), row.version), ("COMPLETED", 5));
            assert_eq!(row.total_deferred, money("0.5"));
            assert_eq!(row.recognized, money("0.3"));
            assert_eq!(
                r.mark_schedule_status(tx, &s, tenant, "s", "ACTIVE", "REPLACED")
                    .await?,
                0
            );
            assert_eq!(
                r.max_done_segment_period_in_txn(tx, &s, tenant, "s")
                    .await?
                    .as_deref(),
                Some("202602")
            );
            assert_eq!(
                r.count_due_not_done_in_txn(tx, &s, tenant, "202612")
                    .await?,
                0
            );
            let rows = r.list_segments_in_txn(tx, &s, tenant, "s").await?;
            assert_eq!(
                rows.iter().map(|v| v.version).collect::<Vec<_>>(),
                vec![2, 2]
            );
            assert!(rows.iter().all(|v| v.amount == money("0.2")));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let s = AccessScope::for_tenant(tenant);
    assert_eq!(repo.list_segments(&s, tenant, "s").await.unwrap().len(), 2);
    assert!(
        repo.list_due_pending_segments(&s, tenant, "202612")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.list_schedules(&s, tenant, Some("invoice"), Some("service"))
            .await
            .unwrap()
            .0
            .len(),
        1
    );
    let denied = AccessScope::for_tenant(Uuid::now_v7());
    assert!(
        repo.read_schedule(&denied, tenant, "s")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.list_segments(&denied, tenant, "s")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.list_schedules(&denied, tenant, None, None)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert!(
        repo.list_due_pending_segments(&denied, tenant, "202612")
            .await
            .unwrap()
            .is_empty()
    );
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            assert!(matches!(
                r.add_recognized(tx, &denied, tenant, "s", &money("0.01"))
                    .await,
                Err(RepoError::RecognitionPolicyConflict(_))
            ));
            assert!(
                r.insert_schedule(tx, &denied, &schedule(tenant, "foreign", money("1")))
                    .await
                    .is_err()
            );
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn creation_replacement_uniqueness_exact_caps_and_metadata() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("0.3")))
                .await?;
            r.insert_segments(tx, &s, &[segment(tenant, "s", 1, money("0.1"))])
                .await?;
            assert!(
                r.insert_segments(tx, &s, &[segment(tenant, "s", 1, money("0.1"))])
                    .await
                    .is_err()
            );
            let mut duplicate_period = segment(tenant, "s", 2, money("0.1"));
            duplicate_period.period_id = "202601".into();
            assert!(
                r.insert_segments(tx, &s, &[duplicate_period])
                    .await
                    .is_err()
            );
            assert!(matches!(
                r.insert_segments(
                    tx,
                    &s,
                    &[segment(tenant, "s", 2, specified("0.1", "EUR", 3))]
                )
                .await,
                Err(RepoError::Money(MoneyError::ScaleMismatch))
            ));
            assert!(matches!(
                r.insert_segments(
                    tx,
                    &s,
                    &[segment(tenant, "s", 2, specified("0.1", "USD", 2))]
                )
                .await,
                Err(RepoError::Money(MoneyError::CurrencyMismatch))
            ));
            r.add_recognized(tx, &s, tenant, "s", &money("0.1")).await?;
            r.add_recognized(tx, &s, tenant, "s", &money("0.2")).await?;
            assert!(matches!(
                r.add_recognized(tx, &s, tenant, "s", &money("0.01")).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            assert!(matches!(
                r.reduce_deferred(tx, &s, tenant, "s", &money("0.01")).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            assert!(matches!(
                r.add_recognized(tx, &s, tenant, "s", &money("-0.31")).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            let old = r.required_schedule(tx, &s, tenant, "s").await?;
            assert_eq!(old.version, 2);
            assert_eq!(
                r.mark_schedule_status(tx, &s, tenant, "s", "ACTIVE", "REPLACED")
                    .await?,
                1
            );
            let replacement = ReplacementSchedule {
                tenant_id: tenant,
                schedule_id: "next".into(),
                payer_tenant_id: tenant,
                source_invoice_id: old.source_invoice_id.clone(),
                source_invoice_item_ref: old.source_invoice_item_ref.clone(),
                po_allocation_group: None,
                subscription_ref: None,
                revenue_stream: old.revenue_stream.clone(),
                total_deferred: money("0"),
                policy_ref: old.policy_ref,
                ssp_snapshot_ref: None,
                vc_estimate_ref: None,
                vc_method_ref: None,
                version: old.version + 1,
            };
            r.insert_replacement_schedule(tx, &s, &replacement).await?;
            let next = r
                .read_active_successor_in_txn(tx, &s, tenant, "invoice", "s", "service", 3)
                .await?
                .unwrap();
            assert_eq!(next.schedule_id, "next");
            assert_eq!(next.recognized, money("0"));
            assert_eq!(
                r.read_active_schedule_in_txn(tx, &s, tenant, "invoice", "s", "service")
                    .await?
                    .unwrap(),
                next
            );
            for (id, value) in [
                ("whole", specified("1", "JPY", 0)),
                (
                    "tiny",
                    specified("0.0000000000000000000000000001", "TOK", 28),
                ),
            ] {
                r.insert_schedule(tx, &s, &schedule(tenant, id, value.clone()))
                    .await?;
                r.insert_segments(tx, &s, &[segment(tenant, id, 1, value.clone())])
                    .await?;
                r.add_recognized(tx, &s, tenant, id, &value).await?;
                assert_eq!(
                    r.required_schedule(tx, &s, tenant, id).await?.recognized,
                    value
                );
            }
            let max = money("9999999999999999999999999999");
            r.insert_schedule(tx, &s, &schedule(tenant, "max", max.clone()))
                .await?;
            assert!(matches!(
                r.increase_total_deferred(tx, &s, tenant, "max", &money("1"))
                    .await,
                Err(RepoError::Money(MoneyError::AmountOutOfRange))
            ));
            r.add_recognized(tx, &s, tenant, "max", &max).await?;
            // Exact cap is tested before narrowing the out-of-range intermediate.
            assert!(matches!(
                r.add_recognized(tx, &s, tenant, "max", &max).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let s = AccessScope::for_tenant(tenant);
    let due = repo
        .list_due_pending_segments(&s, tenant, "202612")
        .await
        .unwrap();
    assert_eq!(due.len(), 2); // Old replaced schedule excluded.
    assert_eq!(due[0].schedule_id, "tiny");
    assert_eq!(due[0].amount.currency().scale(), 28);
    assert_eq!(due[1].schedule_id, "whole");
}

#[tokio::test]
async fn stale_completion_segment_races_abort_three_whole_attempts_and_rollback() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("1")))
                .await?;
            r.insert_segments(tx, &s, &[segment(tenant, "s", 1, money("1"))])
                .await?;
            r.add_recognized(tx, &s, tenant, "s", &money("1")).await?;
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = attempts.clone();
    let r = repo.clone();
    let result = retry_transaction(&db, move |tx| {
        let r = r.clone();
        let count = count.clone();
        Box::pin(async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "rollback", money("2")))
                .await?;
            let old = r.required_schedule(tx, &s, tenant, "s").await?;
            assert!(r.complete_schedule_if_drained(tx, &s, tenant, "s").await?);
            let current = r.required_schedule(tx, &s, tenant, "s").await?;
            assert_eq!(old.version, current.version); // version alone cannot catch this race
            r.write_schedule(
                tx,
                &s,
                &old,
                &money("2"),
                &old.recognized,
                &old.status,
                old.version + 1,
            )
            .await?;
            Ok(())
        })
    })
    .await;
    assert!(matches!(
        result,
        Err(crate::domain::error::DomainError::ConcurrentModification(_))
    ));
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    let s = AccessScope::for_tenant(tenant);
    assert!(
        repo.read_schedule(&s, tenant, "rollback")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.read_schedule(&s, tenant, "s")
            .await
            .unwrap()
            .unwrap()
            .status,
        "ACTIVE"
    );
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let stale = r.required_segment(tx, &s, tenant, "s", 1).await?;
            r.mark_segment_queued_in(tx, &s, tenant, "s", 1).await?;
            assert!(matches!(
                r.write_segment(tx, &s, &stale, &money("2"), "PENDING", None, None)
                    .await,
                Err(RepoError::Conflict(_))
            ));
            let queued = r.required_segment(tx, &s, tenant, "s", 1).await?;
            r.stamp_segment_done(
                tx,
                &s,
                tenant,
                "s",
                1,
                Uuid::now_v7(),
                OffsetDateTime::now_utc(),
            )
            .await?;
            assert!(matches!(
                r.write_segment(tx, &s, &queued, &queued.amount, "QUEUED", None, None)
                    .await,
                Err(RepoError::Conflict(_))
            ));
            let row = r.required_schedule(tx, &s, tenant, "s").await?;
            r.reduce_deferred(tx, &s, tenant, "s", &money("-1")).await?;
            assert!(matches!(
                r.write_schedule(
                    tx,
                    &s,
                    &row,
                    &row.total_deferred,
                    &row.recognized,
                    "CANCELLED",
                    row.version + 1
                )
                .await,
                Err(RepoError::Conflict(_))
            ));
            assert!(!r.complete_schedule_if_drained(tx, &s, tenant, "s").await?);
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stored_corruption_and_version_exhaustion_fail_before_repair() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("1")))
                .await?;
            r.insert_segments(tx, &s, &[segment(tenant, "s", 1, money("1"))])
                .await?;
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let scope = AccessScope::for_tenant(tenant);
    let conn = repo.db.conn().unwrap();
    for text in ["1.00", "broken", "2"] {
        recognition_schedule::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(recognition_schedule::Column::Recognized, Expr::value(text))
            .exec(&conn)
            .await
            .unwrap();
        assert!(matches!(
            repo.read_schedule(&scope, tenant, "s").await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
        assert!(matches!(
            repo.list_schedules(&scope, tenant, None, None).await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
        assert!(matches!(
            repo.list_due_pending_segments(&scope, tenant, "202612")
                .await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
        let r = repo.clone();
        db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let s = AccessScope::for_tenant(tenant);
                assert!(matches!(
                    r.reduce_deferred(tx, &s, tenant, "s", &money("-10")).await,
                    Err(RepoError::InvalidStoredMoney(_))
                ));
                Ok::<_, AttemptError>(())
            })
        })
        .await
        .unwrap();
    }
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(recognition_schedule::Column::Recognized, Expr::value("0"))
        .col_expr(recognition_schedule::Column::Version, Expr::value(i64::MAX))
        .exec(&conn)
        .await
        .unwrap();
    for text in ["1.00", "bad"] {
        recognition_segment::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(recognition_segment::Column::Amount, Expr::value(text))
            .exec(&conn)
            .await
            .unwrap();
        assert!(matches!(
            repo.list_segments(&scope, tenant, "s").await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
        assert!(matches!(
            repo.mark_segment_queued(&scope, tenant, "s", 1).await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
    recognition_segment::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(recognition_segment::Column::Amount, Expr::value("1"))
        .col_expr(
            recognition_segment::Column::CurrencyScale,
            Expr::value(3_i16),
        )
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        repo.list_segments(&scope, tenant, "s").await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    recognition_segment::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_segment::Column::CurrencyScale,
            Expr::value(2_i16),
        )
        .col_expr(recognition_segment::Column::Version, Expr::value(i64::MAX))
        .exec(&conn)
        .await
        .unwrap();
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            assert!(matches!(
                r.increase_total_deferred(tx, &s, tenant, "s", &money("1"))
                    .await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            assert!(matches!(
                r.mark_segment_queued_in(tx, &s, tenant, "s", 1).await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            assert_eq!(
                r.required_schedule(tx, &s, tenant, "s")
                    .await?
                    .total_deferred,
                money("1")
            );
            assert_eq!(
                r.required_segment(tx, &s, tenant, "s", 1).await?.status,
                "PENDING"
            );
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

fn header(tenant: Uuid, entry: Uuid) -> journal_entry::ActiveModel {
    journal_entry::ActiveModel {
        entry_id: Set(entry),
        tenant_id: Set(tenant),
        legal_entity_id: Set(tenant),
        period_id: Set("2026-10".into()),
        entry_currency: Set("EUR".into()),
        source_doc_type: Set(SourceDocType::Recognition.as_str().into()),
        source_business_id: Set("i".into()),
        reverses_entry_id: Set(None),
        reverses_period_id: Set(None),
        posted_at_utc: Set(OffsetDateTime::now_utc()),
        effective_at: Set(chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
        origin: Set("SYSTEM".into()),
        posted_by_actor_id: Set(tenant),
        correlation_id: Set(Uuid::now_v7()),
        rounding_evidence: Set(serde_json::json!({})),
        created_seq: Set(1),
        row_hash: Set(None),
        prev_hash: Set(None),
        prev_entry_id: Set(None),
        prev_period_id: Set(None),
    }
}

fn line(tenant: Uuid, entry: Uuid, text: &str, side: Side) -> journal_line::ActiveModel {
    journal_line::ActiveModel {
        line_id: Set(Uuid::now_v7()),
        entry_id: Set(entry),
        tenant_id: Set(tenant),
        period_id: Set("2026-10".into()),
        payer_tenant_id: Set(tenant),
        seller_tenant_id: Set(None),
        resource_tenant_id: Set(None),
        account_id: Set(Uuid::now_v7()),
        account_class: Set(AccountClass::Revenue.as_str().into()),
        gl_code: Set(None),
        side: Set(side.as_str().into()),
        amount: Set(text.into()),
        currency: Set("EUR".into()),
        currency_scale: Set(2),
        invoice_id: Set(Some("i".into())),
        due_date: Set(None),
        revenue_stream: Set(Some("service".into())),
        mapping_status: Set("RESOLVED".into()),
        functional_amount: Set(None),
        functional_currency: Set(None),
        functional_currency_scale: Set(None),
        tax_jurisdiction: Set(None),
        tax_filing_period: Set(None),
        tax_rate_ref: Set(None),
        legal_entity_id: Set(None),
        invoice_item_ref: Set(None),
        sku_or_plan_ref: Set(None),
        price_id: Set(None),
        pricing_snapshot_ref: Set(None),
        po_allocation_group: Set(None),
        credit_grant_event_type: Set(None),
        ar_status: Set(None),
        rate_snapshot_ref: Set(None),
    }
}

#[tokio::test]
async fn journal_disaggregation_exact_cancellation_currency_grains_and_scale_validation() {
    let (repo, db, tenant) = setup().await;
    let entry = Uuid::now_v7();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let h = header(tenant, entry);
            journal_entry::Entity::insert(h.clone())
                .secure()
                .scope_with_model(&s, &h)
                .unwrap()
                .exec(tx)
                .await
                .unwrap();
            // Intermediate 2*max exceeds PostedMoney but final exact sum is max.
            for (code, scale, text, side) in [
                ("EUR", 2, "9999999999999999999999999999", Side::Credit),
                ("EUR", 2, "9999999999999999999999999999", Side::Credit),
                ("EUR", 2, "9999999999999999999999999999", Side::Debit),
                ("JPY", 0, "2", Side::Credit),
                ("JPY", 0, "1", Side::Debit),
                ("TOK", 28, "0.0000000000000000000000000001", Side::Credit),
            ] {
                let line_entry = if code == "EUR" {
                    entry
                } else {
                    let id = Uuid::now_v7();
                    let mut h = header(tenant, id);
                    h.entry_currency = Set(code.into());
                    h.created_seq = Set(if code == "JPY" { 2 } else { 3 });
                    journal_entry::Entity::insert(h.clone())
                        .secure()
                        .scope_with_model(&s, &h)
                        .unwrap()
                        .exec(tx)
                        .await
                        .unwrap();
                    id
                };
                let mut l = line(tenant, line_entry, text, side);
                l.currency = Set(code.into());
                l.currency_scale = Set(scale);
                journal_line::Entity::insert(l.clone())
                    .secure()
                    .scope_with_model(&s, &l)
                    .unwrap()
                    .exec(tx)
                    .await
                    .unwrap();
            }
            // A different source type must not contribute to recognition reporting.
            let foreign_entry = Uuid::now_v7();
            let mut h = header(tenant, foreign_entry);
            h.source_doc_type = Set(SourceDocType::InvoicePost.as_str().into());
            h.created_seq = Set(2);
            journal_entry::Entity::insert(h.clone())
                .secure()
                .scope_with_model(&s, &h)
                .unwrap()
                .exec(tx)
                .await
                .unwrap();
            let l = line(tenant, foreign_entry, "9", Side::Credit);
            journal_line::Entity::insert(l.clone())
                .secure()
                .scope_with_model(&s, &l)
                .unwrap()
                .exec(tx)
                .await
                .unwrap();
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let s = AccessScope::for_tenant(tenant);
    let result = repo
        .list_revenue_disaggregation(&s, tenant, Some("2026-10"))
        .await
        .unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].recognized, money("9999999999999999999999999999"));
    assert_eq!(result[1].recognized, specified("1", "JPY", 0));
    assert_eq!(
        result[2].recognized,
        specified("0.0000000000000000000000000001", "TOK", 28)
    );
    assert_eq!(result[0].period_id, "2026-10");
    assert!(
        repo.list_revenue_disaggregation(&s, tenant, Some("202601"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.list_revenue_disaggregation(&AccessScope::for_tenant(Uuid::now_v7()), tenant, None)
            .await
            .unwrap()
            .is_empty()
    );
    // A conflicting scale on the same currency grain is corruption, not a new bucket.
    let conn = repo.db.conn().unwrap();
    let mut bad = line(tenant, entry, "0.1", Side::Credit);
    bad.currency_scale = Set(3);
    let Set(bad_id) = bad.line_id.clone() else {
        unreachable!()
    };
    journal_line::Entity::insert(bad.clone())
        .secure()
        .scope_with_model(&s, &bad)
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        repo.list_revenue_disaggregation(&s, tenant, None).await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    // Remove metadata mismatch via secure fixture update, then final bound is rejected.
    journal_line::Entity::update_many()
        .secure()
        .scope_with(&s)
        .col_expr(journal_line::Column::CurrencyScale, Expr::value(2_i16))
        .filter(journal_line::Column::LineId.eq(bad_id).into())
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        repo.list_revenue_disaggregation(&s, tenant, None).await,
        Err(RepoError::Money(MoneyError::AmountOutOfRange))
    ));
}

#[tokio::test]
async fn segment_delta_stale_release_and_completion_stale_money_both_directions() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("1")))
                .await?;
            r.insert_segments(tx, &s, &[segment(tenant, "s", 1, money("1"))])
                .await?;
            let old_seg = r.required_segment(tx, &s, tenant, "s", 1).await?;
            r.add_pending_segment_amount(tx, &s, tenant, "s", 1, &money("1"))
                .await?;
            assert!(matches!(
                r.write_segment(
                    tx,
                    &s,
                    &old_seg,
                    &old_seg.amount,
                    "DONE",
                    Some(OffsetDateTime::now_utc()),
                    Some(Uuid::now_v7())
                )
                .await,
                Err(RepoError::Conflict(_))
            ));
            r.add_recognized(tx, &s, tenant, "s", &money("1")).await?;
            let old_schedule = r.required_schedule(tx, &s, tenant, "s").await?;
            r.increase_total_deferred(tx, &s, tenant, "s", &money("1"))
                .await?;
            assert!(matches!(
                r.write_schedule(
                    tx,
                    &s,
                    &old_schedule,
                    &old_schedule.total_deferred,
                    &old_schedule.recognized,
                    "COMPLETED",
                    old_schedule.version
                )
                .await,
                Err(RepoError::Conflict(_))
            ));
            assert_eq!(
                r.required_schedule(tx, &s, tenant, "s").await?.status,
                "ACTIVE"
            );
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    // Standalone queue is also a real atomic CAS writer, no extra bump on replay.
    let s = AccessScope::for_tenant(tenant);
    repo.mark_segment_queued(&s, tenant, "s", 1).await.unwrap();
    repo.mark_segment_queued(&s, tenant, "s", 1).await.unwrap();
    let row = repo.list_segments(&s, tenant, "s").await.unwrap().remove(0);
    assert_eq!(
        (row.amount, row.version, row.status.as_str()),
        (money("2"), 2, "QUEUED")
    );
}

#[tokio::test]
async fn decoder_rejects_schema_protected_corruption() {
    use sea_orm::TryIntoModel;
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("1")))
                .await?;
            r.insert_segments(tx, &s, &[segment(tenant, "s", 1, money("1"))])
                .await?;
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let s = AccessScope::for_tenant(tenant);
    let conn = repo.db.conn().unwrap();
    let raw = recognition_schedule::Entity::find()
        .secure()
        .scope_with(&s)
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    for bad in [
        recognition_schedule::Model {
            recognized: "-1".into(),
            ..raw.clone()
        },
        recognition_schedule::Model {
            total_deferred: "-1".into(),
            ..raw.clone()
        },
        recognition_schedule::Model {
            currency_scale: 29,
            ..raw.clone()
        },
        recognition_schedule::Model {
            currency: "bad currency".into(),
            ..raw.clone()
        },
        recognition_schedule::Model {
            version: -1,
            ..raw.clone()
        },
        recognition_schedule::Model {
            status: "UNKNOWN".into(),
            ..raw.clone()
        },
    ] {
        assert!(matches!(
            decode_schedule(bad),
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
    let parent = decode_schedule(raw).unwrap();
    let raw = recognition_segment::Entity::find()
        .secure()
        .scope_with(&s)
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    for bad in [
        recognition_segment::Model {
            amount: "-1".into(),
            ..raw.clone()
        },
        recognition_segment::Model {
            currency: "USD".into(),
            ..raw.clone()
        },
        recognition_segment::Model {
            version: -1,
            ..raw.clone()
        },
        recognition_segment::Model {
            status: "UNKNOWN".into(),
            ..raw.clone()
        },
    ] {
        assert!(matches!(
            decode_segment(bad, Some(&parent)),
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
    assert!(matches!(
        decode_segment(raw, None),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    let mut bad_line = line(tenant, Uuid::now_v7(), "-1", Side::Credit)
        .try_into_model()
        .unwrap();
    assert!(matches!(
        fold_revenue(vec![bad_line.clone()]),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    bad_line.amount = "1".into();
    bad_line.functional_amount = Some("1".into());
    assert!(matches!(
        fold_revenue(vec![bad_line]),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    assert!(matches!(cas(0), Err(RepoError::Conflict(_))));
    assert!(matches!(cas(2), Err(RepoError::Conflict(_))));
}

#[tokio::test]
async fn simultaneous_sqlite_queue_beats_amount_writer_with_typed_conflict() {
    let path = std::env::temp_dir().join(format!("ledger-recognition-{}.sqlite", Uuid::new_v4()));
    let dsn = format!("sqlite://{}?mode=rwc&busy_timeout=1", path.display());
    let db = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let repo = RecognitionRepo::new(DBProvider::new(db.clone()));
    let tenant = Uuid::new_v4();
    retry_transaction(&db, |tx| {
        let r = repo.clone();
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("1")))
                .await?;
            r.insert_segments(tx, &s, &[segment(tenant, "s", 1, money("1"))])
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let contender_db = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    let contender = RecognitionRepo::new(DBProvider::new(contender_db.clone()));
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let held = tokio::spawn(async move {
        db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                repo.mark_segment_queued_in(tx, &AccessScope::for_tenant(tenant), tenant, "s", 1)
                    .await?;
                locked_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok::<_, AttemptError>(())
            })
        })
        .await
        .unwrap();
    });
    locked_rx.await.unwrap();
    let result = retry_transaction(&contender_db, |tx| {
        let r = contender.clone();
        Box::pin(async move {
            r.add_pending_segment_amount(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                "s",
                1,
                &money("1"),
            )
            .await?;
            Ok(())
        })
    })
    .await;
    release_tx.send(()).unwrap();
    held.await.unwrap();
    assert!(matches!(
        result,
        Err(crate::domain::error::DomainError::ConcurrentModification(_))
    ));
    let row = contender
        .list_segments(&AccessScope::for_tenant(tenant), tenant, "s")
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (row.status.as_str(), row.version, row.amount),
        ("QUEUED", 1, money("1"))
    );
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = attempts.clone();
    let result = retry_transaction(&contender_db, |tx| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let r = contender.clone();
        Box::pin(async move {
            r.add_pending_segment_amount(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                "s",
                1,
                &money("1"),
            )
            .await?;
            Ok(())
        })
    })
    .await;
    assert!(matches!(
        result,
        Err(crate::domain::error::DomainError::RecognitionPolicyConflict(_))
    ));
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(contender);
    drop(contender_db);
    // Best effort: Windows keeps the file locked until the pool's async close completes.
    let _ = std::fs::remove_file(&path);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test]
async fn failed_segment_batch_rolls_back_parent_and_prior_segments() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    let result = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let s = AccessScope::for_tenant(tenant);
                r.insert_schedule(tx, &s, &schedule(tenant, "s", money("2")))
                    .await?;
                r.insert_segments(
                    tx,
                    &s,
                    &[
                        segment(tenant, "s", 1, money("1")),
                        segment(tenant, "s", 2, specified("1", "EUR", 3)),
                    ],
                )
                .await?;
                Ok::<_, AttemptError>(())
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::InconsistentScale(_)
        ))
    ));
    let s = AccessScope::for_tenant(tenant);
    assert!(repo.read_schedule(&s, tenant, "s").await.unwrap().is_none());
    assert!(
        repo.list_segments(&s, tenant, "s")
            .await
            .unwrap()
            .is_empty()
    );
}

/// An observation edited at the current version (the state types are plain
/// data) is a conflict: every observed write pins the money, metadata and
/// period it read, not only the version and status, so it can neither rewrite
/// stored amounts nor complete a schedule the stored row says is not drained.
#[tokio::test]
async fn observed_writes_refuse_an_edited_observation() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let at = OffsetDateTime::now_utc();
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("0.3")))
                .await?;
            r.insert_segments(
                tx,
                &s,
                &[
                    segment(tenant, "s", 1, money("0.1")),
                    segment(tenant, "s", 2, money("0.2")),
                ],
            )
            .await?;
            let observed = r.required_schedule(tx, &s, tenant, "s").await?;
            let first = r
                .read_segment_of(tx, &s, &observed, 1)
                .await?
                .expect("segment 1");

            let inflated = ScheduleState {
                total_deferred: money("9.00"),
                ..observed.clone()
            };
            assert!(matches!(
                r.add_recognized_to(tx, &s, &inflated, &money("0.1")).await,
                Err(RepoError::Conflict(_))
            ));
            let drained = ScheduleState {
                recognized: money("0.3"),
                ..observed.clone()
            };
            assert!(matches!(
                r.complete_observed_schedule_if_drained(tx, &s, &drained)
                    .await,
                Err(RepoError::Conflict(_))
            ));
            let rescaled = ScheduleState {
                total_deferred: specified("0.3", "EUR", 4),
                recognized: specified("0", "EUR", 4),
                ..observed.clone()
            };
            assert!(matches!(
                r.add_recognized_to(tx, &s, &rescaled, &specified("0.0001", "EUR", 4))
                    .await,
                Err(RepoError::Conflict(_))
            ));

            let repriced = SegmentState {
                amount: money("5.00"),
                ..first.clone()
            };
            assert!(matches!(
                r.stamp_observed_segment_done(tx, &s, &repriced, Uuid::now_v7(), at)
                    .await,
                Err(RepoError::Conflict(_))
            ));
            let moved = SegmentState {
                period_id: "202500".into(),
                ..first.clone()
            };
            assert!(matches!(
                r.stamp_observed_segment_done(tx, &s, &moved, Uuid::now_v7(), at)
                    .await,
                Err(RepoError::Conflict(_))
            ));

            assert_eq!(
                observed,
                r.required_schedule(tx, &s, tenant, "s").await?,
                "no edited observation reached the schedule row"
            );
            assert_eq!(
                Some(first.clone()),
                r.read_segment_in(tx, &s, tenant, "s", 1).await?,
                "no edited observation reached the segment row"
            );
            r.add_recognized_to(tx, &s, &observed, &money("0.1"))
                .await?;
            r.stamp_observed_segment_done(tx, &s, &first, Uuid::now_v7(), at)
                .await?;
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

/// A release mutates the schedule and segment it read once: each write CASes
/// on that observation, the returned schedule is the stored row, and a stale
/// observation is a conflict instead of a fresh read that would hide it.
#[tokio::test]
async fn observed_release_writes_cas_on_the_state_read_once() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let at = OffsetDateTime::now_utc();
            r.insert_schedule(tx, &s, &schedule(tenant, "s", money("0.3")))
                .await?;
            r.insert_segments(
                tx,
                &s,
                &[
                    segment(tenant, "s", 1, money("0.1")),
                    segment(tenant, "s", 2, money("0.2")),
                ],
            )
            .await?;
            let observed = r.required_schedule(tx, &s, tenant, "s").await?;
            let first = r
                .read_segment_of(tx, &s, &observed, 1)
                .await?
                .expect("segment 1");
            assert_eq!(
                Some(first.clone()),
                r.read_segment_in(tx, &s, tenant, "s", 1).await?,
                "validated against the supplied parent, same state"
            );
            assert!(r.read_segment_of(tx, &s, &observed, 9).await?.is_none());

            let written = r
                .add_recognized_to(tx, &s, &observed, &money("0.1"))
                .await?;
            assert_eq!(
                written,
                r.required_schedule(tx, &s, tenant, "s").await?,
                "the returned schedule is the stored row"
            );
            assert!(matches!(
                r.add_recognized_to(tx, &s, &observed, &money("0.1")).await,
                Err(RepoError::Conflict(_))
            ));
            assert!(matches!(
                r.add_recognized_to(tx, &s, &written, &money("0.3")).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            assert!(
                !r.complete_observed_schedule_if_drained(tx, &s, &written)
                    .await?
            );
            r.stamp_observed_segment_done(tx, &s, &first, Uuid::now_v7(), at)
                .await?;
            assert!(matches!(
                r.stamp_observed_segment_done(tx, &s, &first, Uuid::now_v7(), at)
                    .await,
                Err(RepoError::Conflict(_))
            ));

            let second = r
                .read_segment_of(tx, &s, &written, 2)
                .await?
                .expect("segment 2");
            let written = r.add_recognized_to(tx, &s, &written, &money("0.2")).await?;
            r.stamp_observed_segment_done(tx, &s, &second, Uuid::now_v7(), at)
                .await?;
            assert!(
                r.complete_observed_schedule_if_drained(tx, &s, &written)
                    .await?
            );
            let done = r.required_schedule(tx, &s, tenant, "s").await?;
            assert_eq!(done.status, SCHEDULE_STATUS_COMPLETED);
            assert_eq!(
                done.version, written.version,
                "completion keeps the version"
            );
            assert!(matches!(
                r.complete_observed_schedule_if_drained(tx, &s, &written)
                    .await,
                Err(RepoError::Conflict(_))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}
