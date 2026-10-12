//! Actual migrated schema and repository checks for exact frozen quote evidence.
use super::*;
use sea_orm::sea_query::Expr;
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, SecureUpdateExt, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Repo(RepoError),
    #[error(transparent)]
    Database(DbError),
}
impl From<RepoError> for TestError {
    fn from(e: RepoError) -> Self {
        Self::Repo(e)
    }
}
impl From<DbError> for TestError {
    fn from(e: DbError) -> Self {
        Self::Database(e)
    }
}

async fn setup() -> (FxRepo, Db, Uuid) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    (FxRepo::new(DBProvider::new(db.clone())), db, Uuid::now_v7())
}
fn spec(code: &str, scale: u8) -> CurrencySpec {
    CurrencySpec::try_new(code.into(), scale).unwrap()
}
fn snapshot(tenant: Uuid) -> NewRateSnapshot {
    NewRateSnapshot {
        tenant_id: tenant,
        base_currency: spec("EUR", 2),
        quote_currency: spec("JPY", 0),
        rate: parse_decimal("0.1234567890123456789012345678").unwrap(),
        as_of: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        provider: "provider".into(),
        stale: false,
        fallback_order: 2,
        triangulated_via: Some("USD".into()),
    }
}
fn reference(snap: &NewRateSnapshot) -> NewFxRate {
    NewFxRate {
        tenant_id: snap.tenant_id,
        base_currency: snap.base_currency.code().into(),
        quote_currency: snap.quote_currency.code().into(),
        provider: snap.provider.clone(),
        rate: snap.rate,
        as_of: snap.as_of,
        fallback_order: snap.fallback_order,
    }
}

#[tokio::test]
async fn exact_reference_upsert_and_input_bounds() {
    let (repo, _, tenant) = setup().await;
    let snap = snapshot(tenant);
    let mut rate = reference(&snap);
    repo.upsert_rate(&rate).await.unwrap();
    let rows = repo.latest_rates(tenant, "EUR", "JPY").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rate, snap.rate);
    rate.rate = Decimal::from_str_exact("9.1200").unwrap();
    repo.upsert_rate(&rate).await.unwrap();
    let rows = repo.latest_rates(tenant, "EUR", "JPY").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(canonical_decimal(rows[0].rate), "9.12");
    assert!(
        repo.latest_rates(Uuid::now_v7(), "EUR", "JPY")
            .await
            .unwrap()
            .is_empty()
    );
    for invalid in [
        Decimal::ZERO,
        Decimal::NEGATIVE_ONE,
        Decimal::from_str_exact("10000000000000000000000000000").unwrap(),
    ] {
        rate.rate = invalid;
        assert!(matches!(
            repo.upsert_rate(&rate).await,
            Err(RepoError::Money(_))
        ));
        let mut bad = snap.clone();
        bad.rate = invalid;
        assert!(matches!(
            repo.insert_snapshot(&AccessScope::for_tenant(tenant), &bad)
                .await,
            Err(RepoError::Money(_))
        ));
    }
    assert_eq!(
        repo.latest_rates(tenant, "EUR", "JPY").await.unwrap()[0].rate,
        parse_decimal("9.12").unwrap()
    );
}

#[tokio::test]
async fn actual_snapshot_identity_includes_quote_and_both_scales() {
    let (repo, _, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let mut snap = snapshot(tenant);
    snap.rate = Decimal::from_str_exact("1.2300").unwrap();
    let original = repo.insert_snapshot(&scope, &snap).await.unwrap();
    snap.rate = parse_decimal("1.23").unwrap();
    assert_eq!(repo.insert_snapshot(&scope, &snap).await.unwrap(), original);
    let frozen = repo
        .read_snapshot(&scope, tenant, original)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frozen.quote.base_currency, spec("EUR", 2));
    assert_eq!(frozen.quote.quote_currency, spec("JPY", 0));
    assert_eq!(frozen.quote.rate, snap.rate);
    assert_eq!(frozen.quote.triangulated_via.as_deref(), Some("USD"));
    snap.rate = parse_decimal("1.24").unwrap();
    let corrected = repo.insert_snapshot(&scope, &snap).await.unwrap();
    assert_ne!(corrected, original);
    snap.rate = parse_decimal("1.23").unwrap();
    snap.base_currency = spec("EUR", 3);
    let base_scale = repo.insert_snapshot(&scope, &snap).await.unwrap();
    assert_ne!(base_scale, original);
    assert_eq!(
        repo.insert_snapshot(&scope, &snap).await.unwrap(),
        base_scale
    );
    snap.base_currency = spec("EUR", 2);
    snap.quote_currency = spec("JPY", 8);
    let quote_scale = repo.insert_snapshot(&scope, &snap).await.unwrap();
    assert_ne!(quote_scale, original);
    assert_ne!(quote_scale, base_scale);
    assert_eq!(
        repo.insert_snapshot(&scope, &snap).await.unwrap(),
        quote_scale
    );
    assert!(
        repo.read_snapshot(&AccessScope::for_tenant(Uuid::now_v7()), tenant, original)
            .await
            .unwrap()
            .is_none()
    );
    let mut foreign = snap.clone();
    foreign.tenant_id = Uuid::now_v7();
    assert!(repo.insert_snapshot(&scope, &foreign).await.is_err());
}

