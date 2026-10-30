//! Actual invoice/core/recognition/FX transactions and immutable historical wrappers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::domain::invoice::builder::TaxBreakdown;
use crate::domain::model::{AccountRow, CurrencyScaleRow, FiscalPeriodRow};
use crate::domain::ports::metrics::NoopLedgerMetrics;
use crate::infra::posting::retry::retry_transaction;
use crate::infra::posting::service::decimal_tests::money;
use crate::infra::storage::entity::{
    account_balance, ar_invoice_balance, ar_payer_balance, chain_state, fiscal_calendar, fx_rate,
    fx_rate_snapshot, idempotency_dedup, journal_entry, journal_line, posting_policy,
    recognition_schedule, recognition_segment, tax_subbalance,
};
use bss_ledger_sdk::{AccountClass, Side};
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use toolkit_db::secure::{Db, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
use toolkit_db::{ConnectOpts, connect_db};

#[async_trait::async_trait]
pub(super) trait AttemptHook: Send + Sync {
    async fn before(&self, _txn: &DbTx<'_>) -> Result<(), AttemptError> {
        Ok(())
    }
    async fn after(&self, _txn: &DbTx<'_>) -> Result<(), AttemptError> {
        Ok(())
    }
}
async fn fixture() -> (Db, InvoicePostService, PostedInvoice) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    seeded(db).await
}
async fn seeded(db: Db) -> (Db, InvoicePostService, PostedInvoice) {
    let provider = DBProvider::new(db.clone());
    let svc = InvoicePostService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(NoopLedgerMetrics),
        RecognitionConfig::default(),
        FxConfig::default(),
    );
    let tenant = Uuid::now_v7();
    // The invoice currency (TKA) and the functional currency (TKB) are non-ISO codes, so
    // the registry row is the only source of their scale. A test can then change the
    // registry and prove that a replay or a reversal keeps the stored scale (an ISO code
    // always keeps its ISO scale, and the registry cannot change it).
    for code in ["TKA", "TKB"] {
        svc.reference
            .upsert_currency_scale(CurrencyScaleRow {
                tenant_id: tenant,
                currency: code.into(),
                currency_scale: 2,
                source: "test".into(),
            })
            .await
            .unwrap();
    }
    for (class, side) in [
        (AccountClass::Ar, Side::Debit),
        (AccountClass::Revenue, Side::Credit),
        (AccountClass::ContractLiability, Side::Credit),
        (AccountClass::TaxPayable, Side::Credit),
        (AccountClass::Suspense, Side::Credit),
    ] {
        svc.reference
            .insert_account(AccountRow {
                account_id: Uuid::now_v7(),
                tenant_id: tenant,
                legal_entity_id: tenant,
                account_class: class.as_str().into(),
                currency: "TKA".into(),
                revenue_stream: class.is_per_stream().then(|| "test".into()),
                normal_side: side.as_str().into(),
                may_go_negative: false,
                lifecycle_state: "OPEN".into(),
            })
            .await
            .unwrap();
    }
    let reference = svc.reference.clone();
    retry_transaction(&db, move |txn| {
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
            Ok(())
        })
    })
    .await
    .unwrap();
    let inv = PostedInvoice {
        invoice_id: Uuid::now_v7().to_string(),
        payer_tenant_id: tenant,
        resource_tenant_id: Some(tenant),
        seller_tenant_id: tenant,
        effective_at: chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        due_date: None,
        period_id: "202610".into(),
        items: vec![InvoiceItem {
            amount_ex_tax: money("12.34", "TKA", 2),
            deferred: money("0", "TKA", 2),
            revenue_stream: "test".into(),
            catalog_class: Some(AccountClass::Revenue),
            contract_class: None,
            gl_code: Some("sales".into()),
            recognition: None,
            invoice_item_ref: Some("item".into()),
            sku_or_plan_ref: Some("sku".into()),
            price_id: Some("price".into()),
            pricing_snapshot_ref: Some("pricing".into()),
        }],
        tax: vec![TaxBreakdown {
            amount: money("1.23", "TKA", 2),
            tax_jurisdiction: "EU".into(),
            tax_filing_period: "2026Q4".into(),
            tax_rate_ref: Some("tax-policy".into()),
        }],
        posted_by_actor_id: tenant,
        correlation_id: Uuid::now_v7(),
    };
    (db, svc, inv)
}
fn deferred(inv: &mut PostedInvoice) {
    inv.items[0].recognition = Some(RecognitionInput {
        policy_ref: "policy-v1".into(),
        timing: RecognitionTiming::StraightLine {
            periods: 3,
            first_period_id: None,
        },
        po_allocation_group: Some("po".into()),
        multi_po: false,
        ssp_snapshot_ref: Some("ssp".into()),
        subscription_ref: Some("sub".into()),
        vc_estimate_ref: None,
        vc_method_ref: None,
        immaterial_one_shot_sku: false,
    });
}
/// Full durable financial state, excluding only the quote/config fixture rows.
async fn state_in<R: toolkit_db::secure::DBRunner>(runner: &R) -> String {
    let scope = AccessScope::allow_all();
    let mut rows = Vec::new();
    macro_rules! read {
        ($entity:ident) => {
            rows.push(format!(
                "{}={:?}",
                stringify!($entity),
                $entity::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .all(runner)
                    .await
                    .unwrap()
            ));
        };
    }
    read!(journal_entry);
    read!(journal_line);
    read!(account_balance);
    read!(ar_invoice_balance);
    read!(ar_payer_balance);
    read!(tax_subbalance);
    read!(chain_state);
    read!(idempotency_dedup);
    read!(recognition_schedule);
    read!(recognition_segment);
    read!(fx_rate_snapshot);
    rows.join("\n")
}
async fn state(db: &Db) -> String {
    state_in(&db.conn().unwrap()).await
}
async fn fx(svc: &InvoicePostService, inv: &PostedInvoice) {
    let tenant = inv.seller_tenant_id;
    let conn = svc.db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant);
    let row = fiscal_calendar::ActiveModel {
        tenant_id: Set(tenant),
        legal_entity_id: Set(tenant),
        fiscal_tz: Set("UTC".into()),
        granularity: Set("MONTH".into()),
        fy_start_month: Set(1),
        functional_currency: Set(Some("TKB".into())),
    };
    fiscal_calendar::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&scope, &row)
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();
    FxRepo::new(svc.db.clone())
        .upsert_rate(&crate::infra::storage::repo::NewFxRate {
            tenant_id: tenant,
            base_currency: "TKA".into(),
            quote_currency: "TKB".into(),
            provider: "ecb".into(),
            rate: bss_ledger_sdk::parse_decimal("1.5").unwrap(),
            as_of: OffsetDateTime::now_utc(),
            fallback_order: 0,
        })
        .await
        .unwrap();
}
#[tokio::test]
async fn typed_invoice_tax_deferral_replay_and_complete_recognition_identity() {
    let (db, svc, mut inv) = fixture().await;
    deferred(&mut inv);
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let ctx = SecurityContext::anonymous();
    let posted = svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    let entry = svc
        .journal
        .find_entry_with_lines(&scope, inv.seller_tenant_id, posted.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        entry
            .lines
            .iter()
            .find(|l| l.account_class == "AR")
            .unwrap()
            .money,
        money("13.57", "TKA", 2)
    );
    assert_eq!(
        entry
            .lines
            .iter()
            .find(|l| l.account_class == "CONTRACT_LIABILITY")
            .unwrap()
            .money,
        money("12.34", "TKA", 2)
    );
    assert_eq!(
        entry
            .lines
            .iter()
            .find(|l| l.account_class == "TAX_PAYABLE")
            .unwrap()
            .tax_rate_ref
            .as_deref(),
        Some("tax-policy")
    );
    let schedules = recognition_schedule::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].total_deferred, "12.34");
    let segments = recognition_segment::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert_eq!(
        segments
            .iter()
            .map(|s| s.amount.as_str())
            .collect::<Vec<_>>(),
        vec!["4.11", "4.11", "4.12"]
    );
    let before = state(&db).await;
    inv.correlation_id = Uuid::now_v7();
    inv.items[0].amount_ex_tax = money("12.3400", "TKA", 2);
    assert!(
        svc.post_invoice(&ctx, &scope, &inv, false)
            .await
            .unwrap()
            .replayed
    );
    inv.items[0].recognition.as_mut().unwrap().policy_ref = "changed".into();
    assert!(matches!(
        svc.post_invoice(&ctx, &scope, &inv, false).await,
        Err(DomainError::IdempotencyConflict(_))
    ));
    assert_eq!(state(&db).await, before);
}
#[tokio::test]
async fn metadata_and_policy_rejections_leave_full_state_untouched() {
    let (db, svc, inv) = fixture().await;
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let ctx = SecurityContext::anonymous();
    let before = state(&db).await;
    let mut bad = inv.clone();
    bad.items[0].amount_ex_tax = money("12.34", "TKA", 3);
    bad.items[0].deferred = money("0", "TKA", 3);
    bad.tax[0].amount = money("1.23", "TKA", 3);
    assert!(matches!(
        svc.post_invoice(&ctx, &scope, &bad, true).await,
        Err(DomainError::InconsistentScale(_))
    ));
    assert_eq!(state(&db).await, before);
    svc.posting_policy_repo
        .write_version(
            &scope,
            inv.seller_tenant_id,
            &crate::domain::invoice::policy::PostingPolicy {
                missing_mapping_mode: MissingMappingMode::HardBlock,
                ..Default::default()
            },
            OffsetDateTime::now_utc() - time::Duration::seconds(1),
        )
        .await
        .unwrap();
    bad = inv.clone();
    bad.items[0].catalog_class = None;
    assert!(matches!(
        svc.post_invoice(&ctx, &scope, &bad, true).await,
        Err(DomainError::AccountMappingMissing(_))
    ));
    assert_eq!(state(&db).await, before);
    assert!(matches!(
        svc.post_invoice(&ctx, &AccessScope::deny_all(), &inv, true)
            .await,
        Err(DomainError::CrossTenantAccessDenied(_))
    ));
    assert_eq!(state(&db).await, before);
}
#[tokio::test]
async fn replay_precedes_changed_policy_registry_accounts_and_payer_but_requires_target() {
    use crate::infra::storage::entity::tenant_account;
    let (db, svc, inv) = fixture().await;
    let tenant = inv.seller_tenant_id;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = SecurityContext::anonymous();
    let original = svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    changed_registry(&db, tenant).await;
    let conn = db.conn().unwrap();
    tenant_account::Entity::update_many()
        .col_expr(
            tenant_account::Column::LifecycleState,
            sea_orm::sea_query::Expr::value("CLOSED"),
        )
        .secure()
        .scope_with(&scope)
        .exec(&conn)
        .await
        .unwrap();
    let before = state(&db).await;
    let replay = svc
        .post_invoice(
            &ctx,
            &AccessScope::for_resource(original.entry_id),
            &inv,
            false,
        )
        .await
        .unwrap();
    assert_eq!(replay.entry_id, original.entry_id);
    assert!(replay.replayed);
    let mut changed = inv.clone();
    changed.items[0].recognition = Some(RecognitionInput {
        policy_ref: "explicit".into(),
        timing: RecognitionTiming::PointInTime,
        po_allocation_group: None,
        multi_po: false,
        ssp_snapshot_ref: None,
        subscription_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
        immaterial_one_shot_sku: false,
    });
    assert!(matches!(
        svc.post_invoice(
            &ctx,
            &AccessScope::for_resource(Uuid::now_v7()),
            &changed,
            false
        )
        .await,
        Err(DomainError::CrossTenantAccessDenied(_))
    ));
    assert!(matches!(
        svc.post_invoice(&ctx, &scope, &changed, false).await,
        Err(DomainError::IdempotencyConflict(_))
    ));
    assert_eq!(state(&db).await, before);
}

