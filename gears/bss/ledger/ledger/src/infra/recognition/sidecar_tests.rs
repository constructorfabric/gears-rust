//! Actual-source sidecars with migrated SQLite and the real posting transaction.
use super::*;
use crate::domain::recognition::builder::PlannedSegment;
use crate::infra::posting::service::decimal_tests::{counts, money, setup};
use crate::infra::storage::entity::{
    account_balance, chain_state, idempotency_dedup, journal_entry, journal_line,
    recognition_schedule, recognition_segment,
};
use sea_orm::EntityTrait;
use toolkit_db::secure::{Db, SecureEntityExt, SecureUpdateExt};
use toolkit_db::{DBProvider, DbError};

fn plan(parts: &[(&str, &str)]) -> PlannedScheduleMaterialization {
    let total = parts.iter().fold(rust_decimal::Decimal::ZERO, |a, (_, b)| {
        a + bss_ledger_sdk::parse_decimal(b).unwrap()
    });
    PlannedScheduleMaterialization {
        schedule: BuiltSchedule {
            deferred: PostedMoney::try_new(
                total,
                bss_ledger_sdk::CurrencySpec::try_new("EUR".into(), 2).unwrap(),
            )
            .unwrap(),
            segments: parts
                .iter()
                .enumerate()
                .map(|(n, (period, value))| PlannedSegment {
                    segment_no: i32::try_from(n + 1).unwrap(),
                    period_id: (*period).into(),
                    amount: money(value, "EUR", 2),
                })
                .collect(),
            policy_ref: "straight-line".into(),
            ssp_snapshot_ref: None,
            po_allocation_group: None,
            subscription_ref: None,
            vc_estimate_ref: None,
            vc_method_ref: None,
            revenue_stream: "test".into(),
        },
        source_invoice_item_ref: "item".into(),
    }
}
fn builder(
    db: &Db,
    tenant: Uuid,
    plans: Vec<PlannedScheduleMaterialization>,
    discriminator: Option<&str>,
) -> ScheduleBuilderSidecar {
    ScheduleBuilderSidecar {
        tenant_id: tenant,
        payer_tenant_id: tenant,
        source_invoice_id: "invoice".into(),
        schedules: plans,
        idempotency: IdempotencyGate::new(),
        recognition_repo: Arc::new(RecognitionRepo::new(DBProvider::<DbError>::new(db.clone()))),
        max_segments_per_schedule: 120,
        build_discriminator: discriminator.map(str::to_owned),
    }
}
async fn rows(db: &Db) -> (u64, u64) {
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
    )
}
async fn state(
    db: &Db,
    tenant: Uuid,
) -> (
    crate::infra::storage::repo::recognition_repo::ScheduleState,
    Vec<crate::infra::storage::repo::recognition_repo::SegmentState>,
) {
    let repo = RecognitionRepo::new(DBProvider::<DbError>::new(db.clone()));
    let scope = AccessScope::allow_all();
    let schedule = repo
        .read_active_schedule_in_txn(
            &db.conn().unwrap(),
            &scope,
            tenant,
            "invoice",
            "item",
            "test",
        )
        .await
        .unwrap()
        .unwrap();
    let segments = repo
        .list_segments(&scope, tenant, &schedule.schedule_id)
        .await
        .unwrap();
    (schedule, segments)
}

