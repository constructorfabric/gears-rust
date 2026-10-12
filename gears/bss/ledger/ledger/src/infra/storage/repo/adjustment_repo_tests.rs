//! Real migrated SQLite checks for canonical adjustment records and exposure CAS.
use super::*;
use crate::infra::posting::retry::AttemptError;
use bss_ledger_sdk::MoneyError;
use bss_ledger_sdk::parse_decimal;
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

fn money(text: &str) -> PostedMoney {
    specified(text, "EUR", 2)
}
fn specified(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
async fn setup() -> (AdjustmentRepo, Db, Uuid) {
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
        AdjustmentRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
    )
}
fn note(tenant: Uuid) -> NewCreditNote {
    NewCreditNote {
        tenant_id: tenant,
        credit_note_id: "cn".into(),
        origin_invoice_id: "i".into(),
        origin_invoice_item_ref: None,
        revenue_stream: "service".into(),
        amount: money("1.2"),
        recognized_part: money("0.4"),
        deferred_part: money("0.6"),
        split_basis_ref: None,
        reason_code: "correction".into(),
        created_at_utc: OffsetDateTime::now_utc(),
    }
}
fn refund(tenant: Uuid) -> NewRefund {
    NewRefund {
        tenant_id: tenant,
        refund_id: "rf".into(),
        psp_refund_id: "psp".into(),
        phase: "initiated".into(),
        pattern: "A_UNALLOCATED".into(),
        payment_id: "p".into(),
        invoice_id: None,
        amount: money("0.01"),
        clearing_state: "PENDING".into(),
        relates_to_refund_id: None,
        reverses_entry_id: None,
        created_at_utc: OffsetDateTime::now_utc(),
    }
}
#[tokio::test]
async fn first_write_caps_metadata_and_stale_cas() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.seed_exposure_first_touch(tx, &s, tenant, "i", &money("1.2"))
                .await?;
            r.add_credit_note_total(tx, &s, tenant, "i", &money("1.2"))
                .await?;
            let before = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            r.seed_exposure_first_touch(tx, &s, tenant, "i", &money("900"))
                .await?;
            assert_eq!(
                r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap(),
                before
            );
            assert!(matches!(
                r.seed_exposure_first_touch(tx, &s, tenant, "i", &specified("1", "EUR", 3))
                    .await,
                Err(RepoError::Money(MoneyError::ScaleMismatch))
            ));
            assert!(matches!(
                r.add_credit_note_total(tx, &s, tenant, "i", &money("0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            assert!(matches!(
                r.add_debit_note_total(tx, &s, tenant, "i", &specified("1", "USD", 2))
                    .await,
                Err(RepoError::Money(MoneyError::CurrencyMismatch))
            ));
            r.add_debit_note_total(tx, &s, tenant, "i", &money("0.1"))
                .await?;
            assert!(matches!(
                r.write_exposure(tx, &s, &before, &before, ExposureTotal::CreditNotes)
                    .await,
                Err(RepoError::Conflict(_))
            ));
            r.add_credit_note_total(tx, &s, tenant, "i", &money("0.1"))
                .await?;
            let after = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            assert_eq!(after.credit_note_total, money("1.3"));
            assert_eq!(after.version, 3);
            // Preserve signed counter deltas, constrained by the resulting full state.
            assert!(matches!(
                r.add_debit_note_total(tx, &s, tenant, "i", &money("-0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            r.add_credit_note_total(tx, &s, tenant, "i", &money("-0.1"))
                .await?;
            r.add_debit_note_total(tx, &s, tenant, "i", &money("-0.1"))
                .await?;
            assert!(matches!(
                r.add_credit_note_total(tx, &s, tenant, "i", &money("-2"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    for scope in [
        AccessScope::for_tenant(Uuid::now_v7()),
        AccessScope::default(),
    ] {
        assert!(
            repo.read_exposure_out_of_txn(&scope, tenant, "i")
                .await
                .unwrap()
                .is_none()
        );
    }
}
#[tokio::test]
async fn cap_error_rolls_back_seed_and_note() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    let result = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let s = AccessScope::for_tenant(tenant);
                r.seed_exposure_first_touch(tx, &s, tenant, "i", &money("1"))
                    .await?;
                r.insert_credit_note(tx, &s, &note(tenant)).await?;
                r.add_credit_note_total(tx, &s, tenant, "i", &money("1.2"))
                    .await?;
                Ok::<_, AttemptError>(())
            })
        })
        .await;
    assert!(result.is_err());
    let s = AccessScope::for_tenant(tenant);
    assert!(
        repo.read_exposure_out_of_txn(&s, tenant, "i")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.read_credit_note_out_of_txn(&s, tenant, "cn")
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn inclusive_tax_notes_refund_pages_and_corruption() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_credit_note(tx, &s, &note(tenant)).await?;
            r.insert_debit_note(
                tx,
                &s,
                &NewDebitNote {
                    tenant_id: tenant,
                    debit_note_id: "dn".into(),
                    origin_invoice_id: "i".into(),
                    amount: money("1.2"),
                    recognized_part: money("0.4"),
                    deferred_part: money("0.6"),
                    created_at_utc: OffsetDateTime::now_utc(),
                },
            )
            .await?;
            r.insert_refund(tx, &s, &refund(tenant)).await?;
            let mut bad = note(tenant);
            bad.credit_note_id = "bad".into();
            bad.deferred_part = specified("0.6", "EUR", 3);
            assert!(matches!(
                r.insert_credit_note(tx, &s, &bad).await,
                Err(RepoError::Money(MoneyError::ScaleMismatch))
            ));
            assert_eq!(
                r.read_refund_by_psp_phase_in(tx, &s, tenant, "psp", "initiated")
                    .await?
                    .unwrap()
                    .amount,
                money("0.01")
            );
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let s = AccessScope::for_tenant(tenant);
    let q = ODataQuery::default();
    assert_eq!(
        repo.list_credit_notes(&s, tenant, &q).await.unwrap().items[0].amount,
        money("1.2")
    );
    assert_eq!(
        repo.list_debit_notes(&s, tenant, &q).await.unwrap().items[0].recognized_part,
        money("0.4")
    );
    assert_eq!(
        repo.list_refunds(&s, tenant, &q).await.unwrap().items[0].amount,
        money("0.01")
    );
    let foreign = AccessScope::for_tenant(Uuid::now_v7());
    assert!(
        repo.list_refunds(&foreign, tenant, &q)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        repo.list_credit_notes(&foreign, tenant, &q)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        repo.list_debit_notes(&foreign, tenant, &q)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let conn = repo.db.conn().unwrap();
    refund::Entity::update_many()
        .secure()
        .scope_with(&s)
        .col_expr(refund::Column::Amount, Expr::value("0.010"))
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        repo.read_refund_out_of_txn(&s, tenant, "rf").await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    assert!(matches!(
        repo.list_refunds(&s, tenant, &q).await,
        Err(OdataPageError::Db(_))
    ));
    credit_note::Entity::update_many()
        .secure()
        .scope_with(&s)
        .col_expr(credit_note::Column::RecognizedPart, Expr::value("00.4"))
        .exec(&conn)
        .await
        .unwrap();
    debit_note::Entity::update_many()
        .secure()
        .scope_with(&s)
        .col_expr(debit_note::Column::DeferredPart, Expr::value("0.60"))
        .exec(&conn)
        .await
        .unwrap();
    assert!(repo.list_credit_notes(&s, tenant, &q).await.is_err());
    assert!(repo.list_debit_notes(&s, tenant, &q).await.is_err());
    assert!(matches!(
        repo.read_credit_note_out_of_txn(&s, tenant, "cn").await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    assert!(matches!(
        repo.read_debit_note_out_of_txn(&s, tenant, "dn").await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
}
#[tokio::test]
async fn exact_wide_headroom_overflow_and_corrupt_existing_cap() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let max = money("9999999999999999999999999999");
            r.seed_exposure_first_touch(tx, &s, tenant, "i", &max)
                .await?;
            r.add_debit_note_total(tx, &s, tenant, "i", &max).await?;
            r.add_credit_note_total(tx, &s, tenant, "i", &max).await?;
            assert!(matches!(
                r.add_debit_note_total(tx, &s, tenant, "i", &money("0.01"))
                    .await,
                Err(RepoError::Money(MoneyError::AmountOutOfRange))
            ));
            assert_eq!(
                r.read_exposure_in(tx, &s, tenant, "i")
                    .await?
                    .unwrap()
                    .version,
                2
            );
            invoice_exposure::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(invoice_exposure::Column::OriginalTotal, Expr::value("0"))
                .col_expr(invoice_exposure::Column::DebitNoteTotal, Expr::value("0"))
                .exec(tx)
                .await
                .map_err(|e| scope_to_repo(e, r.db.db().backend()))?;
            // Persisted impossible caps are corruption, even when a delta could repair them.
            assert_internal_stored_exposure(
                r.read_exposure_in(tx, &s, tenant, "i").await.unwrap_err(),
            );
            assert_internal_stored_exposure(
                r.add_debit_note_total(tx, &s, tenant, "i", &max)
                    .await
                    .unwrap_err(),
            );
            assert_internal_stored_exposure(
                r.add_credit_note_total(
                    tx,
                    &s,
                    tenant,
                    "i",
                    &money("-9999999999999999999999999999"),
                )
                .await
                .unwrap_err(),
            );
            let row = invoice_exposure::Entity::find()
                .secure()
                .scope_with(&s)
                .one(tx)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.original_total, "0");
            assert_eq!(row.debit_note_total, "0");
            assert_eq!(row.credit_note_total, encode_amount(&max));
            assert_eq!(row.version, 2);
            // SQLite's nonnegative CHECK prevents these persisted fixtures, so exercise
            // the actual stored-row decoder with each corrupted field on this real model.
            for field in 0..3 {
                let mut bad = row.clone();
                bad.original_total = "1".into();
                bad.debit_note_total = "0".into();
                bad.credit_note_total = "0".into();
                match field {
                    0 => bad.original_total = "-1".into(),
                    1 => bad.debit_note_total = "-1".into(),
                    _ => bad.credit_note_total = "-1".into(),
                }
                assert_internal_stored_exposure(decode_invoice_exposure(bad).unwrap_err());
            }
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
        source_doc_type: Set(SourceDocType::InvoicePost.as_str().into()),
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
        account_class: Set(AccountClass::Ar.as_str().into()),
        gl_code: Set(None),
        side: Set(side.as_str().into()),
        amount: Set(text.into()),
        currency: Set("EUR".into()),
        currency_scale: Set(2),
        invoice_id: Set(Some("i".into())),
        due_date: Set(None),
        revenue_stream: Set(None),
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

fn ar_row(tenant: Uuid, text: &str) -> ar_invoice_balance::ActiveModel {
    ar_invoice_balance::ActiveModel {
        tenant_id: Set(tenant),
        payer_tenant_id: Set(tenant),
        account_id: Set(Uuid::now_v7()),
        invoice_id: Set("i".into()),
        currency: Set("EUR".into()),
        currency_scale: Set(2),
        balance: Set(text.into()),
        disputed: Set("0".into()),
        functional_balance: Set(None),
        functional_currency: Set(None),
        functional_currency_scale: Set(None),
        original_posted_at: Set(None),
        due_date: Set(None),
        last_entry_seq: Set(None),
        version: Set(0),
    }
}

#[tokio::test]
async fn ar_folds_exact_cancellation_explicit_zero_corruption_and_scope() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let spec = money("0").currency().clone();
            assert_eq!(
                r.read_posted_ar_incl_tax_in(tx, &s, tenant, "missing", &spec)
                    .await?,
                money("0")
            );
            assert_eq!(
                r.read_open_ar_for_invoice_in(tx, &s, tenant, "missing", &spec)
                    .await?,
                money("0")
            );
            let entry = Uuid::now_v7();
            let h = header(tenant, entry);
            journal_entry::Entity::insert(h.clone())
                .secure()
                .scope_with_model(&s, &h)
                .unwrap()
                .exec(tx)
                .await
                .unwrap();
            // Intermediate sum exceeds PostedMoney; the final exact net still fits.
            let max = "9999999999999999999999999999";
            for (text, side) in [
                (max, Side::Debit),
                ("0.01", Side::Debit),
                (max, Side::Credit),
            ] {
                let l = line(tenant, entry, text, side);
                journal_line::Entity::insert(l.clone())
                    .secure()
                    .scope_with_model(&s, &l)
                    .unwrap()
                    .exec(tx)
                    .await
                    .unwrap();
            }
            assert!(r.posted_invoice_exists_in(tx, &s, tenant, "i").await?);
            assert_eq!(
                r.read_posted_ar_incl_tax_in(tx, &s, tenant, "i", &spec)
                    .await?,
                money("0.01")
            );
            for text in ["0.1", "0.02", "0"] {
                let row = ar_row(tenant, text);
                ar_invoice_balance::Entity::insert(row.clone())
                    .secure()
                    .scope_with_model(&s, &row)
                    .unwrap()
                    .exec(tx)
                    .await
                    .unwrap();
            }
            assert_eq!(
                r.read_open_ar_for_invoice_in(tx, &s, tenant, "i", &spec)
                    .await?,
                money("0.12")
            );
            for scope in [
                AccessScope::default(),
                AccessScope::for_tenant(Uuid::now_v7()),
            ] {
                assert!(!r.posted_invoice_exists_in(tx, &scope, tenant, "i").await?);
                assert_eq!(
                    r.read_posted_ar_incl_tax_in(tx, &scope, tenant, "i", &spec)
                        .await?,
                    money("0")
                );
                assert_eq!(
                    r.read_open_ar_for_invoice_in(tx, &scope, tenant, "i", &spec)
                        .await?,
                    money("0")
                );
            }
            assert!(matches!(
                r.read_posted_ar_incl_tax_in(
                    tx,
                    &s,
                    tenant,
                    "i",
                    &CurrencySpec::try_new("EUR".into(), 3).unwrap()
                )
                .await,
                // The caller's spec disagrees with the stored invoice: a client
                // money mismatch, not corrupt stored data.
                Err(RepoError::Money(
                    bss_ledger_sdk::MoneyError::CurrencyMismatch
                        | bss_ledger_sdk::MoneyError::ScaleMismatch
                ))
            ));
            ar_invoice_balance::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(ar_invoice_balance::Column::Balance, Expr::value("0.010"))
                .exec(tx)
                .await
                .unwrap();
            assert!(matches!(
                r.read_open_ar_for_invoice_in(tx, &s, tenant, "i", &spec)
                    .await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            journal_line::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(journal_line::Column::Amount, Expr::value("0.001"))
                .exec(tx)
                .await
                .unwrap();
            assert!(matches!(
                r.read_posted_ar_incl_tax_in(tx, &s, tenant, "i", &spec)
                    .await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_contention_preserves_first_seed_and_credit_winner() {
    use crate::infra::posting::retry::retry_transaction;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let path = std::env::temp_dir().join(format!("ledger-exposure-{}.sqlite", Uuid::now_v7()));
    let dsn = format!("sqlite://{}?mode=rwc&busy_timeout=1", path.display());
    let db = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let repo = AdjustmentRepo::new(DBProvider::new(db.clone()));
    let tenant = Uuid::now_v7();
    let other_db = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    let other = AdjustmentRepo::new(DBProvider::new(other_db.clone()));
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let holder = tokio::spawn(async move {
        db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let s = AccessScope::for_tenant(tenant);
                repo.seed_exposure_first_touch(tx, &s, tenant, "i", &money("1"))
                    .await?;
                repo.add_credit_note_total(tx, &s, tenant, "i", &money("0.8"))
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
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let result = retry_transaction(&other_db, |tx| {
        count.fetch_add(1, Ordering::SeqCst);
        let r = other.clone();
        Box::pin(async move {
            r.seed_exposure_first_touch(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                "i",
                &money("2"),
            )
            .await?;
            Ok(())
        })
    })
    .await;
    release_tx.send(()).unwrap();
    holder.await.unwrap();
    assert!(matches!(
        result,
        Err(crate::domain::error::DomainError::ConcurrentModification(_))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    retry_transaction(&other_db, |tx| {
        let r = other.clone();
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.seed_exposure_first_touch(tx, &s, tenant, "i", &money("2"))
                .await?;
            r.add_credit_note_total(tx, &s, tenant, "i", &money("0.2"))
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let winner = other
        .read_exposure_out_of_txn(&AccessScope::for_tenant(tenant), tenant, "i")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(winner.original_total, money("1"));
    assert_eq!(winner.credit_note_total, money("1"));
    assert_eq!(winner.version, 2);
    drop(other);
    drop(other_db);
    // Best effort: Windows keeps the file locked until the pool's async close completes.
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn refund_phase_identity_and_duplicate_rolls_back_other_effects() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.insert_refund(tx, &s, &refund(tenant)).await?;
            let mut next = refund(tenant);
            next.refund_id = "confirmed".into();
            next.phase = "confirmed".into();
            next.clearing_state = "SETTLED".into();
            next.pattern = "B_RESTORE_AR".into();
            next.invoice_id = Some("i".into());
            r.insert_refund(tx, &s, &next).await?;
            assert_eq!(
                r.read_refund_by_psp_phase_in(tx, &s, tenant, "psp", "confirmed")
                    .await?
                    .unwrap()
                    .clearing_state,
                "SETTLED"
            );
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let r = repo.clone();
    let result = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let s = AccessScope::for_tenant(tenant);
                r.insert_credit_note(tx, &s, &note(tenant)).await?;
                let mut duplicate = refund(tenant);
                duplicate.refund_id = "another".into();
                r.insert_refund(tx, &s, &duplicate).await?;
                Ok::<_, AttemptError>(())
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::Internal(_)
        ))
    ));
    let s = AccessScope::for_tenant(tenant);
    assert!(
        repo.read_credit_note_out_of_txn(&s, tenant, "cn")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.list_refunds(&s, tenant, &ODataQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        2
    );
}

#[tokio::test]
async fn exposure_corrupt_text_metadata_and_version_are_internal() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.seed_exposure_first_touch(tx, &s, tenant, "i", &money("1"))
                .await?;
            for text in ["garbage", "1.00", "0.001"] {
                invoice_exposure::Entity::update_many()
                    .secure()
                    .scope_with(&s)
                    .col_expr(invoice_exposure::Column::OriginalTotal, Expr::value(text))
                    .exec(tx)
                    .await
                    .unwrap();
                assert!(matches!(
                    r.read_exposure_in(tx, &s, tenant, "i").await,
                    Err(RepoError::InvalidStoredMoney(_))
                ));
                assert!(matches!(
                    r.add_debit_note_total(tx, &s, tenant, "i", &money("1"))
                        .await,
                    Err(RepoError::InvalidStoredMoney(_))
                ));
            }
            invoice_exposure::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(invoice_exposure::Column::OriginalTotal, Expr::value("1"))
                .col_expr(invoice_exposure::Column::Version, Expr::value(i64::MAX))
                .exec(tx)
                .await
                .unwrap();
            assert!(matches!(
                r.add_credit_note_total(tx, &s, tenant, "i", &money("0.01"))
                    .await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            let view = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            assert_eq!(view.credit_note_total, money("0"));
            assert_eq!(view.version, i64::MAX);
            invoice_exposure::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(invoice_exposure::Column::Currency, Expr::value("bad-code"))
                .exec(tx)
                .await
                .unwrap();
            assert!(matches!(
                r.read_exposure_in(tx, &s, tenant, "i").await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stale_version_aborts_whole_attempt_and_rebuilds_three_times() {
    use crate::infra::posting::retry::retry_transaction;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (repo, db, tenant) = setup().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let result = retry_transaction(&db, |tx| {
        count.fetch_add(1, Ordering::SeqCst);
        let r = repo.clone();
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            assert!(r.read_exposure_in(tx, &s, tenant, "i").await?.is_none());
            assert!(r.read_credit_note_in(tx, &s, tenant, "cn").await?.is_none());
            r.seed_exposure_first_touch(tx, &s, tenant, "i", &money("2"))
                .await?;
            let observed = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            r.insert_credit_note(tx, &s, &note(tenant)).await?;
            r.add_credit_note_total(tx, &s, tenant, "i", &money("1.2"))
                .await?;
            // Exercise the actual CAS writer with a genuinely stale observed token.
            r.write_exposure(tx, &s, &observed, &observed, ExposureTotal::CreditNotes)
                .await?;
            Ok(())
        })
    })
    .await;
    assert!(matches!(
        result,
        Err(crate::domain::error::DomainError::ConcurrentModification(_))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    let s = AccessScope::for_tenant(tenant);
    assert!(
        repo.read_exposure_out_of_txn(&s, tenant, "i")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.read_credit_note_out_of_txn(&s, tenant, "cn")
            .await
            .unwrap()
            .is_none()
    );
}

/// Corrupt numeric invariants must survive the real sentinel adapter as Internal.
fn assert_internal_stored_exposure(error: RepoError) {
    use crate::domain::error::DomainError;
    use crate::infra::posting::error_transport::{decode_business_error, repo_to_db};
    assert!(matches!(&error, RepoError::InvalidStoredMoney(_)));
    let transported = repo_to_db(error);
    assert!(matches!(
        decode_business_error(&transported),
        DomainError::Internal(_)
    ));
}

/// Each note kind moves only its own exposure total, and the headroom rule sees
/// the other total unchanged; the write CASes the column the kind names.
#[tokio::test]
async fn each_note_kind_moves_only_its_own_exposure_total() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            r.seed_exposure_first_touch(tx, &s, tenant, "i", &money("1"))
                .await?;
            let seeded = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            r.add_debit_note_total(tx, &s, tenant, "i", &money("0.5"))
                .await?;
            let debited = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            assert_eq!(debited.debit_note_total, money("0.5"));
            assert_eq!(debited.credit_note_total, seeded.credit_note_total);
            // The debit widened the headroom: 1 + 0.5 may now be credited.
            r.add_credit_note_total(tx, &s, tenant, "i", &money("1.5"))
                .await?;
            let credited = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            assert_eq!(credited.credit_note_total, money("1.5"));
            assert_eq!(credited.debit_note_total, money("0.5"));
            assert!(matches!(
                r.add_credit_note_total(tx, &s, tenant, "i", &money("0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            // The CAS writer stores the named total only.
            let mut next = credited.clone();
            next.debit_note_total = money("0.7");
            next.credit_note_total = money("0.1");
            r.write_exposure(tx, &s, &credited, &next, ExposureTotal::DebitNotes)
                .await?;
            let written = r.read_exposure_in(tx, &s, tenant, "i").await?.unwrap();
            assert_eq!(written.debit_note_total, money("0.7"));
            assert_eq!(written.credit_note_total, money("1.5"));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}
