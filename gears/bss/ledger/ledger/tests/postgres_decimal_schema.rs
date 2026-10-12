//! Postgres-only: the database guards behind decimal money. The canonical
//! decimal validator every money CHECK calls; the currency-scale lock on first
//! registration (the m041 fixture); the functional metadata rejection at
//! COMMIT; the append-only dual-control policy versions; the snapshot identity
//! over both scales and the rate; and the chain verifier over a tampered
//! `journal_line.amount`. Ignored by default; run with
//! `cargo test -p cf-gears-bss-ledger --test postgres_decimal_schema -- --ignored`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::sync::Arc;

use bss_ledger::domain::model::{AccountRow, CurrencyScaleRow, NewEntry, NewLine};
use bss_ledger::domain::ports::metrics::NoopLedgerMetrics;
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::jobs::verifier::ChainVerifierJob;
use bss_ledger::infra::posting::service::PostingService;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use bss_ledger_sdk::{AccountClass, MappingStatus, Side, SourceDocType};
use chrono::NaiveDate;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

/// A migrated container, its raw connection and a repository provider whose
/// search path resolves the unqualified entities into `bss`.
async fn boot() -> (
    ContainerAsync<Postgres>,
    DatabaseConnection,
    DBProvider<DbError>,
) {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let provider = DBProvider::new(connect_db(&repo_url, ConnectOpts::default()).await.unwrap());
    (container, raw, provider)
}

async fn scalar_bool(raw: &DatabaseConnection, sql: &str) -> Option<bool> {
    raw.query_one_raw(pg(sql))
        .await
        .unwrap()
        .and_then(|row| row.try_get_by_index::<Option<bool>>(0).unwrap())
}

async fn count(raw: &DatabaseConnection, sql: &str) -> i64 {
    raw.query_one_raw(pg(sql))
        .await
        .unwrap()
        .map_or(0, |row| row.try_get_by_index::<i64>(0).unwrap())
}

/// The validator accepts exactly canonical text within the 28-digit bound and
/// the stored scale, and rejects every other spelling a regex or bound slip
/// would let through.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn decimal_validator_accepts_canonical_text_and_rejects_the_rest() {
    let (_c, raw, _) = boot().await;
    let accepted = [
        ("0", "2"),
        ("12.34", "2"),
        ("-12.34", "2"),
        ("12.3", "2"),
        ("9999999999999999999999999999", "0"),
        ("-9999999999999999999999999999", "0"),
        ("0.0000000000000000000000000001", "28"),
        ("-0.0000000000000000000000000001", "28"),
        ("1", "28"),
    ];
    for (value, scale) in accepted {
        assert_eq!(
            scalar_bool(
                &raw,
                &format!("SELECT bss.ledger_decimal_valid('{value}', {scale})")
            )
            .await,
            Some(true),
            "{value} at scale {scale} must be accepted"
        );
    }
    assert_eq!(
        scalar_bool(&raw, "SELECT bss.ledger_decimal_valid(NULL, 2)").await,
        Some(true),
        "NULL is left to the column's NOT NULL"
    );
    let rejected = [
        ("1.00", "2"),
        ("1.0", "2"),
        ("01", "2"),
        ("00", "2"),
        ("-0", "2"),
        ("1.234", "2"),
        ("0.01", "0"),
        ("10000000000000000000000000000", "0"),
        ("999999999999999999999999999.99", "2"),
        ("1e2", "2"),
        ("+1", "2"),
        (" 1", "2"),
        ("", "2"),
        ("1.", "2"),
        (".5", "2"),
        ("1", "29"),
        ("1", "-1"),
        ("1", "NULL"),
    ];
    for (value, scale) in rejected {
        assert_eq!(
            scalar_bool(
                &raw,
                &format!("SELECT bss.ledger_decimal_valid('{value}', {scale})")
            )
            .await,
            Some(false),
            "{value:?} at scale {scale} must be rejected"
        );
    }
}