#[tokio::test]
async fn materialization_replay_and_changed_plan_claim_are_money_bound() {
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    let plans = vec![plan(&[("202610", "0.1"), ("202611", "0.2")])];
    let sidecar = Arc::new(builder(&db, tenant, plans.clone(), None));
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(sidecar.clone()),
    )
    .await
    .unwrap();
    let (schedule, segments) = state(&db, tenant).await;
    assert_eq!(schedule.total_deferred, money("0.3", "EUR", 2));
    assert_eq!(schedule.version, 0);
    assert_eq!(
        segments
            .iter()
            .map(|s| s.amount.clone())
            .collect::<Vec<_>>(),
        vec![money("0.1", "EUR", 2), money("0.2", "EUR", 2)]
    );
    let before = counts(&db).await;
    svc.post(&ctx, &scope, entry.clone(), lines.clone(), Some(sidecar))
        .await
        .unwrap();
    assert_eq!(counts(&db).await, before);
    // A separate outer journal claim reaches the existing inner build claim.
    entry.entry_id = Uuid::now_v7();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    entry.source_business_id = "second".into();
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(&db, tenant, plans, None))),
    )
    .await
    .unwrap();
    assert_eq!(rows(&db).await, (1, 2));
    let before = counts(&db).await;
    for changed in [
        plan(&[("202610", "0.11"), ("202611", "0.2")]),
        {
            let mut p = plan(&[("202610", "0.1"), ("202611", "0.2")]);
            p.schedule.deferred = money("0.3", "EUR", 3);
            for s in &mut p.schedule.segments {
                s.amount = money(
                    &bss_ledger_sdk::canonical_decimal(s.amount.amount()),
                    "EUR",
                    3,
                );
            }
            p
        },
        {
            let mut p = plan(&[("202610", "0.1"), ("202611", "0.2")]);
            p.schedule.deferred = money("0.3", "USD", 2);
            for s in &mut p.schedule.segments {
                s.amount = money(
                    &bss_ledger_sdk::canonical_decimal(s.amount.amount()),
                    "USD",
                    2,
                );
            }
            p
        },
    ] {
        entry.entry_id = Uuid::now_v7();
        for line in &mut lines {
            line.line_id = Uuid::now_v7();
        }
        entry.source_business_id = Uuid::now_v7().to_string();
        assert!(matches!(
            svc.post(
                &ctx,
                &scope,
                entry.clone(),
                lines.clone(),
                Some(Arc::new(builder(&db, tenant, vec![changed], None)))
            )
            .await,
            Err(DomainError::IdempotencyConflict(_))
        ));
        assert_eq!(counts(&db).await, before);
        assert_eq!(rows(&db).await, (1, 2));
        assert_eq!(
            state(&db, tenant).await,
            (schedule.clone(), segments.clone())
        );
    }
}

#[tokio::test]
async fn extension_merges_pending_period_and_appends_checked_number() {
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.1")])],
            None,
        ))),
    )
    .await
    .unwrap();
    entry.entry_id = Uuid::now_v7();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    entry.source_business_id = "note".into();
    svc.post(
        &ctx,
        &scope,
        entry,
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.2"), ("202611", "0.3")])],
            Some("note"),
        ))),
    )
    .await
    .unwrap();
    let (schedule, segments) = state(&db, tenant).await;
    assert_eq!(schedule.total_deferred, money("0.6", "EUR", 2));
    assert_eq!(schedule.version, 1);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].amount, money("0.3", "EUR", 2));
    assert_eq!(segments[0].version, 1);
    assert_eq!(segments[1].segment_no, 2);
    assert_eq!(segments[1].amount, money("0.3", "EUR", 2));
}

/// A labelled invalid plan set and the error variant it must fail with.
type InvalidPlanCase = (
    &'static str,
    Vec<PlannedScheduleMaterialization>,
    fn(&DomainError) -> bool,
);

#[tokio::test]
async fn invalid_plans_fail_without_journal_chain_dedup_or_schedule_effects() {
    let (svc, db, entry, lines) = setup().await;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    let valid = plan(&[("202610", "0.3")]);
    let conflict = |e: &DomainError| matches!(e, DomainError::RecognitionPolicyConflict(_));
    let mut invalid: Vec<InvalidPlanCase> = vec![];
    let mut p = valid.clone();
    p.schedule.deferred = money("0.4", "EUR", 2);
    invalid.push(("segment total differs", vec![p], conflict));
    let mut p = valid.clone();
    p.schedule.segments[0].amount = money("0.3", "EUR", 3);
    invalid.push(("segment scale differs", vec![p], |e| {
        matches!(e, DomainError::InconsistentScale(_))
    }));
    let mut p = valid.clone();
    p.schedule.segments[0].amount = money("0.3", "USD", 2);
    invalid.push(("segment currency differs", vec![p], |e| {
        matches!(e, DomainError::CurrencyMismatch(_))
    }));
    let mut p = valid.clone();
    p.schedule.segments[0].segment_no = i32::MAX;
    invalid.push(("segment out of sequence", vec![p], conflict));
    let mut p = valid.clone();
    p.schedule.segments[0].period_id = "invalid".into();
    invalid.push(("malformed period", vec![p], conflict));
    let mut p = valid.clone();
    p.schedule.segments.clear();
    invalid.push(("no segments", vec![p], |e| {
        matches!(e, DomainError::ScheduleTooLong(_))
    }));
    let mut p = valid.clone();
    p.schedule.deferred = money("-0.3", "EUR", 2);
    invalid.push(("negative deferred", vec![p], conflict));
    invalid.push((
        "duplicate item and stream",
        vec![valid.clone(), valid.clone()],
        conflict,
    ));
    invalid.push((
        "descending periods",
        vec![plan(&[("202611", "0.1"), ("202610", "0.2")])],
        conflict,
    ));
    invalid.push((
        "repeated period",
        vec![plan(&[("202610", "0.1"), ("202610", "0.2")])],
        conflict,
    ));
    invalid.push((
        "negative segment",
        vec![plan(&[("202610", "0.5"), ("202611", "-0.2")])],
        conflict,
    ));
    for (why, plans, expected) in invalid {
        let result = svc
            .post(
                &ctx,
                &scope,
                entry.clone(),
                lines.clone(),
                Some(Arc::new(builder(&db, entry.tenant_id, plans, None))),
            )
            .await;
        match result {
            Err(error) => assert!(expected(&error), "{why}: unexpected {error:?}"),
            Ok(posted) => panic!("{why}: an invalid plan posted {posted:?}"),
        }
        assert_eq!(counts(&db).await, (0, 0, 0, 0), "{why}");
        assert_eq!(rows(&db).await, (0, 0), "{why}");
    }
}