fn reversal(inv: &PostedInvoice, original: Uuid) -> PostEntry {
    PostEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: inv.seller_tenant_id,
        period_id: inv.period_id.clone(),
        entry_currency: "TKA".into(),
        source_doc_type: SourceDocType::Reversal,
        source_business_id: crate::domain::invoice::reversal::reversal_business_id(original),
        effective_at: inv.effective_at,
        posted_by_actor_id: inv.posted_by_actor_id,
        correlation_id: Uuid::now_v7(),
        reverses_entry_id: Some(original),
        reverses_period_id: Some(inv.period_id.clone()),
        lines: vec![],
    }
}
async fn historical_case(db: Db, svc: InvoicePostService, inv: PostedInvoice) {
    fx(&svc, &inv).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let original = svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    let before = svc
        .journal
        .find_entry_with_lines(&scope, inv.seller_tenant_id, original.entry_id)
        .await
        .unwrap()
        .unwrap();
    let original_rows = journal_line::Entity::find()
        .filter(journal_line::Column::EntryId.eq(original.entry_id))
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    let snapshot = original_rows[0].rate_snapshot_ref.unwrap();
    assert!(
        before
            .lines
            .iter()
            .all(|l| l.functional_money.as_ref().unwrap().currency().code() == "TKB")
    );
    if db.backend() == sea_orm::DbBackend::Sqlite {
        changed_registry(&db, inv.seller_tenant_id).await;
    } else {
        assert!(matches!(
            svc.reference
                .upsert_currency_scale(CurrencyScaleRow {
                    tenant_id: inv.seller_tenant_id,
                    currency: "TKA".into(),
                    currency_scale: 3,
                    source: "changed".into(),
                })
                .await,
            Err(crate::domain::model::RepoError::CurrencyScaleLocked(_))
        ));
    }
    fx_rate::Entity::update_many()
        .col_expr(fx_rate::Column::Rate, sea_orm::sea_query::Expr::value("9"))
        .secure()
        .scope_with(&scope)
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    // Caller provides no lines; full immutable history is the sole amount source.
    let requested = reversal(&inv, original.entry_id);
    let reversed = svc
        .post_reversal(
            &ctx,
            &scope,
            requested.clone(),
            Some("operator reason".into()),
        )
        .await
        .unwrap();
    let after = svc
        .journal
        .find_entry_with_lines(&scope, inv.seller_tenant_id, reversed.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.legal_entity_id, before.legal_entity_id);
    assert_eq!(after.rounding_evidence, before.rounding_evidence);
    assert_eq!(after.lines.len(), before.lines.len());
    for original in &before.lines {
        let actual = after
            .lines
            .iter()
            .find(|l| l.account_id == original.account_id)
            .unwrap();
        assert_eq!(actual.money, original.money);
        assert_eq!(actual.functional_money, original.functional_money);
        assert_ne!(actual.side, original.side);
        assert_eq!(actual.invoice_id, original.invoice_id);
        assert_eq!(actual.tax_rate_ref, original.tax_rate_ref);
        assert_eq!(actual.invoice_item_ref, original.invoice_item_ref);
    }
    let rows = journal_line::Entity::find()
        .filter(journal_line::Column::EntryId.eq(reversed.entry_id))
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(
        rows.iter()
            .all(|row| row.rate_snapshot_ref == Some(snapshot))
    );
    let balances = account_balance::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(balances.iter().all(|b| b.balance == "0"));
    let stable = state(&db).await;
    assert!(
        svc.post_reversal(
            &ctx,
            &AccessScope::for_resource(original.entry_id),
            requested.clone(),
            Some("operator reason".into())
        )
        .await
        .unwrap()
        .replayed
    );
    for reason in [None, Some("other reason".into()), Some(String::new())] {
        assert!(matches!(
            svc.post_reversal(&ctx, &scope, requested.clone(), reason)
                .await,
            Err(DomainError::IdempotencyConflict(_))
        ));
    }
    let mut invalid = requested.clone();
    invalid.reverses_period_id = Some("202609".into());
    assert!(matches!(
        svc.post_reversal(&ctx, &scope, invalid, None).await,
        Err(DomainError::InvalidRequest(_))
    ));
    assert!(matches!(
        svc.post_reversal(
            &ctx,
            &scope,
            reversal(&inv, reversed.entry_id),
            Some("again".into())
        )
        .await,
        Err(DomainError::InvalidRequest(_))
    ));
    assert_eq!(state(&db).await, stable);
}
#[tokio::test]
async fn historical_full_reversal_preserves_money_dimensions_snapshot_and_reason_identity() {
    let (db, svc, inv) = fixture().await;
    historical_case(db, svc, inv).await;
}