/// Runs `tests/fixtures/decimal_currency_scale_immutable.sql`: a first
/// registration disagreeing with stored transaction or functional scales is
/// locked, as is an UPDATE once postings exist; same-value updates, currencies
/// without postings and other tenants pass.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn currency_scale_lock_fixture_passes() {
    let (_c, raw, _) = boot().await;
    raw.execute_unprepared(include_str!(
        "fixtures/decimal_currency_scale_immutable.sql"
    ))
    .await
    .expect("every lock assertion in the fixture holds");
    assert_eq!(
        count(
            &raw,
            "SELECT COUNT(*) FROM bss.ledger_currency_scale_registry \
             WHERE tenant_id = '00000000-0000-0000-0000-000000000001'"
        )
        .await,
        3,
        "EUR@2, USD@3 and JPY registered for the fixture tenant"
    );
}

/// Seed one entry with two balanced lines whose functional money disagrees on
/// metadata, and COMMIT.
async fn commit_entry_with_functional(
    raw: &DatabaseConnection,
    debit: (&str, i16),
    credit: (&str, i16),
) -> Result<(), sea_orm::DbErr> {
    let tenant = Uuid::now_v7();
    let entry = Uuid::now_v7();
    let txn = raw.begin().await?;
    txn.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_journal_entry
            (entry_id, tenant_id, legal_entity_id, period_id, entry_currency,
             source_doc_type, source_business_id, posted_at_utc, effective_at,
             origin, posted_by_actor_id, correlation_id)
         VALUES ('{entry}','{tenant}','{tenant}','202606','EUR',
                 'MANUAL_ADJUSTMENT','fx-meta', now(), CURRENT_DATE,
                 'SYSTEM','{tenant}','{tenant}')"
    )))
    .await?;
    for (side, class, (code, scale)) in [("DR", "AR", debit), ("CR", "CASH_CLEARING", credit)] {
        txn.execute_raw(pg(format!(
            "INSERT INTO bss.ledger_journal_line
                (line_id, entry_id, tenant_id, period_id, payer_tenant_id, account_id,
                 account_class, side, amount, currency, currency_scale, mapping_status,
                 functional_amount, functional_currency, functional_currency_scale)
             VALUES ('{line}','{entry}','{tenant}','202606','{tenant}','{tenant}',
                     '{class}','{side}','10','EUR',2,'RESOLVED','11','{code}',{scale})",
            line = Uuid::now_v7()
        )))
        .await?;
    }
    txn.commit().await
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn functional_metadata_mismatch_is_rejected_at_commit() {
    let (_c, raw, _) = boot().await;
    for (debit, credit) in [(("USD", 2), ("USD", 3)), (("USD", 2), ("GBP", 2))] {
        let err = commit_entry_with_functional(&raw, debit, credit)
            .await
            .expect_err("functional lines in different metadata must not commit");
        assert!(
            err.to_string()
                .contains("LEDGER_ENTRY_FUNCTIONAL_METADATA_MISMATCH"),
            "{debit:?} vs {credit:?}: {err}"
        );
    }
    commit_entry_with_functional(&raw, ("USD", 2), ("USD", 2))
        .await
        .expect("matching functional metadata commits");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn dual_control_policy_versions_and_thresholds_are_append_only() {
    let (_c, raw, _) = boot().await;
    let tenant = Uuid::now_v7();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_dual_control_policy
            (tenant_id, version, effective_from, a6_backdating_biz_days,
             pending_ttl_seconds, created_at_utc)
         VALUES ('{tenant}', 1, now(), 5, 604800, now())"
    )))
    .await
    .unwrap();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_dual_control_policy_threshold
            (tenant_id, version, currency, currency_scale, amount)
         VALUES ('{tenant}', 1, 'USD', 2, '1000')"
    )))
    .await
    .unwrap();
    for statement in [
        format!(
            "UPDATE bss.ledger_dual_control_policy SET a6_backdating_biz_days = 6 \
             WHERE tenant_id = '{tenant}'"
        ),
        format!(
            "UPDATE bss.ledger_dual_control_policy_threshold SET amount = '2000' \
             WHERE tenant_id = '{tenant}'"
        ),
        format!(
            "DELETE FROM bss.ledger_dual_control_policy_threshold WHERE tenant_id = '{tenant}'"
        ),
        format!("DELETE FROM bss.ledger_dual_control_policy WHERE tenant_id = '{tenant}'"),
    ] {
        let err = raw
            .execute_raw(pg(statement.clone()))
            .await
            .expect_err("a stored policy version is evidence and never changes");
        assert!(
            err.to_string().contains("append-only"),
            "{statement}: {err}"
        );
    }
    assert_eq!(
        count(
            &raw,
            &format!(
                "SELECT COUNT(*) FROM bss.ledger_dual_control_policy_threshold \
                 WHERE tenant_id = '{tenant}' AND amount = '1000'"
            )
        )
        .await,
        1
    );
}