fn stamp(
    db: &Db,
    tenant: Uuid,
    schedule_id: &str,
    segment_no: i32,
    amount: PostedMoney,
    schedule_version: i64,
    segment_version: i64,
) -> RecognitionStampSidecar {
    RecognitionStampSidecar {
        tenant_id: tenant,
        schedule_id: schedule_id.into(),
        segment_no,
        period_id: "202610".into(),
        amount,
        revenue_stream: "test".into(),
        expected_schedule_version: schedule_version,
        expected_segment_version: segment_version,
        run_id: Uuid::now_v7(),
        recognition_repo: Arc::new(RecognitionRepo::new(DBProvider::<DbError>::new(db.clone()))),
        publisher: Arc::new(LedgerEventPublisher::noop()),
        ctx: SecurityContext::anonymous(),
    }
}

#[allow(clippy::too_many_lines)] // one scenario end to end; splitting hides the state transitions
#[tokio::test]
async fn release_rejects_snapshot_mismatch_then_completes_and_historically_reverses() {
    use crate::infra::posting::retry::retry_transaction;
    use crate::infra::posting::service::ClaimSpec;
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    for line in &mut lines {
        line.money = money("0.3", "EUR", 2);
    }
    // Actual funding/release account classes; the historical seam reverses these stored legs.
    let liability = Uuid::now_v7();
    crate::infra::storage::repo::ReferenceRepo::new(DBProvider::<DbError>::new(db.clone()))
        .insert_account(crate::domain::model::AccountRow {
            account_id: liability,
            tenant_id: tenant,
            legal_entity_id: tenant,
            account_class: bss_ledger_sdk::AccountClass::ContractLiability
                .as_str()
                .into(),
            currency: "EUR".into(),
            revenue_stream: Some("test".into()),
            normal_side: "CR".into(),
            may_go_negative: false,
            lifecycle_state: "OPEN".into(),
        })
        .await
        .unwrap();
    let mut funding = lines.clone();
    funding[1].account_id = liability;
    funding[1].account_class = bss_ledger_sdk::AccountClass::ContractLiability;
    lines[0].account_id = liability;
    lines[0].account_class = bss_ledger_sdk::AccountClass::ContractLiability;
    entry.source_doc_type = SourceDocType::InvoicePost;
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        funding,
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.3")])],
            None,
        ))),
    )
    .await
    .unwrap();
    let (schedule, segments) = state(&db, tenant).await;
    entry.entry_id = Uuid::now_v7();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    entry.source_doc_type = SourceDocType::Recognition;
    entry.source_business_id = "release".into();
    let before = counts(&db).await;
    for (amount, sv, segv, category) in [
        (money("0.2", "EUR", 2), 0, 0, "amount"),
        (money("0.3", "USD", 2), 0, 0, "currency"),
        (money("0.3", "EUR", 3), 0, 0, "scale"),
        (money("0.3", "EUR", 2), 1, 0, "version"),
        (money("0.3", "EUR", 2), 0, 1, "version"),
    ] {
        let error = svc
            .post(
                &ctx,
                &scope,
                entry.clone(),
                lines.clone(),
                Some(Arc::new(stamp(
                    &db,
                    tenant,
                    &schedule.schedule_id,
                    1,
                    amount,
                    sv,
                    segv,
                ))),
            )
            .await
            .unwrap_err();
        match category {
            "amount" => assert!(
                matches!(error, DomainError::RecognitionPolicyConflict(_)),
                "{error:?}"
            ),
            "currency" => assert!(
                matches!(error, DomainError::CurrencyMismatch(_)),
                "{error:?}"
            ),
            "scale" => assert!(
                matches!(error, DomainError::InconsistentScale(_)),
                "{error:?}"
            ),
            _ => assert!(
                matches!(error, DomainError::ConcurrentModification(_)),
                "{error:?}"
            ),
        }
        assert_eq!(counts(&db).await, before);
        assert_eq!(
            state(&db, tenant).await,
            (schedule.clone(), segments.clone())
        );
    }
    let release = svc
        .post(
            &ctx,
            &scope,
            entry.clone(),
            lines.clone(),
            Some(Arc::new(stamp(
                &db,
                tenant,
                &schedule.schedule_id,
                1,
                money("0.3", "EUR", 2),
                0,
                0,
            ))),
        )
        .await
        .unwrap();
    let repo = RecognitionRepo::new(DBProvider::<DbError>::new(db.clone()));
    let completed = repo
        .read_schedule(&scope, tenant, &schedule.schedule_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "COMPLETED");
    assert_eq!(completed.recognized, completed.total_deferred);
    assert_eq!(completed.version, 1);
    let done = repo
        .list_segments(&scope, tenant, &schedule.schedule_id)
        .await
        .unwrap();
    assert_eq!(done[0].status, "DONE");
    assert_eq!(done[0].version, 1);
    assert!(done[0].recognized_at.is_some());
    assert!(done[0].run_id.is_some());
    let after = counts(&db).await;
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(stamp(
            &db,
            tenant,
            &schedule.schedule_id,
            1,
            money("0.3", "EUR", 2),
            0,
            0,
        ))),
    )
    .await
    .unwrap();
    assert_eq!(counts(&db).await, after);
    entry.entry_id = Uuid::now_v7();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    entry.source_business_id = "reversal".into();
    entry.reverses_entry_id = Some(release.entry_id);
    entry.reverses_period_id = Some("202610".into());
    let reversal = Arc::new(RecognitionReversalSidecar {
        tenant_id: tenant,
        schedule_id: schedule.schedule_id.clone(),
        segment_no: 1,
        period_id: "202610".into(),
        amount: done[0].amount.clone(),
        revenue_stream: "test".into(),
        expected_schedule_version: 1,
        expected_segment_version: 1,
        recognition_repo: Arc::new(repo),
        publisher: Arc::new(LedgerEventPublisher::noop()),
        ctx: ctx.clone(),
    });
    for replay in [false, true] {
        let svc = svc.clone();
        let ctx = ctx.clone();
        let scope = scope.clone();
        let entry = entry.clone();
        let reversal = reversal.clone();
        let result = retry_transaction(&db, move |tx| {
            let svc = svc.clone();
            let ctx = ctx.clone();
            let scope = scope.clone();
            let entry = entry.clone();
            let reversal = reversal.clone();
            Box::pin(async move {
                svc.post_reversal_once(&ctx, tx, &scope, entry, Some(reversal), ClaimSpec::fresh())
                    .await
            })
        })
        .await
        .unwrap();
        assert_eq!(result.replayed, replay);
    }
    let repo = RecognitionRepo::new(DBProvider::<DbError>::new(db.clone()));
    let reversed = repo
        .read_schedule(&scope, tenant, &schedule.schedule_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reversed.status, "COMPLETED");
    assert_eq!(reversed.version, 2);
    assert_eq!(reversed.recognized, money("0", "EUR", 2));
    assert_eq!(
        repo.list_segments(&scope, tenant, &schedule.schedule_id)
            .await
            .unwrap(),
        done
    );
}