#[tokio::test]
async fn runner_quote_and_snapshot_reads_share_transaction_and_rollback() {
    let (repo, db, tenant) = setup().await;
    let snap = snapshot(tenant);
    repo.upsert_rate(&reference(&snap)).await.unwrap();
    let r = repo.clone();
    let s = snap.clone();
    let aborted: Result<Uuid, TestError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                assert_eq!(
                    r.latest_rates_in(tx, &scope, tenant, "EUR", "JPY").await?[0].rate,
                    s.rate
                );
                let id = r.insert_snapshot_in(tx, &scope, &s).await?;
                assert_eq!(
                    r.read_snapshot_in(tx, &scope, tenant, id)
                        .await?
                        .unwrap()
                        .quote
                        .rate,
                    s.rate
                );
                Err(TestError::Repo(RepoError::Conflict(
                    "abort whole operation".into(),
                )))
            })
        })
        .await;
    assert!(matches!(
        aborted,
        Err(TestError::Repo(RepoError::Conflict(_)))
    ));
    let conn = db.conn().unwrap();
    assert!(
        fx_rate_snapshot::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant))
            .all(&conn)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn corrupt_reference_and_snapshot_evidence_fails_closed() {
    let (repo, db, tenant) = setup().await;
    let snap = snapshot(tenant);
    let scope = AccessScope::for_tenant(tenant);
    repo.upsert_rate(&reference(&snap)).await.unwrap();
    let conn = db.conn().unwrap();
    for invalid in [
        "1.2300",
        "not-a-rate",
        "0.000",
        "10000000000000000000000000000",
    ] {
        fx_rate::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(fx_rate::Column::Rate, Expr::value(invalid))
            .exec(&conn)
            .await
            .unwrap();
        assert!(matches!(
            repo.latest_rates(tenant, "EUR", "JPY").await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
    let id = repo.insert_snapshot(&scope, &snap).await.unwrap();
    fx_rate_snapshot::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(fx_rate_snapshot::Column::BaseCurrency, Expr::value("bad"))
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        repo.read_snapshot(&scope, tenant, id).await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    fx_rate_snapshot::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(fx_rate_snapshot::Column::BaseCurrency, Expr::value("EUR"))
        .col_expr(fx_rate_snapshot::Column::Rate, Expr::value("1.2300"))
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        repo.read_snapshot(&scope, tenant, id).await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[tokio::test]
async fn actual_unique_identity_collision_is_typed_and_rolls_back() {
    let (repo, db, tenant) = setup().await;
    let snap = snapshot(tenant);
    let scope = AccessScope::for_tenant(tenant);
    let id = repo.insert_snapshot(&scope, &snap).await.unwrap();
    let r = repo.clone();
    let s = snap.clone();
    let result: Result<(), TestError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                use sea_orm::IntoActiveModel;
                let scope = AccessScope::for_tenant(tenant);
                let mut corrected = s.clone();
                corrected.rate = parse_decimal("2.34").unwrap();
                r.insert_snapshot_in(tx, &scope, &corrected).await?;
                // Model the loser of a read-before-insert identity race: a different
                // generated rate_id, but the exact existing lock identity.
                let row = fx_rate_snapshot::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(Condition::all().add(fx_rate_snapshot::Column::RateId.eq(id)))
                    .one(tx)
                    .await
                    .unwrap()
                    .unwrap();
                let mut duplicate = row.into_active_model();
                duplicate.rate_id = Set(Uuid::now_v7());
                fx_rate_snapshot::Entity::insert(duplicate.clone())
                    .secure()
                    .scope_with_model(&scope, &duplicate)
                    .unwrap()
                    .exec(tx)
                    .await
                    .map_err(|e| insert_to_repo(e, db_backend()))?;
                Ok(())
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(TestError::Repo(RepoError::Conflict(_)))
    ));
    let conn = db.conn().unwrap();
    assert_eq!(
        fx_rate_snapshot::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&conn)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(repo.insert_snapshot(&scope, &snap).await.unwrap(), id);
}
fn db_backend() -> sea_orm::DbBackend {
    sea_orm::DbBackend::Sqlite
}

#[tokio::test]
async fn canonical_storage_and_historical_decoder_extremes() {
    let (repo, db, tenant) = setup().await;
    let mut snap = snapshot(tenant);
    snap.base_currency = spec("EUR", 28);
    snap.quote_currency = spec("JPY", 28);
    for text in [
        "0.0000000000000000000000000001",
        "9999999999999999999999999999",
    ] {
        snap.rate = parse_decimal(text).unwrap();
        let id = repo
            .insert_snapshot(&AccessScope::for_tenant(tenant), &snap)
            .await
            .unwrap();
        let conn = db.conn().unwrap();
        let row = fx_rate_snapshot::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant))
            .filter(Condition::all().add(fx_rate_snapshot::Column::RateId.eq(id)))
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.rate, text);
        assert_eq!(snapshot_row(row.clone()).unwrap().quote.rate, snap.rate);
        for scale in [-1, 29] {
            let mut bad = row.clone();
            bad.base_currency_scale = scale;
            assert!(matches!(
                snapshot_row(bad),
                Err(RepoError::InvalidStoredMoney(_))
            ));
            let mut bad = row.clone();
            bad.quote_currency_scale = scale;
            assert!(matches!(
                snapshot_row(bad),
                Err(RepoError::InvalidStoredMoney(_))
            ));
        }
        for text in ["0", "-1", "1e-2", "1.2300", "10000000000000000000000000000"] {
            let mut bad = row.clone();
            bad.rate = text.into();
            assert!(matches!(
                snapshot_row(bad),
                Err(RepoError::InvalidStoredMoney(_))
            ));
        }
    }
}