/// The lock identity spans both currency scales and the rate: snapshots that
/// differ only there are distinct rows, an exact duplicate is refused.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn snapshot_identity_spans_both_scales_and_the_rate() {
    let (_c, raw, _) = boot().await;
    let tenant = Uuid::now_v7();
    let insert = |base_scale: i16, quote_scale: i16, rate: &str| {
        pg(format!(
            "INSERT INTO bss.ledger_fx_rate_snapshot
                (tenant_id, rate_id, base_currency, base_currency_scale, quote_currency,
                 quote_currency_scale, rate, as_of, provider, fallback_order)
             VALUES ('{tenant}','{rate_id}','EUR',{base_scale},'JPY',{quote_scale},'{rate}',
                     '2026-06-01T00:00:00Z','ecb',0)",
            rate_id = Uuid::now_v7()
        ))
    };
    for (base_scale, quote_scale, rate) in [
        (2, 0, "160.5"),
        (3, 0, "160.5"),
        (2, 2, "160.5"),
        (2, 0, "160.25"),
    ] {
        raw.execute_raw(insert(base_scale, quote_scale, rate))
            .await
            .unwrap_or_else(|e| {
                panic!("{base_scale}/{quote_scale}/{rate} is a distinct identity: {e}")
            });
    }
    let err = raw
        .execute_raw(insert(2, 0, "160.5"))
        .await
        .expect_err("an identical lock identity under a new rate_id is refused");
    assert!(
        err.to_string().contains("uq_fx_rate_snapshot_lock"),
        "{err}"
    );
    assert_eq!(
        count(
            &raw,
            &format!(
                "SELECT COUNT(*) FROM bss.ledger_fx_rate_snapshot WHERE tenant_id = '{tenant}'"
            )
        )
        .await,
        4
    );
}

/// A tenant with USD@2, an OPEN period and an AR/CASH account pair; posts one
/// balanced 10.00 USD entry and returns the posted line ids.
async fn posted_chain(raw: &DatabaseConnection, provider: &DBProvider<DbError>) -> (Uuid, Uuid) {
    let tenant = Uuid::now_v7();
    let (ar, cash) = (Uuid::now_v7(), Uuid::now_v7());
    let reference = ReferenceRepo::new(provider.clone());
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "USD".to_owned(),
            currency_scale: 2,
            source: "iso".to_owned(),
        })
        .await
        .unwrap();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status)
         VALUES ('{tenant}','{tenant}','202606','UTC','OPEN')"
    )))
    .await
    .unwrap();
    for (account_id, class, normal) in [(ar, "AR", "DR"), (cash, "CASH_CLEARING", "CR")] {
        reference
            .insert_account(AccountRow {
                account_id,
                tenant_id: tenant,
                legal_entity_id: tenant,
                account_class: class.to_owned(),
                currency: "USD".to_owned(),
                revenue_stream: None,
                normal_side: normal.to_owned(),
                may_go_negative: false,
                lifecycle_state: "OPEN".to_owned(),
            })
            .await
            .unwrap();
    }
    let money = bss_ledger_sdk::PostedMoney::try_new(
        rust_decimal::Decimal::new(1000, 2),
        bss_ledger_sdk::CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap();
    let line = |account_id: Uuid, account_class: AccountClass, side: Side| NewLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: tenant,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id,
        account_class,
        gl_code: None,
        side,
        money: money.clone(),
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
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
    };
    let entry = NewEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: "202606".to_owned(),
        entry_currency: "USD".to_owned(),
        source_doc_type: SourceDocType::ManualAdjustment,
        source_business_id: "tamper-1".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: tenant,
        correlation_id: tenant,
        rounding_evidence: serde_json::Value::Null,
        rate_snapshot_ref: None,
    };
    let lines = vec![
        line(ar, AccountClass::Ar, Side::Debit),
        line(cash, AccountClass::CashClearing, Side::Credit),
    ];
    let debit_line = lines[0].line_id;
    PostingService::new(provider.clone(), Arc::new(LedgerEventPublisher::noop()))
        .post(
            &SecurityContext::anonymous(),
            &AccessScope::for_tenant(tenant),
            entry,
            lines,
            None,
        )
        .await
        .expect("post");
    (tenant, debit_line)
}