struct LateFailure {
    calls: AtomicUsize,
    states: Mutex<Vec<String>>,
    conflict: bool,
}
#[async_trait::async_trait]
impl AttemptHook for LateFailure {
    async fn after(&self, txn: &DbTx<'_>) -> Result<(), AttemptError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let evidence = state_in(txn).await;
        self.states.lock().unwrap().push(evidence);
        if self.conflict {
            Err(AttemptError::Conflict)
        } else {
            Err(DomainError::OverRecognition("late rejected schedule policy".into()).into())
        }
    }
}
#[tokio::test]
async fn late_named_business_failure_rolls_back_snapshot_schedule_and_complete_post() {
    let (db, mut svc, mut inv) = fixture().await;
    deferred(&mut inv);
    fx(&svc, &inv).await;
    let hook = Arc::new(LateFailure {
        calls: AtomicUsize::new(0),
        states: Mutex::new(vec![]),
        conflict: false,
    });
    svc.attempt_hook = Some(hook.clone());
    let before = state(&db).await;
    let result = svc
        .post_invoice(
            &SecurityContext::anonymous(),
            &AccessScope::for_tenant(inv.seller_tenant_id),
            &inv,
            true,
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::OverRecognition(_))),
        "{result:?}"
    );
    assert_eq!(hook.calls.load(Ordering::SeqCst), 1);
    let evidence = hook.states.lock().unwrap().clone();
    assert_eq!(evidence.len(), 1);
    for table in [
        "journal_entry=[Model",
        "journal_line=[Model",
        "chain_state=[Model",
        "idempotency_dedup=[Model",
        "recognition_schedule=[Model",
        "recognition_segment=[Model",
        "fx_rate_snapshot=[Model",
    ] {
        assert!(evidence[0].contains(table), "{table}: {}", evidence[0]);
    }
    assert_eq!(state(&db).await, before);
}
struct Refresh {
    calls: AtomicUsize,
    observations: Mutex<Vec<String>>,
    tenant: Uuid,
    accounts: [Uuid; 3],
}
#[async_trait::async_trait]
impl AttemptHook for Refresh {
    async fn before(&self, txn: &DbTx<'_>) -> Result<(), AttemptError> {
        use crate::infra::storage::entity::{currency_scale_registry, tenant_account};
        let index = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let scope = AccessScope::for_tenant(self.tenant);
        // No history leaks from an earlier attempt: query the rows themselves
        // (an empty Vec), not a Debug rendering a format change could hide.
        let all = AccessScope::allow_all();
        let entries = journal_entry::Entity::find()
            .secure()
            .scope_with(&all)
            .all(txn)
            .await
            .unwrap();
        assert!(entries.is_empty(), "leaked journal entries: {entries:?}");
        let snapshots = fx_rate_snapshot::Entity::find()
            .secure()
            .scope_with(&all)
            .all(txn)
            .await
            .unwrap();
        assert!(snapshots.is_empty(), "leaked rate snapshots: {snapshots:?}");
        tenant_account::Entity::update_many()
            .col_expr(
                tenant_account::Column::AccountId,
                sea_orm::sea_query::Expr::value(self.accounts[index - 1]),
            )
            .filter(tenant_account::Column::AccountClass.eq("CONTRACT_LIABILITY"))
            .secure()
            .scope_with(&scope)
            .exec(txn)
            .await
            .map_err(crate::infra::posting::error_transport::scope_to_db)?;
        currency_scale_registry::Entity::update_many()
            .col_expr(
                currency_scale_registry::Column::CurrencyScale,
                sea_orm::sea_query::Expr::value(if index == 3 { 3 } else { 2 }),
            )
            .filter(currency_scale_registry::Column::Currency.eq("TKB"))
            .secure()
            .scope_with(&scope)
            .exec(txn)
            .await
            .map_err(crate::infra::posting::error_transport::scope_to_db)?;
        fx_rate::Entity::update_many()
            .col_expr(
                fx_rate::Column::Rate,
                sea_orm::sea_query::Expr::value(index.to_string()),
            )
            .secure()
            .scope_with(&scope)
            .exec(txn)
            .await
            .map_err(crate::infra::posting::error_transport::scope_to_db)?;
        fiscal_calendar::Entity::update_many()
            .col_expr(
                fiscal_calendar::Column::FunctionalCurrency,
                sea_orm::sea_query::Expr::value(if index == 2 { None } else { Some("TKB") }),
            )
            .secure()
            .scope_with(&scope)
            .exec(txn)
            .await
            .map_err(crate::infra::posting::error_transport::scope_to_db)?;
        Ok(())
    }
    async fn after(&self, txn: &DbTx<'_>) -> Result<(), AttemptError> {
        let evidence = state_in(txn).await;
        self.observations.lock().unwrap().push(evidence);
        if self.calls.load(Ordering::SeqCst) < 3 {
            Err(AttemptError::Conflict)
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn whole_attempt_refresh_uses_attempt_local_rate_functional_lookup_and_no_leaked_history() {
    let (db, mut svc, mut inv) = fixture().await;
    deferred(&mut inv);
    fx(&svc, &inv).await;
    let hook = Arc::new(Refresh {
        calls: AtomicUsize::new(0),
        observations: Mutex::new(vec![]),
        tenant: inv.seller_tenant_id,
        accounts: [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()],
    });
    svc.attempt_hook = Some(hook.clone());
    let result = svc
        .post_invoice(
            &SecurityContext::anonymous(),
            &AccessScope::for_tenant(inv.seller_tenant_id),
            &inv,
            true,
        )
        .await
        .unwrap();
    assert!(!result.replayed);
    assert_eq!(hook.calls.load(Ordering::SeqCst), 3);
    let observations = hook.observations.lock().unwrap().clone();
    assert_eq!(observations.len(), 3);
    assert!(observations[0].contains("rate: \"1\""));
    assert!(observations[1].contains("fx_rate_snapshot=[]"));
    assert!(observations[2].contains("rate: \"3\""));
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let stored = svc
        .journal
        .find_entry_with_lines(&scope, inv.seller_tenant_id, result.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored
            .lines
            .iter()
            .find(|line| line.account_class == "AR")
            .unwrap()
            .functional_money,
        Some(money("40.71", "TKB", 3))
    );
    assert_eq!(
        stored
            .lines
            .iter()
            .find(|line| line.account_class == "CONTRACT_LIABILITY")
            .unwrap()
            .account_id,
        hook.accounts[2]
    );
    let snapshots = fx_rate_snapshot::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].rate, "3");
}
struct PolicyInAttempt {
    tenant: Uuid,
}
#[async_trait::async_trait]
impl AttemptHook for PolicyInAttempt {
    async fn before(&self, txn: &DbTx<'_>) -> Result<(), AttemptError> {
        let scope = AccessScope::for_tenant(self.tenant);
        posting_policy::Entity::update_many()
            .col_expr(
                posting_policy::Column::MissingMappingMode,
                sea_orm::sea_query::Expr::value("HARD_BLOCK"),
            )
            .secure()
            .scope_with(&scope)
            .exec(txn)
            .await
            .map_err(crate::infra::posting::error_transport::scope_to_db)?;
        Ok(())
    }
}
#[tokio::test]
async fn policy_selection_observes_same_runner_changes_and_rolls_back_them() {
    let (db, mut svc, mut inv) = fixture().await;
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    svc.posting_policy_repo
        .write_version(
            &scope,
            inv.seller_tenant_id,
            &crate::domain::invoice::policy::PostingPolicy::default(),
            OffsetDateTime::now_utc() - time::Duration::seconds(1),
        )
        .await
        .unwrap();
    inv.items[0].catalog_class = None;
    svc.attempt_hook = Some(Arc::new(PolicyInAttempt {
        tenant: inv.seller_tenant_id,
    }));
    let before = state(&db).await;
    assert!(matches!(
        svc.post_invoice(&SecurityContext::anonymous(), &scope, &inv, true)
            .await,
        Err(DomainError::AccountMappingMissing(_))
    ));
    assert_eq!(
        svc.posting_policy_repo
            .read_effective_policy(&scope, inv.seller_tenant_id, OffsetDateTime::now_utc())
            .await
            .unwrap()
            .missing_mapping_mode,
        MissingMappingMode::Suspense
    );
    assert_eq!(state(&db).await, before);
}
#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_actual_invoice_fx_replay_full_historical_reversal() {
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
    let (db, mut svc, mut inv) = seeded(db).await;
    historical_case(db.clone(), svc.clone(), inv.clone()).await;
    inv.invoice_id = Uuid::now_v7().to_string();
    deferred(&mut inv);
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let ctx = SecurityContext::anonymous();
    let hook = Arc::new(LateFailure {
        calls: AtomicUsize::new(0),
        states: Mutex::new(vec![]),
        conflict: false,
    });
    svc.attempt_hook = Some(hook.clone());
    let before = state(&db).await;
    assert!(matches!(
        svc.post_invoice(&ctx, &scope, &inv, true).await,
        Err(DomainError::OverRecognition(_))
    ));
    assert!(hook.states.lock().unwrap()[0].contains("rate: \"9\""));
    assert_eq!(state(&db).await, before);
    svc.attempt_hook = None;
    let fresh = svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    assert!(!fresh.replayed);
    assert!(
        svc.post_invoice(&ctx, &scope, &inv, false)
            .await
            .unwrap()
            .replayed
    );
    let segments = recognition_segment::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert_eq!(segments.len(), 3);
}