#[tokio::test]
async fn late_predecessor_failure_rolls_back_recognized_and_entire_post() {
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.1"), ("202611", "0.2")])],
            None,
        ))),
    )
    .await
    .unwrap();
    let before = counts(&db).await;
    let financial_before = posting_snapshot(&db).await;
    let (schedule, segments) = state(&db, tenant).await;
    entry.entry_id = Uuid::now_v7();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    entry.source_business_id = "late".into();
    for line in &mut lines {
        line.money = money("0.2", "EUR", 2);
    }
    assert!(matches!(
        svc.post(
            &ctx,
            &scope,
            entry,
            lines.clone(),
            Some(Arc::new(stamp(
                &db,
                tenant,
                &schedule.schedule_id,
                2,
                money("0.2", "EUR", 2),
                0,
                0
            )))
        )
        .await,
        Err(DomainError::RecognitionPolicyConflict(_))
    ));
    assert_eq!(counts(&db).await, before);
    assert_eq!(posting_snapshot(&db).await, financial_before);
    assert_eq!(state(&db, tenant).await, (schedule, segments));
}

#[tokio::test]
async fn queued_extension_failure_rolls_back_total_and_claim() {
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.3")])],
            None,
        ))),
    )
    .await
    .unwrap();
    let (schedule, _) = state(&db, tenant).await;
    let repo = RecognitionRepo::new(DBProvider::<DbError>::new(db.clone()));
    repo.mark_segment_queued(&scope, tenant, &schedule.schedule_id, 1)
        .await
        .unwrap();
    let snapshot = state(&db, tenant).await;
    let before = counts(&db).await;
    let financial_before = posting_snapshot(&db).await;
    entry.entry_id = Uuid::now_v7();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    entry.source_business_id = "queued-note".into();
    assert!(matches!(
        svc.post(
            &ctx,
            &scope,
            entry,
            lines.clone(),
            Some(Arc::new(builder(
                &db,
                tenant,
                vec![plan(&[("202610", "0.2"), ("202611", "0.1")])],
                Some("note")
            )))
        )
        .await,
        Err(DomainError::RecognitionPolicyConflict(_))
    ));
    assert_eq!(counts(&db).await, before);
    assert_eq!(posting_snapshot(&db).await, financial_before);
    assert_eq!(state(&db, tenant).await, snapshot);
    assert_eq!(rows(&db).await, (1, 1));
}

