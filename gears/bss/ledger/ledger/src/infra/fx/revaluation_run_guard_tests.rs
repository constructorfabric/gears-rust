//! `run_payer` metadata guards that fire before any rate lookup or posting:
//! grains that share a functional code but not its scale, and a same-currency
//! grain whose stored scale differs from the functional scale.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use sea_orm_migration::MigratorTrait;
use toolkit_db::{ConnectOpts, connect_db};

async fn run() -> UnrealizedRevaluationRun {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    UnrealizedRevaluationRun::new(
        DBProvider::new(db),
        Arc::new(LedgerEventPublisher::noop()),
        FxConfig::default(),
    )
}

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn grain(payer: Uuid, balance: PostedMoney, functional: PostedMoney) -> RevaluationGrain {
    RevaluationGrain {
        tenant_id: Uuid::from_u128(1),
        payer_tenant_id: payer,
        account_id: Uuid::now_v7(),
        invoice_id: Some("inv-1".to_owned()),
        credit_grant_event_type: None,
        version: 1,
        balance,
        functional_balance: functional,
    }
}

async fn run_payer(
    service: &UnrealizedRevaluationRun,
    grains: &[RevaluationGrain],
) -> Result<Option<usize>, DomainError> {
    let tenant = Uuid::from_u128(1);
    let mut rates = BTreeMap::new();
    service
        .run_payer(
            &SecurityContext::anonymous(),
            &AccessScope::for_tenant(tenant),
            tenant,
            "202610",
            RevaluationScope::Ar,
            grains[0].payer_tenant_id,
            grains,
            OffsetDateTime::now_utc(),
            &ChartIndex::from_rows(std::iter::empty()),
            &mut rates,
        )
        .await
}

#[tokio::test]
async fn grains_sharing_a_functional_code_at_different_scales_are_rejected() {
    let service = run().await;
    let payer = Uuid::now_v7();
    let grains = [
        grain(payer, money("10", "USD", 2), money("9", "EUR", 2)),
        grain(payer, money("20", "USD", 2), money("18", "EUR", 3)),
    ];
    match run_payer(&service, &grains).await {
        Err(DomainError::Internal(detail)) => {
            assert!(detail.contains("currencies or scales"), "{detail}");
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_same_currency_grain_at_another_scale_is_an_inconsistent_scale() {
    let service = run().await;
    let payer = Uuid::now_v7();
    // Transaction EUR at scale 3, functional EUR at scale 2: the identity pair
    // must not copy the balance across scales.
    let grains = [grain(
        payer,
        money("10.125", "EUR", 3),
        money("10.13", "EUR", 2),
    )];
    assert!(matches!(
        run_payer(&service, &grains).await,
        Err(DomainError::InconsistentScale(_))
    ));
}
