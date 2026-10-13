//! Actual migrated SQLite lifecycle, storage validation, isolation and CAS tests.
use super::*;
use crate::infra::posting::retry::{AttemptError, retry_transaction};
use crate::infra::storage::repo::PaymentRepo;
use bss_ledger_sdk::{CurrencySpec, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::Db;
use toolkit_db::{ConnectOpts, connect_db};

fn money(s: &str) -> PostedMoney {
    spec_money(s, "EUR", 2)
}
fn spec_money(s: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(s).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
async fn setup() -> (DisputeRepo, Db, Uuid) {
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
        DisputeRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
    )
}

#[tokio::test]
async fn lifecycle_cycles_and_hold_survive_settlement_reduction() {
    let (r, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let payment = PaymentRepo::new(DBProvider::new(db.clone()));
    retry_transaction(&db, |tx| {
        let r = r.clone();
        let scope = scope.clone();
        let payment = payment.clone();
        Box::pin(async move {
            let scope = &scope;
            assert!(matches!(
                r.dispute_upsert(
                    tx,
                    scope,
                    tenant,
                    "d",
                    "p",
                    DisputeVariant::CashHold,
                    2,
                    &money("0.1"),
                    &money("0.09")
                )
                .await,
                Err(RepoError::DisputeNotOpen(_))
            ));
            payment
                .seed_settlement(tx, scope, tenant, "p", &money("0.1"), &money("0.01"))
                .await?;
            r.dispute_upsert(
                tx,
                scope,
                tenant,
                "d",
                "p",
                DisputeVariant::CashHold,
                1,
                &money("0.1"),
                &money("0.09"),
            )
            .await?;
            let opened = r.read_dispute_in(tx, scope, tenant, "d").await?.unwrap();
            assert_eq!(opened.version, 0);
            assert_eq!(
                r.read_open_dispute_for_payment_in(tx, scope, tenant, "p")
                    .await?
                    .unwrap(),
                opened
            );
            for cycle in [0, 1, 2] {
                assert!(matches!(
                    r.dispute_upsert(
                        tx,
                        scope,
                        tenant,
                        "d",
                        "p",
                        DisputeVariant::CashHold,
                        cycle,
                        &money("0.1"),
                        &money("0.09")
                    )
                    .await,
                    Err(RepoError::DisputeNotOpen(_))
                ));
            }
            assert!(matches!(
                r.dispute_advance(tx, scope, tenant, "d", DisputePhase::Won, 2, &money("0.1"))
                    .await,
                Err(RepoError::DisputeNotOpen(_))
            ));
            payment
                .add_fee(tx, scope, tenant, "p", &money("-0.01"))
                .await?;
            payment
                .add_settled(tx, scope, tenant, "p", &money("-0.1"))
                .await?;
            r.dispute_advance(tx, scope, tenant, "d", DisputePhase::Won, 1, &money("0.1"))
                .await?;
            let won = r.read_dispute_in(tx, scope, tenant, "d").await?.unwrap();
            assert_eq!(won.cash_hold, money("0.09"));
            assert_eq!(won.version, 1);
            assert!(matches!(
                r.dispute_advance(tx, scope, tenant, "d", DisputePhase::Lost, 1, &money("0.1"))
                    .await,
                Err(RepoError::DisputeNotOpen(_))
            ));
            for cycle in [0, 1, 3] {
                assert!(matches!(
                    r.dispute_upsert(
                        tx,
                        scope,
                        tenant,
                        "d",
                        "p",
                        DisputeVariant::CashHold,
                        cycle,
                        &money("0.1"),
                        &money("0.09")
                    )
                    .await,
                    Err(RepoError::DisputeNotOpen(_))
                ));
            }
            r.dispute_upsert(
                tx,
                scope,
                tenant,
                "d",
                "p",
                DisputeVariant::ArReclass,
                2,
                &money("0.01"),
                &money("0"),
            )
            .await?;
            let reopened = r.read_dispute_in(tx, scope, tenant, "d").await?.unwrap();
            assert_eq!(reopened.variant, DisputeVariant::ArReclass);
            assert_eq!(reopened.cycle, 2);
            assert_eq!(reopened.version, 2);
            r.dispute_advance(
                tx,
                scope,
                tenant,
                "d",
                DisputePhase::Lost,
                2,
                &money("0.01"),
            )
            .await?;
            r.dispute_upsert(
                tx,
                scope,
                tenant,
                "d",
                "p",
                DisputeVariant::CashHold,
                3,
                &money("0.01"),
                &money("0.01"),
            )
            .await?;
            r.dispute_advance(
                tx,
                scope,
                tenant,
                "d",
                DisputePhase::Partial,
                3,
                &money("0.01"),
            )
            .await?;
            assert!(matches!(
                r.dispute_upsert(
                    tx,
                    scope,
                    tenant,
                    "d",
                    "p",
                    DisputeVariant::CashHold,
                    4,
                    &money("0.01"),
                    &money("0.01")
                )
                .await,
                Err(RepoError::DisputeNotOpen(_))
            ));
            Ok(())
        })
    })
    .await
    .unwrap();
    let row = r.read_dispute(&scope, tenant, "d").await.unwrap().unwrap();
    assert_eq!(row.version, 5);
    assert!(
        r.read_open_dispute_for_payment(&scope, tenant, "p")
            .await
            .unwrap()
            .is_none()
    );
}

#[allow(clippy::too_many_lines)] // one scenario end to end; splitting hides the state transitions
#[tokio::test]
async fn money_identity_caps_and_scope() {
    let (r, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    retry_transaction(&db, |tx| {
        let r = r.clone();
        let scope = scope.clone();
        Box::pin(async move {
            let scope = &scope;
            for (amount, hold) in [("-0.01", "0"), ("0.01", "-0.01"), ("0.01", "0.09")] {
                assert!(matches!(
                    r.dispute_upsert(
                        tx,
                        scope,
                        tenant,
                        "bad",
                        "p",
                        DisputeVariant::CashHold,
                        1,
                        &money(amount),
                        &money(hold)
                    )
                    .await,
                    Err(RepoError::MoneyOutCapExceeded(_))
                ));
            }
            r.dispute_upsert(
                tx,
                scope,
                tenant,
                "d",
                "p",
                DisputeVariant::CashHold,
                1,
                &money("0.1"),
                &money("0.09"),
            )
            .await?;
            assert!(matches!(
                r.dispute_advance(tx, scope, tenant, "d", DisputePhase::Won, 1, &money("0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            for other in [spec_money("0.1", "USD", 2), spec_money("0.1", "EUR", 3)] {
                assert!(matches!(
                    r.dispute_advance(tx, scope, tenant, "d", DisputePhase::Won, 1, &other)
                        .await,
                    Err(RepoError::Money(_))
                ));
            }
            r.dispute_advance(tx, scope, tenant, "d", DisputePhase::Won, 1, &money("0.1"))
                .await?;
            assert!(matches!(
                r.dispute_upsert(
                    tx,
                    scope,
                    tenant,
                    "d",
                    "other",
                    DisputeVariant::CashHold,
                    2,
                    &money("0.1"),
                    &money("0.09")
                )
                .await,
                Err(RepoError::DisputeNotOpen(_))
            ));
            for other in [spec_money("0.1", "USD", 2), spec_money("0.1", "EUR", 3)] {
                assert!(matches!(
                    r.dispute_upsert(
                        tx,
                        scope,
                        tenant,
                        "d",
                        "p",
                        DisputeVariant::CashHold,
                        2,
                        &other,
                        &other
                    )
                    .await,
                    Err(RepoError::Money(_))
                ));
            }
            // Separate dispute keys on the same payment remain legal.
            r.dispute_upsert(
                tx,
                scope,
                tenant,
                "a",
                "p",
                DisputeVariant::CashHold,
                1,
                &spec_money("1", "EUR", 0),
                &spec_money("0", "EUR", 0),
            )
            .await?;
            r.dispute_upsert(
                tx,
                scope,
                tenant,
                "z",
                "p",
                DisputeVariant::CashHold,
                1,
                &spec_money("0.0000000000000000000000000001", "EUR", 28),
                &spec_money("0", "EUR", 28),
            )
            .await?;
            for denied in [
                AccessScope::deny_all(),
                AccessScope::for_tenant(Uuid::now_v7()),
            ] {
                assert!(r.read_dispute_in(tx, &denied, tenant, "d").await?.is_none());
                assert!(
                    r.read_open_dispute_for_payment_in(tx, &denied, tenant, "p")
                        .await?
                        .is_none()
                );
                assert!(
                    r.list_disputes_in(tx, &denied, tenant, &ODataQuery::default())
                        .await
                        .unwrap()
                        .items
                        .is_empty()
                );
                assert!(matches!(
                    r.dispute_advance(
                        tx,
                        &denied,
                        tenant,
                        "d",
                        DisputePhase::Won,
                        1,
                        &money("0.1")
                    )
                    .await,
                    Err(RepoError::DisputeNotOpen(_))
                ));
                assert!(matches!(
                    r.dispute_upsert(
                        tx,
                        &denied,
                        tenant,
                        "new",
                        "p",
                        DisputeVariant::CashHold,
                        1,
                        &money("0.1"),
                        &money("0")
                    )
                    .await,
                    Err(RepoError::Db(_))
                ));
            }
            Ok(())
        })
    })
    .await
    .unwrap();
    let page = r
        .list_disputes(&scope, tenant, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|s| s.dispute_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "d", "z"]
    );
    assert_eq!(page.page_info.limit, 25);
    let page1 = r
        .list_disputes(&scope, tenant, &ODataQuery::default().with_limit(2))
        .await
        .unwrap();
    assert_eq!(
        page1
            .items
            .iter()
            .map(|s| s.dispute_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "d"]
    );
    let cursor =
        toolkit_odata::CursorV1::decode(page1.page_info.next_cursor.as_deref().unwrap()).unwrap();
    let page2 = r
        .list_disputes(
            &scope,
            tenant,
            &ODataQuery::default().with_limit(2).with_cursor(cursor),
        )
        .await
        .unwrap();
    assert_eq!(
        page2
            .items
            .iter()
            .map(|s| s.dispute_id.as_str())
            .collect::<Vec<_>>(),
        ["z"]
    );
    let filter = toolkit_odata::parse_filter_string("last_phase eq 'WON' and payment_id eq 'p'")
        .unwrap()
        .into_expr();
    let filtered = r
        .list_disputes(&scope, tenant, &ODataQuery::default().with_filter(filter))
        .await
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].dispute_id, "d");
}

#[tokio::test]
async fn stale_version_and_insert_race_abort_and_rollback() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (repo, db, tenant) = setup().await;
    for insert_race in [false, true] {
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let r = repo.clone();
        let payment = PaymentRepo::new(DBProvider::new(db.clone()));
        let result = retry_transaction(&db, move |tx| {
            let r = r.clone();
            let payment = payment.clone();
            let count = count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                let scope = AccessScope::for_tenant(tenant);
                assert!(r.read_dispute_in(tx, &scope, tenant, "d").await?.is_none());
                r.dispute_upsert(
                    tx,
                    &scope,
                    tenant,
                    "d",
                    "p",
                    DisputeVariant::CashHold,
                    1,
                    &money("0.1"),
                    &money("0.09"),
                )
                .await?;
                let observed = r.read_dispute_in(tx, &scope, tenant, "d").await?.unwrap();
                payment
                    .seed_settlement(tx, &scope, tenant, "p", &money("0.1"), &money("0"))
                    .await?;
                if insert_race {
                    r.insert_open(tx, &scope, &observed).await?;
                } else {
                    // Real literal version-only update: phase and cycle remain valid.
                    dispute::Entity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .col_expr(dispute::Column::Version, Expr::value(1_i64))
                        .filter(key(tenant, "d"))
                        .exec(tx)
                        .await
                        .map_err(|e| scope_to_repo(e, r.db.db().backend()))?;
                    let next = DisputeState {
                        last_phase: DisputePhase::Won,
                        ..observed.clone()
                    };
                    r.replace_observed(tx, &scope, &observed, &next).await?;
                }
                Ok(())
            })
        })
        .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::ConcurrentModification(_))
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(
            repo.read_dispute(&AccessScope::for_tenant(tenant), tenant, "d")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            PaymentRepo::new(DBProvider::new(db.clone()))
                .read_settlement(&AccessScope::for_tenant(tenant), tenant, "p")
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn corrupted_storage_list_projection_and_version_exhaustion_fail_closed() {
    let (r, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    retry_transaction(&db, |tx| {
        let r = r.clone();
        let scope = scope.clone();
        Box::pin(async move {
            r.dispute_upsert(
                tx,
                &scope,
                tenant,
                "d",
                "p",
                DisputeVariant::CashHold,
                1,
                &money("0.1"),
                &money("0.09"),
            )
            .await?;
            let mut model = dispute::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(key(tenant, "d"))
                .one(tx)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(model.disputed_amount, "0.1");
            assert_eq!(model.cash_hold, "0.09");
            // Exercise the canonical decoder with malformed fields on an actual row model.
            for text in ["garbage", "0.10", "NaN", "0.001"] {
                let mut bad = model.clone();
                bad.disputed_amount = text.into();
                assert!(matches!(decode(bad), Err(RepoError::InvalidStoredMoney(_))));
            }
            model.currency_scale = 29;
            assert!(matches!(
                decode(model.clone()),
                Err(RepoError::InvalidStoredMoney(_))
            ));
            model.currency_scale = 2;
            model.currency = String::new();
            assert!(matches!(
                decode(model.clone()),
                Err(RepoError::InvalidStoredMoney(_))
            ));
            model.currency = "EUR".into();
            for field in [0, 1, 2, 3] {
                let mut bad = model.clone();
                match field {
                    0 => bad.cycle = 0,
                    1 => bad.version = -1,
                    2 => bad.variant = "unknown".into(),
                    _ => bad.last_phase = "unknown".into(),
                }
                assert!(matches!(decode(bad), Err(RepoError::InvalidStoredMoney(_))));
            }
            dispute::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .col_expr(dispute::Column::Version, Expr::value(i64::MAX))
                .filter(key(tenant, "d"))
                .exec(tx)
                .await
                .unwrap();
            assert!(matches!(
                r.dispute_advance(tx, &scope, tenant, "d", DisputePhase::Won, 1, &money("0.1"))
                    .await,
                Err(RepoError::Db(_))
            ));
            assert_eq!(
                r.read_dispute_in(tx, &scope, tenant, "d")
                    .await?
                    .unwrap()
                    .last_phase,
                DisputePhase::Opened
            );
            for text in ["garbage", "0.10", "0.001"] {
                dispute::Entity::update_many()
                    .secure()
                    .scope_with(&scope)
                    .col_expr(dispute::Column::DisputedAmount, Expr::value(text))
                    .filter(key(tenant, "d"))
                    .exec(tx)
                    .await
                    .unwrap();
                assert!(matches!(
                    r.read_dispute_in(tx, &scope, tenant, "d").await,
                    Err(RepoError::InvalidStoredMoney(_))
                ));
                assert!(matches!(
                    r.list_disputes_in(tx, &scope, tenant, &ODataQuery::default())
                        .await,
                    Err(OdataPageError::Db(_))
                ));
            }
            dispute::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .col_expr(dispute::Column::DisputedAmount, Expr::value("0.1"))
                .filter(key(tenant, "d"))
                .exec(tx)
                .await
                .unwrap();
            // SQLite intentionally omits the cross-column numeric TEXT cap; decoder owns it.
            dispute::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .col_expr(dispute::Column::CashHold, Expr::value("0.2"))
                .filter(key(tenant, "d"))
                .exec(tx)
                .await
                .unwrap();
            assert!(matches!(
                r.read_dispute_in(tx, &scope, tenant, "d").await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            assert!(matches!(
                r.list_disputes_in(tx, &scope, tenant, &ODataQuery::default())
                    .await,
                Err(OdataPageError::Db(_))
            ));
            assert!(matches!(
                r.dispute_advance(tx, &scope, tenant, "d", DisputePhase::Won, 1, &money("0.2"))
                    .await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            Ok(())
        })
    })
    .await
    .unwrap();
    assert!(matches!(
        r.list_disputes(&scope, tenant, &ODataQuery::default())
            .await,
        Err(OdataPageError::Db(_))
    ));
}

#[tokio::test]
async fn actual_driver_contention_is_conflict_and_committed_winner_survives() {
    use toolkit_db::secure::TxConfig;
    let path = std::env::temp_dir().join(format!("ledger-dispute-{}.sqlite", Uuid::new_v4()));
    let dsn = format!("sqlite://{}?mode=rwc&busy_timeout=1", path.display());
    let db = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let r = DisputeRepo::new(DBProvider::new(db.clone()));
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    retry_transaction(&db, |tx| {
        let r = r.clone();
        let scope = scope.clone();
        Box::pin(async move {
            r.dispute_upsert(
                tx,
                &scope,
                tenant,
                "d",
                "p",
                DisputeVariant::CashHold,
                1,
                &money("0.1"),
                &money("0.09"),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let contender_db = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    let contender = DisputeRepo::new(DBProvider::new(contender_db.clone()));
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let held = tokio::spawn(async move {
        db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                r.dispute_advance(tx, &scope, tenant, "d", DisputePhase::Won, 1, &money("0.1"))
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
            r.dispute_advance(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                "d",
                DisputePhase::Lost,
                1,
                &money("0.1"),
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
        .read_dispute(&AccessScope::for_tenant(tenant), tenant, "d")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.last_phase, DisputePhase::Won);
    assert_eq!(row.version, 1);
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = attempts.clone();
    let result = retry_transaction(&contender_db, |tx| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let r = contender.clone();
        Box::pin(async move {
            r.dispute_advance(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                "d",
                DisputePhase::Lost,
                1,
                &money("0.1"),
            )
            .await?;
            Ok(())
        })
    })
    .await;
    assert!(matches!(
        result,
        Err(crate::domain::error::DomainError::InvalidDisputeTransition(
            _
        ))
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