#[tokio::test]
async fn append_number_exhaustion_rolls_back_schedule_total_and_new_claim() {
    use sea_orm::sea_query::Expr;
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.3")])],
            None,
        ))),
    )
    .await
    .unwrap();
    recognition_segment::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_segment::Column::SegmentNo,
            Expr::value(i32::MAX),
        )
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    let snapshot = state(&db, tenant).await;
    let before = counts(&db).await;
    entry.entry_id = Uuid::now_v7();
    entry.source_business_id = "overflow-note".into();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    assert!(matches!(
        svc.post(
            &ctx,
            &scope,
            entry,
            lines,
            Some(Arc::new(builder(
                &db,
                tenant,
                vec![plan(&[("202611", "0.1")])],
                Some("note")
            )))
        )
        .await,
        Err(DomainError::ScheduleTooLong(_))
    ));
    assert_eq!(counts(&db).await, before);
    assert_eq!(state(&db, tenant).await, snapshot);
}

#[tokio::test]
async fn corrupt_stored_counter_is_internal_and_rolls_back_post() {
    use sea_orm::sea_query::Expr;
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.3")])],
            None,
        ))),
    )
    .await
    .unwrap();
    let (schedule, _) = state(&db, tenant).await;
    recognition_schedule::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(
            recognition_schedule::Column::Recognized,
            Expr::value("broken"),
        )
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    let before = counts(&db).await;
    entry.entry_id = Uuid::now_v7();
    entry.source_business_id = "corrupt-release".into();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    assert!(matches!(
        svc.post(
            &ctx,
            &scope,
            entry,
            lines,
            Some(Arc::new(stamp(
                &db,
                tenant,
                &schedule.schedule_id,
                1,
                money("0.3", "EUR", 2),
                99,
                99
            )))
        )
        .await,
        Err(DomainError::Internal(_))
    ));
    assert_eq!(counts(&db).await, before);
    let stored = recognition_schedule::Entity::find()
        .secure()
        .scope_with(&scope)
        .one(&db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.recognized, "broken");
    assert_eq!(stored.version, 0);
    let segment = recognition_segment::Entity::find()
        .secure()
        .scope_with(&scope)
        .one(&db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(segment.status, "PENDING");
    assert_eq!(segment.version, 0);
}

