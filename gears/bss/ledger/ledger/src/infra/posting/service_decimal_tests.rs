//! Real SQLite posting attempts: journal/cache/dedup/chain atomicity and provenance.
use super::*;
use crate::domain::model::{AccountRow, FiscalPeriodRow};
use crate::infra::posting::retry::retry_transaction;
use crate::infra::storage::entity::{
    account_balance, chain_state, idempotency_dedup, journal_entry,
};
use bss_ledger_sdk::{
    AccountClass, CurrencySpec, MappingStatus, PostedMoney, SourceDocType, parse_decimal,
};
use sea_orm::EntityTrait;
use sea_orm_migration::MigratorTrait;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_db::secure::{Db, SecureEntityExt};
use toolkit_db::{ConnectOpts, connect_db};

pub(crate) fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

pub(crate) async fn setup() -> (PostingService, Db, NewEntry, Vec<NewLine>) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let provider = DBProvider::new(db.clone());
    let reference = ReferenceRepo::new(provider.clone());
    let tenant = Uuid::now_v7();
    let entry = NewEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: "202610".to_owned(),
        entry_currency: "EUR".to_owned(),
        source_doc_type: SourceDocType::ManualAdjustment,
        source_business_id: "first".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: tenant,
        correlation_id: tenant,
        rounding_evidence: serde_json::Value::Null,
        rate_snapshot_ref: None,
    };
    let mut lines = Vec::new();
    for (class, side) in [
        (AccountClass::CashClearing, Side::Debit),
        (AccountClass::Revenue, Side::Credit),
    ] {
        let account_id = Uuid::now_v7();
        reference
            .insert_account(AccountRow {
                account_id,
                tenant_id: tenant,
                legal_entity_id: tenant,
                account_class: class.as_str().to_owned(),
                currency: "EUR".to_owned(),
                revenue_stream: None,
                normal_side: side.as_str().to_owned(),
                may_go_negative: false,
                lifecycle_state: "OPEN".to_owned(),
            })
            .await
            .unwrap();
        lines.push(NewLine {
            line_id: Uuid::now_v7(),
            payer_tenant_id: tenant,
            seller_tenant_id: None,
            resource_tenant_id: None,
            account_id,
            account_class: class,
            gl_code: None,
            side,
            money: money("1", "EUR", 2),
            invoice_id: None,
            due_date: None,
            revenue_stream: Some("test".to_owned()),
            mapping_status: MappingStatus::Resolved,
            functional_money: None,
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
            legal_entity_id: None,
            invoice_item_ref: None,
            sku_or_plan_ref: None,
            price_id: None,
            pricing_snapshot_ref: None,
            po_allocation_group: None,
            credit_grant_event_type: None,
            ar_status: None,
        });
    }
    provider
        .transaction(|txn| {
            Box::pin(async move {
                reference
                    .insert_fiscal_period_if_absent_txn(
                        txn,
                        FiscalPeriodRow {
                            tenant_id: tenant,
                            legal_entity_id: tenant,
                            period_id: "202610".to_owned(),
                            fiscal_tz: "UTC".to_owned(),
                            status: "OPEN".to_owned(),
                        },
                    )
                    .await
                    .map_err(repo_to_db)
            })
        })
        .await
        .unwrap();
    (
        PostingService::new(
            provider,
            Arc::new(crate::infra::events::publisher::LedgerEventPublisher::noop()),
        ),
        db,
        entry,
        lines,
    )
}

pub(crate) async fn counts(db: &Db) -> (u64, u64, u64, u64) {
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    (
        journal_entry::Entity::find()
            .secure()
            .scope_with(&scope)
            .count(&conn)
            .await
            .unwrap(),
        account_balance::Entity::find()
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
        chain_state::Entity::find()
            .secure()
            .scope_with(&scope)
            .count(&conn)
            .await
            .unwrap(),
    )
}

struct Reject {
    calls: Arc<AtomicUsize>,
    conflict: bool,
}
#[async_trait::async_trait]
impl PostSidecar for Reject {
    async fn run(&self, _: &DbTx<'_>, _: &AccessScope, _: &PostedFacts) -> Result<(), DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(if self.conflict {
            DomainError::ConcurrentModification("sidecar".to_owned())
        } else {
            DomainError::InvalidRequest("cap".to_owned())
        })
    }
}