/// Isolated fixture changes the configured spec directly; production provisioning
/// intentionally prevents currency scale changes after a monetary posting.
async fn changed_registry(db: &Db, tenant: Uuid) {
    use crate::infra::storage::entity::currency_scale_registry;
    currency_scale_registry::Entity::update_many()
        .col_expr(
            currency_scale_registry::Column::CurrencyScale,
            sea_orm::sea_query::Expr::value(3),
        )
        .filter(currency_scale_registry::Column::Currency.eq("TKA"))
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn correction_retains_fresh_business_identity_and_original_policy_references() {
    let (db, svc, inv) = fixture().await;
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let ctx = SecurityContext::anonymous();
    let original = svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    let mapped: Vec<_> = inv.items.iter().map(resolve).collect();
    let mut correction = build_invoice_entry(&inv, &mapped).unwrap();
    correction.source_doc_type = SourceDocType::MappingCorrection;
    correction.source_business_id = format!("{}:correction", inv.invoice_id);
    correction.reverses_entry_id = Some(original.entry_id);
    correction.reverses_period_id = Some(inv.period_id.clone());
    let corrected = svc
        .post_correction(&ctx, &scope, correction.clone())
        .await
        .unwrap();
    let stored = svc
        .journal
        .find_entry_with_lines(&scope, inv.seller_tenant_id, corrected.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.source_doc_type, "MAPPING_CORRECTION");
    assert_eq!(stored.reverses_entry_id, Some(original.entry_id));
    assert_eq!(
        stored
            .lines
            .iter()
            .find(|l| l.account_class == "REVENUE")
            .unwrap()
            .pricing_snapshot_ref
            .as_deref(),
        Some("pricing")
    );
    let before = state(&db).await;
    assert!(
        svc.post_correction(&ctx, &scope, correction.clone())
            .await
            .unwrap()
            .replayed
    );
    correction.lines[0].money = money("13.58", "TKA", 2);
    assert!(matches!(
        svc.post_correction(&ctx, &scope, correction).await,
        Err(DomainError::IdempotencyConflict(_))
    ));
    assert_eq!(state(&db).await, before);
}

#[tokio::test]
async fn every_recognition_policy_evidence_and_timing_intent_is_bound_before_sidecar_replay() {
    let (db, svc, mut inv) = fixture().await;
    deferred(&mut inv);
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let ctx = SecurityContext::anonymous();
    svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    let before = state(&db).await;
    for variant in 0..10 {
        let mut changed = inv.clone();
        let spec = changed.items[0].recognition.as_mut().unwrap();
        match variant {
            0 => spec.policy_ref = "policy-v2".into(),
            1 => {
                spec.timing = RecognitionTiming::StraightLine {
                    periods: 4,
                    first_period_id: None,
                }
            }
            2 => {
                spec.timing = RecognitionTiming::StraightLine {
                    periods: 3,
                    first_period_id: Some("202611".into()),
                }
            }
            3 => spec.po_allocation_group = Some("other-po".into()),
            4 => spec.ssp_snapshot_ref = Some("other-ssp".into()),
            5 => spec.subscription_ref = Some("other-sub".into()),
            6 => spec.vc_estimate_ref = Some("estimate".into()),
            7 => spec.vc_method_ref = Some("method".into()),
            8 => spec.multi_po = true,
            _ => spec.immaterial_one_shot_sku = true,
        }
        assert!(
            matches!(
                svc.post_invoice(&ctx, &scope, &changed, false).await,
                Err(DomainError::IdempotencyConflict(_))
            ),
            "variant {variant}"
        );
    }
    assert_eq!(state(&db).await, before);
}

#[test]
fn a_tenant_outside_the_caller_scope_is_an_authorization_error() {
    let caller = AccessScope::for_tenant(Uuid::now_v7());
    let other = Uuid::now_v7();
    assert!(matches!(
        authorize_tenant_scope(&caller, other, SourceDocType::InvoicePost),
        Err(DomainError::CrossTenantAccessDenied(_))
    ));
    let own = Uuid::now_v7();
    assert!(
        authorize_tenant_scope(
            &AccessScope::for_tenant(own),
            own,
            SourceDocType::InvoicePost
        )
        .is_ok()
    );
}

/// A grant that names the tenant but also a resource id is in scope for the
/// tenant: it is refused because its shape cannot authorize a fresh post, never
/// reported as a cross-tenant denial.
#[test]
fn a_constrained_grant_on_the_tenant_is_not_a_cross_tenant_denial() {
    use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};
    let tenant = Uuid::now_v7();
    let constrained = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
        ScopeFilter::in_uuids(pep_properties::RESOURCE_ID, vec![Uuid::now_v7()]),
    ]));
    match authorize_tenant_scope(&constrained, tenant, SourceDocType::MappingCorrection) {
        Err(DomainError::InvalidRequest(detail)) => {
            assert!(detail.contains("mapping correction"), "{detail}");
        }
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

/// One labelled single-field change to an invoice intent.
type InvoiceMutation = fn(&mut PostedInvoice);

/// Every non-recognition field of the invoice intent is bound by the request
/// hash: a re-post that changes any header, item or tax field is an idempotency
/// conflict with no effect, never a silent replay of the original entry.
#[tokio::test]
async fn every_header_item_and_tax_field_is_bound_before_replay() {
    let (db, svc, inv) = fixture().await;
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let ctx = SecurityContext::anonymous();
    svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    let before = state(&db).await;
    let variants: Vec<(&str, InvoiceMutation)> = vec![
        ("item amount", |i| {
            i.items[0].amount_ex_tax = money("12.35", "TKA", 2);
        }),
        ("item revenue stream", |i| {
            i.items[0].revenue_stream = "other".into();
        }),
        ("item gl code", |i| {
            i.items[0].gl_code = Some("other-gl".into());
        }),
        ("item ref", |i| {
            i.items[0].invoice_item_ref = Some("other-item".into());
        }),
        ("item sku", |i| {
            i.items[0].sku_or_plan_ref = None;
        }),
        ("item price", |i| {
            i.items[0].price_id = Some("other-price".into());
        }),
        ("item pricing snapshot", |i| {
            i.items[0].pricing_snapshot_ref = Some("other-pricing".into());
        }),
        ("item catalog class", |i| {
            i.items[0].catalog_class = None;
        }),
        ("tax amount", |i| {
            i.tax[0].amount = money("1.24", "TKA", 2);
        }),
        ("tax jurisdiction", |i| {
            i.tax[0].tax_jurisdiction = "EU-DE".into();
        }),
        ("tax filing period", |i| {
            i.tax[0].tax_filing_period = "2027Q1".into();
        }),
        ("tax rate ref", |i| {
            i.tax[0].tax_rate_ref = None;
        }),
        ("extra tax line", |i| {
            let mut extra = i.tax[0].clone();
            extra.tax_jurisdiction = "EU-FR".into();
            i.tax.push(extra);
        }),
        ("due date", |i| {
            i.due_date = chrono::NaiveDate::from_ymd_opt(2026, 11, 9);
        }),
        ("effective date", |i| {
            i.effective_at = chrono::NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
        }),
        ("payer", |i| {
            i.payer_tenant_id = Uuid::now_v7();
        }),
        ("resource tenant", |i| {
            i.resource_tenant_id = None;
        }),
    ];
    for (why, mutate) in variants {
        let mut changed = inv.clone();
        mutate(&mut changed);
        let result = svc.post_invoice(&ctx, &scope, &changed, false).await;
        assert!(
            matches!(result, Err(DomainError::IdempotencyConflict(_))),
            "{why}: {result:?}"
        );
    }
    assert_eq!(state(&db).await, before);
    // The unchanged intent still replays.
    assert!(
        svc.post_invoice(&ctx, &scope, &inv, true)
            .await
            .unwrap()
            .replayed
    );
}

/// Invoice exact-arithmetic failures use the gear's one table: named money
/// errors keep their wire codes, the limit is a range error, a term-count
/// overflow is a schedule error and structural defects are invariant failures.
#[test]
fn invoice_exact_errors_use_the_shared_table() {
    use crate::domain::exact_money::ExactError;
    use crate::domain::invoice::builder::InvoiceError;
    use bss_ledger_sdk::MoneyError;
    for (error, check) in [
        (
            ExactError::Money(MoneyError::ScaleMismatch),
            (|e: &DomainError| matches!(e, DomainError::InconsistentScale(_)))
                as fn(&DomainError) -> bool,
        ),
        (ExactError::Money(MoneyError::CurrencyMismatch), |e| {
            matches!(e, DomainError::CurrencyMismatch(_))
        }),
        (ExactError::ArithmeticLimit, |e| {
            matches!(e, DomainError::AmountOutOfRange(_))
        }),
        (ExactError::TooManyTerms, |e| {
            matches!(e, DomainError::ScheduleTooLong(_))
        }),
        (ExactError::DivisionByZero, |e| {
            matches!(e, DomainError::Internal(_))
        }),
    ] {
        let mapped = map_invoice_error(InvoiceError::Exact(error.clone()));
        assert!(check(&mapped), "{error:?} -> {mapped:?}");
    }
}

/// With caller lines advisory, `reverse_inner`'s header guards are the only
/// server-side check against stored history: each malformed reversal request is
/// an `InvalidRequest` that leaves the full state untouched.
#[tokio::test]
async fn reversal_header_guards_reject_without_effects() {
    let (db, svc, inv) = fixture().await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let original = svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    let before = state(&db).await;
    let missing = Uuid::now_v7();
    let cases: Vec<(&str, PostEntry)> = vec![
        ("no original reference", {
            let mut r = reversal(&inv, original.entry_id);
            r.reverses_entry_id = None;
            r
        }),
        ("not a REVERSAL document", {
            let mut r = reversal(&inv, original.entry_id);
            r.source_doc_type = SourceDocType::InvoicePost;
            r
        }),
        ("foreign business id", {
            let mut r = reversal(&inv, original.entry_id);
            r.source_business_id =
                crate::domain::invoice::reversal::reversal_business_id(Uuid::now_v7());
            r
        }),
        ("entry currency differs from the original", {
            let mut r = reversal(&inv, original.entry_id);
            r.entry_currency = "TKB".into();
            r
        }),
        ("original period differs", {
            let mut r = reversal(&inv, original.entry_id);
            r.reverses_period_id = Some("202609".into());
            r
        }),
        ("original absent", reversal(&inv, missing)),
    ];
    for (why, request) in cases {
        let result = svc
            .post_reversal(&ctx, &scope, request, Some("why".into()))
            .await;
        assert!(
            matches!(result, Err(DomainError::InvalidRequest(_))),
            "{why}: {result:?}"
        );
        assert_eq!(state(&db).await, before, "{why}");
    }
}

/// An original carrying a `REUSABLE_CREDIT` line cannot be faithfully reversed
/// (the credit-grant dimension is not reconstructible): refused with no effect.
#[tokio::test]
async fn reversing_an_original_with_a_reusable_credit_line_is_refused() {
    let (db, svc, inv) = fixture().await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let original = svc.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    // Relabel one stored line (SQLite has no immutability trigger here).
    let line_id = journal_line::Entity::find()
        .filter(journal_line::Column::EntryId.eq(original.entry_id))
        .secure()
        .scope_with(&scope)
        .all(&db.conn().unwrap())
        .await
        .unwrap()[0]
        .line_id;
    journal_line::Entity::update_many()
        .col_expr(
            journal_line::Column::AccountClass,
            sea_orm::sea_query::Expr::value("REUSABLE_CREDIT"),
        )
        // A REUSABLE_CREDIT line must carry its sub-grain (chk_journal_line_credit_grant).
        .col_expr(
            journal_line::Column::CreditGrantEventType,
            sea_orm::sea_query::Expr::value("promo"),
        )
        .filter(journal_line::Column::LineId.eq(line_id))
        .secure()
        .scope_with(&scope)
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    let before = state(&db).await;
    let result = svc
        .post_reversal(&ctx, &scope, reversal(&inv, original.entry_id), None)
        .await;
    match result {
        Err(DomainError::InvalidRequest(detail)) => {
            assert!(detail.contains("REUSABLE_CREDIT"), "{detail}");
        }
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
    assert_eq!(state(&db).await, before);
}

/// First attempt conflicts (retried), second fails terminally with a named
/// invariant error.
struct ConflictThenInvariant {
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl AttemptHook for ConflictThenInvariant {
    async fn after(&self, _txn: &DbTx<'_>) -> Result<(), AttemptError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(AttemptError::Conflict)
        } else {
            Err(DomainError::OverRecognition("terminal invariant after a retry".into()).into())
        }
    }
}

/// The invoice-post wrapper is the only source of its terminal invariant alarm:
/// a terminal failure after a retried conflict raises it exactly once per
/// operation (not once per attempt), and a clean post raises none.
#[tokio::test]
#[cfg(feature = "test-support")]
async fn terminal_invariant_alarm_is_emitted_once_per_operation() {
    let metrics = crate::infra::metrics::test_harness::MetricsHarness::new();
    let category = crate::infra::events::payloads::AlarmCategory::OverRecognition;
    let severity = crate::infra::events::alarm_catalog::severity(category);
    let labels = [
        ("category", category.as_str()),
        ("severity", severity.as_str()),
    ];
    let ctx = SecurityContext::anonymous();

    let (_db, mut clean, inv) = fixture().await;
    clean.publisher = Arc::new(LedgerEventPublisher::with_metrics(Arc::new(
        metrics.metrics(),
    )));
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    clean.post_invoice(&ctx, &scope, &inv, true).await.unwrap();
    metrics.force_flush();
    assert_eq!(metrics.counter_value("ledger_alarm_total", &labels), 0);

    let (db, mut svc, inv) = fixture().await;
    svc.publisher = Arc::new(LedgerEventPublisher::with_metrics(Arc::new(
        metrics.metrics(),
    )));
    let hook = Arc::new(ConflictThenInvariant {
        calls: AtomicUsize::new(0),
    });
    svc.attempt_hook = Some(hook.clone());
    let before = state(&db).await;
    let scope = AccessScope::for_tenant(inv.seller_tenant_id);
    let result = svc.post_invoice(&ctx, &scope, &inv, true).await;
    assert!(
        matches!(result, Err(DomainError::OverRecognition(_))),
        "{result:?}"
    );
    assert_eq!(
        hook.calls.load(Ordering::SeqCst),
        2,
        "one retry, then terminal"
    );
    metrics.force_flush();
    assert_eq!(metrics.counter_value("ledger_alarm_total", &labels), 1);
    assert_eq!(state(&db).await, before);
}