#[tokio::test]
async fn canonical_plan_hash_preserves_typed_money_and_option_boundaries() {
    let (_, db, entry, _) = setup().await;
    let a = plan(&[("202610", "0.30")]);
    let b = plan(&[("202610", "0.3")]);
    let sidecar = builder(&db, entry.tenant_id, vec![a.clone()], None);
    assert_eq!(sidecar.plan_hash(&a), sidecar.plan_hash(&b));
    let mut changed = b.clone();
    changed.schedule.subscription_ref = Some(String::new());
    assert_ne!(sidecar.plan_hash(&a), sidecar.plan_hash(&changed));
}

/// Full affected posting row contents, including money/spec/version and chain tip.
#[derive(Debug, PartialEq, Eq)]
struct PostingSnapshot {
    balances: Vec<account_balance::Model>,
    headers: Vec<journal_entry::Model>,
    lines: Vec<journal_line::Model>,
    dedup: Vec<idempotency_dedup::Model>,
    chain: Vec<chain_state::Model>,
}

/// Read actual entities without reducing the evidence to row cardinalities.
async fn posting_snapshot(db: &Db) -> PostingSnapshot {
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let mut balances = account_balance::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    let mut headers = journal_entry::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    let mut lines = journal_line::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    let mut dedup = idempotency_dedup::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    let mut chain = chain_state::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    balances.sort_by_key(|r| (r.tenant_id, r.account_id, r.currency.clone()));
    headers.sort_by_key(|r| (r.tenant_id, r.period_id.clone(), r.entry_id));
    lines.sort_by_key(|r| (r.tenant_id, r.period_id.clone(), r.line_id));
    dedup.sort_by_key(|r| (r.tenant_id, r.flow.clone(), r.business_id.clone()));
    chain.sort_by_key(|r| r.tenant_id);
    PostingSnapshot {
        balances,
        headers,
        lines,
        dedup,
        chain,
    }
}

#[tokio::test]
async fn noncanonical_periods_reject_real_post_before_any_build_effects() {
    let (svc, db, entry, lines) = setup().await;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    let before = posting_snapshot(&db).await;
    for period in [
        "2026+1",
        "+12301",
        "-00101",
        "２０２６０１",
        "20261",
        "2026010",
        "202600",
        "202613",
    ] {
        let error = svc
            .post(
                &ctx,
                &scope,
                entry.clone(),
                lines.clone(),
                Some(Arc::new(builder(
                    &db,
                    entry.tenant_id,
                    vec![plan(&[(period, "0.3")])],
                    None,
                ))),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, DomainError::RecognitionPolicyConflict(_)),
            "{period}: {error:?}"
        );
        assert_eq!(posting_snapshot(&db).await, before);
        assert_eq!(rows(&db).await, (0, 0));
    }
}

#[tokio::test]
async fn changed_amount_and_versions_take_retryable_precedence_over_stale_evidence() {
    let (svc, db, mut entry, mut lines) = setup().await;
    let tenant = entry.tenant_id;
    let scope = AccessScope::allow_all();
    let ctx = SecurityContext::anonymous();
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.3")])],
            None,
        ))),
    )
    .await
    .unwrap();
    let original = state(&db, tenant).await;
    entry.entry_id = Uuid::now_v7();
    entry.source_business_id = "extending-note".into();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    svc.post(
        &ctx,
        &scope,
        entry.clone(),
        lines.clone(),
        Some(Arc::new(builder(
            &db,
            tenant,
            vec![plan(&[("202610", "0.2")])],
            Some("note"),
        ))),
    )
    .await
    .unwrap();
    let changed = state(&db, tenant).await;
    assert_eq!(changed.0.total_deferred, money("0.5", "EUR", 2));
    assert_eq!(changed.0.version, 1);
    assert_eq!(changed.1[0].amount, money("0.5", "EUR", 2));
    assert_eq!(changed.1[0].version, 1);
    let before = posting_snapshot(&db).await;
    entry.entry_id = Uuid::now_v7();
    entry.source_business_id = "stale-release".into();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
    }
    for stale_money in [
        original.1[0].amount.clone(),
        money("0.3", "USD", 2),
        money("0.3", "EUR", 3),
    ] {
        let error = svc
            .post(
                &ctx,
                &scope,
                entry.clone(),
                lines.clone(),
                Some(Arc::new(stamp(
                    &db,
                    tenant,
                    &original.0.schedule_id,
                    1,
                    stale_money,
                    original.0.version,
                    original.1[0].version,
                ))),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, DomainError::ConcurrentModification(_)),
            "{error:?}"
        );
        assert_eq!(posting_snapshot(&db).await, before);
        assert_eq!(state(&db, tenant).await, changed);
    }
}
