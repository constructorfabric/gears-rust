//! Actual identity, exact quote/translation, immutable snapshot and caller rollback tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use bss_ledger_sdk::{AccountClass, MappingStatus, Side};
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

use super::*;
use crate::config::FxConfig;
use crate::domain::model::NewLine;
use crate::infra::posting::retry::AttemptError;
use crate::infra::storage::repo::FxRepo;
use time::OffsetDateTime;

/// Original scale-2 fixture amounts, expressed immediately as major-unit PostedMoney.
fn ar_line(amount_minor: i64, side: Side, currency: &str) -> NewLine {
    NewLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::now_v7(),
        account_class: AccountClass::Ar,
        gl_code: None,
        side,
        money: PostedMoney::try_new(
            rust_decimal::Decimal::new(amount_minor, 2),
            spec(currency, 2),
        )
        .unwrap(),
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
    }
}

/// Build a `RateLocker` over a bare in-memory SQLite provider (no migrations) —
/// enough for the single-currency short-circuit, which never touches the DB.
async fn locker_no_db() -> RateLocker {
    // A shared-cache in-memory SQLite DB; the single-currency path returns before
    // any query, so the (empty) schema is irrelevant.
    let db = connect_db(
        "sqlite:file:fx_rate_locker_unit?mode=memory&cache=shared",
        ConnectOpts::default(),
    )
    .await
    .unwrap();
    let provider = DBProvider::<DbError>::new(db);
    let repo = FxRepo::new(provider);
    let source = RateSource::new(repo.clone(), FxConfig::default());
    RateLocker::new(source, repo)
}