/// `insert_snapshot_in` itself maps a lost read-before-insert identity race to
/// the retryable `Conflict`: a SQLite trigger plays the winner, inserting the
/// same lock identity under another `rate_id` just before this insert lands.
#[tokio::test]
async fn insert_snapshot_in_maps_a_lost_identity_race_to_conflict() {
    use sea_orm::ConnectionTrait;
    let path = std::env::temp_dir().join(format!("fx-identity-race-{}.db", Uuid::now_v7()));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let db = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let raw = sea_orm::Database::connect(&dsn).await.unwrap();
    raw.execute_unprepared(
        "CREATE TRIGGER fx_identity_race_winner BEFORE INSERT ON ledger_fx_rate_snapshot \
         WHEN NEW.provider = 'race-provider' \
          AND NEW.rate_id <> '00000000-0000-7000-8000-000000000001' BEGIN \
         INSERT INTO ledger_fx_rate_snapshot (tenant_id, rate_id, base_currency, \
           base_currency_scale, quote_currency, quote_currency_scale, rate, as_of, provider, \
           stale, fallback_order, triangulated_via) \
         VALUES (NEW.tenant_id, '00000000-0000-7000-8000-000000000001', NEW.base_currency, \
           NEW.base_currency_scale, NEW.quote_currency, NEW.quote_currency_scale, NEW.rate, \
           NEW.as_of, NEW.provider, NEW.stale, NEW.fallback_order, NEW.triangulated_via); END",
    )
    .await
    .unwrap();
    raw.close().await.unwrap();
    let repo = FxRepo::new(DBProvider::new(db.clone()));
    let tenant = Uuid::now_v7();
    let mut snap = snapshot(tenant);
    snap.provider = "race-provider".into();
    let scope = AccessScope::for_tenant(tenant);
    assert!(matches!(
        repo.insert_snapshot(&scope, &snap).await,
        Err(RepoError::Conflict(_))
    ));
    // The aborted statement took the trigger's row with it.
    assert!(
        fx_rate_snapshot::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&db.conn().unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    drop(repo);
    drop(db);
    let _ = std::fs::remove_file(&path);
}