#[tokio::test]
async fn registry_rejection_and_sidecar_failures_leave_no_partial_post() {
    let (svc, db, entry, lines) = setup().await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let mut wrong = lines.clone();
    for l in &mut wrong {
        l.money = money("1", "EUR", 3);
    }
    assert!(matches!(
        svc.post(&ctx, &scope, entry.clone(), wrong, None).await,
        Err(DomainError::InconsistentScale(_))
    ));
    let mut wrong = lines.clone();
    for l in &mut wrong {
        l.functional_money = Some(money("1", "USD", 3));
    }
    assert!(matches!(
        svc.post(&ctx, &scope, entry.clone(), wrong, None).await,
        Err(DomainError::InconsistentScale(_))
    ));
    assert_eq!(counts(&db).await, (0, 0, 0, 0));
    for (conflict, expected) in [(true, 3), (false, 1)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let result = svc
            .post(
                &ctx,
                &scope,
                entry.clone(),
                lines.clone(),
                Some(Arc::new(Reject {
                    calls: calls.clone(),
                    conflict,
                })),
            )
            .await;
        if conflict {
            assert!(matches!(
                result,
                Err(DomainError::ConcurrentModification(_))
            ));
        } else {
            assert!(matches!(result, Err(DomainError::InvalidRequest(_))));
        }
        assert_eq!(calls.load(Ordering::SeqCst), expected);
        assert_eq!(counts(&db).await, (0, 0, 0, 0));
    }
    let posted = svc
        .post(&ctx, &scope, entry.clone(), lines.clone(), None)
        .await
        .unwrap();
    assert!(!posted.replayed);
    assert_eq!(counts(&db).await, (1, 2, 1, 1));
    let replay = svc
        .post(&ctx, &scope, entry.clone(), lines.clone(), None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.entry_id, posted.entry_id);
    let mut changed = lines;
    for l in &mut changed {
        l.money = money("2", "EUR", 2);
    }
    assert!(matches!(
        svc.post(&ctx, &scope, entry, changed, None).await,
        Err(DomainError::IdempotencyConflict(_))
    ));
    assert_eq!(counts(&db).await, (1, 2, 1, 1));
}

#[tokio::test]
async fn single_attempt_rolls_back_seal_and_rebuilds_before_retry() {
    let (svc, db, entry, lines) = setup().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let read_db = db.clone();
    let result = retry_transaction(&db, move |txn| {
        let svc = svc.clone();
        let mut entry = entry.clone();
        let mut lines = lines.clone();
        let calls = calls.clone();
        Box::pin(async move {
            let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
            // No prior failed attempt may leak a balance into this new snapshot.
            let existing = account_balance::Entity::find()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .all(txn)
                .await
                .map_err(super::super::error_transport::scope_to_db)?;
            assert!(existing.is_empty());
            entry.entry_id = Uuid::now_v7();
            for line in &mut lines {
                line.money = money(&attempt.to_string(), "EUR", 2);
            }
            let result = svc
                .post_once(
                    &SecurityContext::anonymous(),
                    txn,
                    &AccessScope::allow_all(),
                    entry,
                    lines,
                    None,
                    ClaimSpec::fresh(),
                )
                .await?;
            // Fail AFTER chain and dedup finalization to test rollback of the whole tail.
            if attempt < 3 {
                return Err(AttemptError::Conflict);
            }
            Ok(result)
        })
    })
    .await
    .unwrap();
    assert!(!result.replayed);
    assert_eq!(observed.load(Ordering::SeqCst), 3);
    assert_eq!(counts(&read_db).await, (1, 2, 1, 1));
    let balances = account_balance::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&read_db.conn().unwrap())
        .await
        .unwrap();
    assert!(balances.iter().all(|row| row.balance == "3"));
}

#[tokio::test]
async fn historical_reversal_and_replay_use_stored_money_after_registry_change() {
    use crate::infra::storage::entity::currency_scale_registry;
    use sea_orm::ActiveValue::Set;
    use toolkit_db::secure::SecureInsertExt;
    let (svc, db, entry, lines) = setup().await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let posted = svc
        .post(&ctx, &scope, entry.clone(), lines.clone(), None)
        .await
        .unwrap();
    // Isolated SQLite fixture simulates changed authoritative configuration;
    // the public provisioning API correctly forbids this after a posting.
    let row = currency_scale_registry::ActiveModel {
        tenant_id: Set(entry.tenant_id),
        currency: Set("EUR".to_owned()),
        currency_scale: Set(3),
        source: Set("test".to_owned()),
    };
    currency_scale_registry::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&scope, &row)
        .unwrap()
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    let replay = svc
        .post(&ctx, &scope, entry.clone(), lines.clone(), None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.entry_id, posted.entry_id);
    let mut fresh = entry.clone();
    fresh.entry_id = Uuid::now_v7();
    fresh.source_business_id = "new".to_owned();
    assert!(matches!(
        svc.post(&ctx, &scope, fresh, lines, None).await,
        Err(DomainError::InconsistentScale(_))
    ));
    let mut reversal = entry.clone();
    reversal.entry_id = Uuid::now_v7();
    reversal.source_business_id = "reversal".to_owned();
    reversal.reverses_entry_id = Some(entry.entry_id);
    reversal.reverses_period_id = Some(entry.period_id);
    let result = retry_transaction(&db, move |txn| {
        let svc = svc.clone();
        let reversal = reversal.clone();
        let ctx = ctx.clone();
        let scope = scope.clone();
        Box::pin(async move {
            svc.post_reversal_once(&ctx, txn, &scope, reversal, None, ClaimSpec::fresh())
                .await
        })
    })
    .await
    .unwrap();
    assert!(!result.replayed);
    assert_eq!(counts(&db).await, (2, 2, 2, 1));
    let balances = account_balance::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(
        balances
            .iter()
            .all(|row| row.balance == "0" && row.currency_scale == 2)
    );
}