/// Rewrite one sealed line column out-of-band, the append-only trigger off.
async fn tamper_line(
    raw: &DatabaseConnection,
    line: Uuid,
    set: &str,
) -> Result<(), sea_orm::DbErr> {
    raw.execute_raw(pg(
        "ALTER TABLE bss.ledger_journal_line DISABLE TRIGGER trg_journal_line_append_only",
    ))
    .await?;
    let result = raw
        .execute_raw(pg(format!(
            "UPDATE bss.ledger_journal_line SET {set} WHERE line_id = '{line}'"
        )))
        .await
        .map(|_| ());
    raw.execute_raw(pg(
        "ALTER TABLE bss.ledger_journal_line ENABLE TRIGGER trg_journal_line_append_only",
    ))
    .await?;
    result
}

async fn verify_and_read_freeze(
    raw: &DatabaseConnection,
    provider: &DBProvider<DbError>,
    tenant: Uuid,
) -> Option<String> {
    ChainVerifierJob::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(NoopLedgerMetrics),
    )
    .run()
    .await
    .expect("verify run");
    raw.query_one_raw(pg(format!(
        "SELECT reason FROM bss.scope_freeze WHERE tenant_id = '{tenant}' AND cleared_at IS NULL"
    )))
    .await
    .unwrap()
    .map(|row| row.try_get_by_index::<String>(0).unwrap())
}

/// The verifier recomputes the row hash from the decoded line money: a changed
/// amount, and the same digits relabelled at another scale, both break the
/// chain and freeze the tenant. A non-canonical rendering of the same value
/// never reaches the verifier: the money CHECK refuses it even with the
/// append-only trigger disabled.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn verifier_freezes_a_tampered_line_amount_and_the_check_refuses_noncanonical_text() {
    let (_c, raw, provider) = boot().await;

    let (tenant, line) = posted_chain(&raw, &provider).await;
    let noncanonical = tamper_line(&raw, line, "amount = '10.00'").await;
    assert!(
        noncanonical
            .as_ref()
            .is_err_and(|e| e.to_string().contains("check constraint")),
        "a non-canonical amount is refused by the money CHECK: {noncanonical:?}"
    );
    assert_eq!(
        verify_and_read_freeze(&raw, &provider, tenant).await,
        None,
        "clean chain"
    );

    tamper_line(&raw, line, "amount = '11'")
        .await
        .expect("tamper amount");
    let reason = verify_and_read_freeze(&raw, &provider, tenant)
        .await
        .expect("a changed amount freezes the tenant");
    assert!(reason.contains("ROW_HASH_MISMATCH"), "{reason}");

    let (tenant, line) = posted_chain(&raw, &provider).await;
    tamper_line(&raw, line, "currency_scale = 3")
        .await
        .expect("relabel scale");
    let reason = verify_and_read_freeze(&raw, &provider, tenant)
        .await
        .expect("a relabelled scale freezes the tenant");
    assert!(reason.contains("ROW_HASH_MISMATCH"), "{reason}");
}
