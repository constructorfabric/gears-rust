//! A rejected dispute transition names the dispute, the requested cycle and the
//! observed phase and cycle, so a stale or duplicate outcome can be diagnosed.
#![allow(clippy::unwrap_used)]

use super::*;
use crate::infra::posting::retry::retry_transaction;
use bss_ledger_sdk::{CurrencySpec, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::{ConnectOpts, connect_db};

fn eur(text: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("EUR".into(), 2).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn transition_rejections_name_the_dispute_and_both_cycles() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let repo = DisputeRepo::new(DBProvider::new(db.clone()));
    let tenant = Uuid::now_v7();
    let details = retry_transaction(&db, move |tx| {
        let repo = repo.clone();
        Box::pin(async move {
            let scope = AccessScope::for_tenant(tenant);
            let detail = |result: Result<(), RepoError>| match result {
                Err(RepoError::DisputeNotOpen(detail)) => detail,
                other => panic!("expected DisputeNotOpen, got {other:?}"),
            };
            let first = detail(
                repo.dispute_upsert(
                    tx,
                    &scope,
                    tenant,
                    "dsp-1",
                    "pay-1",
                    DisputeVariant::ArReclass,
                    2,
                    &eur("5"),
                    &eur("0"),
                )
                .await,
            );
            repo.dispute_upsert(
                tx,
                &scope,
                tenant,
                "dsp-1",
                "pay-1",
                DisputeVariant::ArReclass,
                1,
                &eur("5"),
                &eur("0"),
            )
            .await?;
            repo.dispute_advance(tx, &scope, tenant, "dsp-1", DisputePhase::Won, 1, &eur("5"))
                .await?;
            let stale = detail(
                repo.dispute_advance(
                    tx,
                    &scope,
                    tenant,
                    "dsp-1",
                    DisputePhase::Lost,
                    1,
                    &eur("5"),
                )
                .await,
            );
            let skipped = detail(
                repo.dispute_upsert(
                    tx,
                    &scope,
                    tenant,
                    "dsp-1",
                    "pay-1",
                    DisputeVariant::ArReclass,
                    3,
                    &eur("5"),
                    &eur("0"),
                )
                .await,
            );
            let missing = detail(
                repo.dispute_advance(tx, &scope, tenant, "dsp-9", DisputePhase::Won, 1, &eur("5"))
                    .await,
            );
            Ok::<_, crate::infra::posting::retry::AttemptError>((first, stale, skipped, missing))
        })
    })
    .await
    .unwrap();
    let (first, stale, skipped, missing) = details;
    assert!(
        first.contains("dsp-1") && first.contains("not 2"),
        "{first}"
    );
    assert!(
        stale.contains("dsp-1")
            && stale.contains("LOST")
            && stale.contains("observed WON at cycle 1"),
        "{stale}"
    );
    assert!(
        skipped.contains("cycle 3") && skipped.contains("observed WON at cycle 1"),
        "{skipped}"
    );
    assert!(missing.contains("dsp-9"), "{missing}");
}