#[tokio::test]
async fn single_currency_returns_none_and_leaves_functional_null() {
    let locker = locker_no_db().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    // A balanced single-currency entry (USD == USD): no FX, no stamping.
    let mut lines = vec![
        ar_line(1000, Side::Debit, "USD"),
        ar_line(1000, Side::Credit, "USD"),
    ];

    let result = locker
        .lock_and_stamp(
            &scope,
            tenant,
            &mut lines,
            &spec("USD", 2),
            &spec("USD", 2),
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("single-currency lock must succeed");

    // No snapshot minted.
    assert_eq!(result, None, "single-currency must return Ok(None)");
    // Functional columns untouched on every line.
    for line in &lines {
        assert_eq!(line.functional_money, None);
    }
}

fn spec(code: &str, scale: u8) -> CurrencySpec {
    CurrencySpec::try_new(code.into(), scale).unwrap()
}
async fn setup() -> (RateLocker, FxRepo, toolkit_db::secure::Db, Uuid) {
    use sea_orm_migration::MigratorTrait;
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let repo = FxRepo::new(DBProvider::new(db.clone()));
    let source = RateSource::new(repo.clone(), FxConfig::default());
    (
        RateLocker::new(source, repo.clone()),
        repo,
        db,
        Uuid::now_v7(),
    )
}
async fn quote(repo: &FxRepo, tenant: Uuid, base: &str, target: &str, value: &str) {
    repo.upsert_rate(&crate::infra::storage::repo::NewFxRate {
        tenant_id: tenant,
        base_currency: base.into(),
        quote_currency: target.into(),
        provider: "ecb".into(),
        rate: rust_decimal::Decimal::from_str_exact(value).unwrap(),
        as_of: OffsetDateTime::now_utc(),
        fallback_order: 0,
    })
    .await
    .unwrap();
}
fn balanced(code: &str) -> Vec<NewLine> {
    vec![
        ar_line(1234, Side::Debit, code),
        ar_line(1234, Side::Credit, code),
    ]
}
#[tokio::test]
async fn exact_quote_cross_scale_and_snapshot_runner() {
    let (locker, repo, db, tenant) = setup().await;
    quote(&repo, tenant, "EUR", "JPY", "160").await;
    let scope = AccessScope::for_tenant(tenant);
    let id = db
        .transaction_ref_mapped_with_config(toolkit_db::secure::TxConfig::serializable(), |tx| {
            let locker = locker.clone();
            let repo = repo.clone();
            let scope = scope.clone();
            Box::pin(async move {
                let mut lines = balanced("EUR");
                let id = locker
                    .lock_and_stamp_in(
                        tx,
                        &scope,
                        tenant,
                        &mut lines,
                        &spec("EUR", 2),
                        &spec("JPY", 0),
                        OffsetDateTime::now_utc(),
                    )
                    .await?
                    .unwrap();
                assert_eq!(
                    lines[0]
                        .functional_money
                        .as_ref()
                        .unwrap()
                        .amount()
                        .to_string(),
                    "1974"
                );
                let frozen = repo
                    .read_snapshot_in(tx, &scope, tenant, id)
                    .await?
                    .unwrap();
                assert_eq!(frozen.quote.base_currency, spec("EUR", 2));
                assert_eq!(frozen.quote.quote_currency, spec("JPY", 0));
                Ok::<_, AttemptError>(id)
            })
        })
        .await
        .unwrap();
    assert!(
        repo.read_snapshot(&scope, tenant, id)
            .await
            .unwrap()
            .is_some()
    );
    quote(&repo, tenant, "EUR", "JPY", "161.1234567890123456789012345").await;
    assert_eq!(
        repo.read_snapshot(&scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .quote
            .rate
            .to_string(),
        "160"
    );
}
#[tokio::test]
async fn caller_error_rolls_snapshot_back() {
    let (locker, repo, db, tenant) = setup().await;
    quote(&repo, tenant, "EUR", "USD", "1.123456789012345678901234567").await;
    let scope = AccessScope::for_tenant(tenant);
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let result = db
        .transaction_ref_mapped_with_config(toolkit_db::secure::TxConfig::serializable(), |tx| {
            let locker = locker.clone();
            let scope = scope.clone();
            let seen = seen.clone();
            Box::pin(async move {
                let mut lines = balanced("EUR");
                let id = locker
                    .lock_and_stamp_in(
                        tx,
                        &scope,
                        tenant,
                        &mut lines,
                        &spec("EUR", 2),
                        &spec("USD", 2),
                        OffsetDateTime::now_utc(),
                    )
                    .await?
                    .unwrap();
                *seen.lock().await = Some(id);
                Err::<(), _>(AttemptError::Conflict)
            })
        })
        .await;
    assert!(matches!(result, Err(AttemptError::Conflict)));
    let id = seen.lock().await.unwrap();
    assert!(
        repo.read_snapshot(&scope, tenant, id)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn metadata_before_identity_and_atomic_translation_failure() {
    let locker = locker_no_db().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let mut lines = balanced("USD");
    lines[1].money = PostedMoney::try_new(rust_decimal::Decimal::ZERO, spec("USD", 3)).unwrap();
    assert!(matches!(
        locker
            .lock_and_stamp(
                &scope,
                tenant,
                &mut lines,
                &spec("USD", 2),
                &spec("USD", 2),
                OffsetDateTime::now_utc()
            )
            .await,
        Err(DomainError::InconsistentScale(_))
    ));
    assert!(lines.iter().all(|l| l.functional_money.is_none()));
    let (locker, repo, _, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let mut lines = balanced("USD");
    assert!(matches!(
        locker
            .lock_and_stamp(
                &scope,
                tenant,
                &mut lines,
                &spec("USD", 2),
                &spec("USD", 3),
                OffsetDateTime::now_utc()
            )
            .await,
        Err(DomainError::FxRateUnavailable(_))
    ));
    quote(&repo, tenant, "USD", "JPY", "0.0000001").await;
    assert!(
        locker
            .lock_and_stamp(
                &scope,
                tenant,
                &mut lines,
                &spec("USD", 2),
                &spec("JPY", 0),
                OffsetDateTime::now_utc()
            )
            .await
            .is_err()
    );
    assert!(lines.iter().all(|l| l.functional_money.is_none()));
}

#[tokio::test]
async fn exact_digits_fallback_and_corrupt_stored_fail_closed() {
    use crate::infra::storage::{entity::fx_rate, repo::NewFxRate};
    use sea_orm::{EntityTrait, sea_query::Expr};
    use toolkit_db::secure::SecureUpdateExt;
    let (_, repo, db, tenant) = setup().await;
    let now = OffsetDateTime::now_utc();
    let scope = AccessScope::for_tenant(tenant);
    for (provider, age, value) in [
        ("primary", 25, "1.2"),
        ("fallback", 0, "1.123456789012345678901234567"),
    ] {
        repo.upsert_rate(&NewFxRate {
            tenant_id: tenant,
            base_currency: "EUR".into(),
            quote_currency: "USD".into(),
            provider: provider.into(),
            rate: rust_decimal::Decimal::from_str_exact(value).unwrap(),
            as_of: now - time::Duration::hours(age),
            fallback_order: 0,
        })
        .await
        .unwrap();
    }
    let source = RateSource::new(
        repo.clone(),
        FxConfig {
            provider_order: vec!["primary".into(), "fallback".into()],
            ..FxConfig::default()
        },
    );
    let conn = db.conn().unwrap();
    let resolved = source
        .resolve_in(&conn, &scope, tenant, "EUR", "USD", now)
        .await
        .unwrap();
    assert_eq!(resolved.rate.to_string(), "1.123456789012345678901234567");
    assert_eq!(resolved.provider, "fallback");
    assert_eq!(resolved.fallback_order, 1);
    assert!(matches!(
        source.resolve(&scope, tenant, "USD", "EUR", now).await,
        Err(DomainError::FxRateUnavailable(_))
    ));
    fx_rate::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(fx_rate::Column::Rate, Expr::value("1.20"))
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        source
            .resolve_in(&conn, &scope, tenant, "EUR", "USD", now)
            .await,
        Err(AttemptError::Business(DomainError::Internal(_)))
    ));
}

#[tokio::test]
async fn existing_evidence_and_ar_anchor_policy() {
    let (locker, repo, _, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    quote(&repo, tenant, "EUR", "USD", "1.5").await;
    let mut lines = vec![
        ar_line(2, Side::Debit, "EUR"),
        ar_line(1, Side::Credit, "EUR"),
        ar_line(1, Side::Credit, "EUR"),
    ];
    lines[0].account_class = AccountClass::Revenue;
    locker
        .lock_and_stamp(
            &scope,
            tenant,
            &mut lines,
            &spec("EUR", 2),
            &spec("USD", 2),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    // 0.03 debit versus 0.02+0.02 credit: first AR (credit) takes the -0.01 residual.
    assert_eq!(
        lines[0]
            .functional_money
            .as_ref()
            .unwrap()
            .amount()
            .to_string(),
        "0.03"
    );
    assert_eq!(
        lines[1]
            .functional_money
            .as_ref()
            .unwrap()
            .amount()
            .to_string(),
        "0.01"
    );
    assert_eq!(
        lines[2]
            .functional_money
            .as_ref()
            .unwrap()
            .amount()
            .to_string(),
        "0.02"
    );
    lines[2].functional_money =
        Some(PostedMoney::try_new(rust_decimal::Decimal::ONE, spec("USD", 2)).unwrap());
    let before: Vec<_> = lines.iter().map(|l| l.functional_money.clone()).collect();
    assert!(
        locker
            .lock_and_stamp(
                &scope,
                tenant,
                &mut lines,
                &spec("EUR", 2),
                &spec("USD", 2),
                OffsetDateTime::now_utc()
            )
            .await
            .is_err()
    );
    assert_eq!(
        before,
        lines
            .iter()
            .map(|l| l.functional_money.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn all_quote_digits_survive_snapshot_and_translation() {
    let (locker, repo, _, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let value = "0.1234567890123456789012345678";
    quote(&repo, tenant, "EUR", "USD", value).await;
    let mut lines = vec![
        ar_line(100, Side::Debit, "EUR"),
        ar_line(100, Side::Credit, "EUR"),
    ];
    let id = locker
        .lock_and_stamp(
            &scope,
            tenant,
            &mut lines,
            &spec("EUR", 2),
            &spec("USD", 28),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        lines[0]
            .functional_money
            .as_ref()
            .unwrap()
            .amount()
            .to_string(),
        value
    );
    assert_eq!(
        repo.read_snapshot(&scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .quote
            .rate
            .to_string(),
        value
    );
    quote(&repo, tenant, "JPY", "EUR", "0.00625").await;
    for line in &mut lines {
        line.money =
            PostedMoney::try_new(rust_decimal::Decimal::from(100), spec("JPY", 0)).unwrap();
        line.functional_money = None;
    }
    locker
        .lock_and_stamp(
            &scope,
            tenant,
            &mut lines,
            &spec("JPY", 0),
            &spec("EUR", 2),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(
        lines[0]
            .functional_money
            .as_ref()
            .unwrap()
            .amount()
            .to_string(),
        "0.62"
    );
}