#[tokio::test]
async fn queued_apply_and_request_hash_replay_keep_claim_semantics() {
    let (svc, db, entry, lines) = setup().await;
    let tenant = entry.tenant_id;
    let flow = entry.source_doc_type.as_str().to_owned();
    let business_id = entry.source_business_id.clone();
    db.transaction_ref_mapped_with_config(
        toolkit_db::secure::TxConfig::serializable(),
        move |txn| {
            Box::pin(async move {
                IdempotencyGate::new()
                    .claim_queued(txn, tenant, &flow, &business_id, "request")
                    .await
                    .map_err(repo_to_db)
            })
        },
    )
    .await
    .unwrap();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let posted = svc
        .post_queued_apply(&ctx, &scope, entry.clone(), lines.clone(), None)
        .await
        .unwrap();
    assert!(!posted.replayed);
    let replay = svc
        .post_queued_apply(&ctx, &scope, entry.clone(), lines.clone(), None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.entry_id, posted.entry_id);
    let replay = svc
        .post_with_request_hash(
            &ctx,
            &scope,
            entry.clone(),
            lines.clone(),
            None,
            "request".to_owned(),
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert!(matches!(
        svc.post_with_request_hash(&ctx, &scope, entry, lines, None, "changed".to_owned())
            .await,
        Err(DomainError::IdempotencyConflict(_))
    ));
    assert_eq!(counts(&db).await, (1, 2, 1, 1));
}

#[tokio::test]
async fn balance_range_failure_rolls_back_journal_dedup_and_chain() {
    let (svc, db, mut entry, mut lines) = setup().await;
    let max = "9999999999999999999999999999";
    for line in &mut lines {
        line.money = money(max, "EUR", 2);
    }
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::allow_all();
    let first = svc
        .post(&ctx, &scope, entry.clone(), lines.clone(), None)
        .await
        .unwrap();
    entry.entry_id = Uuid::now_v7();
    entry.source_business_id = "overflow".to_owned();
    for line in &mut lines {
        line.line_id = Uuid::now_v7();
        line.money = money("0.01", "EUR", 2);
    }
    assert!(matches!(
        svc.post(&ctx, &scope, entry, lines, None).await,
        Err(DomainError::AmountOutOfRange(_))
    ));
    assert_eq!(counts(&db).await, (1, 2, 1, 1));
    let conn = db.conn().unwrap();
    let balances = account_balance::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    assert!(
        balances
            .iter()
            .all(|row| row.balance == max && row.version == 0)
    );
    let tip = chain_state::Entity::find()
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tip.last_entry_id, first.entry_id);
}

#[tokio::test]
async fn once_enforces_lifecycle_on_the_supplied_snapshot() {
    use crate::infra::storage::entity::tenant_account;
    use sea_orm::{ColumnTrait, Condition};
    use toolkit_db::secure::SecureUpdateExt;
    let (svc, db, entry, lines) = setup().await;
    let account = lines[0].account_id;
    let result = retry_transaction(&db, move |txn| {
        let svc = svc.clone();
        let entry = entry.clone();
        let lines = lines.clone();
        Box::pin(async move {
            tenant_account::Entity::update_many()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .col_expr(
                    tenant_account::Column::LifecycleState,
                    sea_orm::sea_query::Expr::value("CLOSED"),
                )
                .filter(Condition::all().add(tenant_account::Column::AccountId.eq(account)))
                .exec(txn)
                .await
                .map_err(super::super::error_transport::scope_to_db)?;
            svc.post_once(
                &SecurityContext::anonymous(),
                txn,
                &AccessScope::allow_all(),
                entry,
                lines,
                None,
                ClaimSpec::fresh(),
            )
            .await
        })
    })
    .await;
    assert!(matches!(result, Err(DomainError::AccountClosed(_))));
    assert_eq!(counts(&db).await, (0, 0, 0, 0));
}

#[tokio::test]
async fn fresh_interface_cannot_reinterpret_an_explicit_reversal() {
    let (svc, db, mut entry, lines) = setup().await;
    entry.source_doc_type = SourceDocType::Reversal;
    assert!(matches!(
        svc.post(
            &SecurityContext::anonymous(),
            &AccessScope::allow_all(),
            entry,
            lines,
            None
        )
        .await,
        Err(DomainError::InvalidRequest(_))
    ));
    assert_eq!(counts(&db).await, (0, 0, 0, 0));
}
