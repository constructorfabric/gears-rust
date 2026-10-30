//! The queued-apply chargeback sidecar delegates the in-txn dispute check to the
//! inline sidecar it wraps, so a queued outcome that lost a race is refused as an
//! invalid transition too. SQLite-backed.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{AccessScope, Db};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::QueuedChargebackApplySidecar;
use crate::domain::error::DomainError;
use crate::domain::payment::chargeback::{DisputePhase, DisputeVariant};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::payment::sidecar::{ChargebackDisputeOp, ChargebackSidecar};
use crate::infra::posting::retry::retry_transaction;
use crate::infra::posting::service::PostSidecar;
use crate::infra::storage::repo::{DisputeRepo, PaymentRepo};

fn usd(text: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("USD".into(), 2).unwrap(),
    )
    .unwrap()
}

async fn migrated_db() -> Db {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    db
}

/// The queued-apply sidecar of a `won` on dispute `dsp-1`, cycle 1.
fn queued_won(db: &Db, tenant: Uuid) -> QueuedChargebackApplySidecar {
    QueuedChargebackApplySidecar {
        inner: ChargebackSidecar {
            tenant,
            dispute_id: "dsp-1".into(),
            payment_id: "pay-1".into(),
            variant: DisputeVariant::CashHold,
            cycle: 1,
            disputed_amount: usd("10"),
            cash_hold: usd("10"),
            op: ChargebackDisputeOp::Advance {
                last_phase: DisputePhase::Won,
                clawed_back: usd("0"),
            },
            dispute: DisputeRepo::new(DBProvider::new(db.clone())),
            payment: PaymentRepo::new(DBProvider::new(db.clone())),
            publisher: Arc::new(LedgerEventPublisher::noop()),
            ctx: SecurityContext::anonymous(),
        },
        flow: "CHARGEBACK".into(),
        business_id: "dsp-1:1:WON".into(),
        tenant,
    }
}

/// Run the wrapper's `check_before_projection` in one transaction.
async fn check(
    db: &Db,
    sidecar: &Arc<QueuedChargebackApplySidecar>,
    scope: &AccessScope,
) -> Result<(), DomainError> {
    retry_transaction(db, |tx| {
        let sidecar = Arc::clone(sidecar);
        let scope = scope.clone();
        Box::pin(async move { Ok(sidecar.check_before_projection(tx, &scope).await) })
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn the_queued_outcome_sidecar_delegates_the_dispute_check() {
    let db = migrated_db().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let disputes = DisputeRepo::new(DBProvider::new(db.clone()));
    retry_transaction(&db, |tx| {
        let disputes = disputes.clone();
        let scope = scope.clone();
        Box::pin(async move {
            disputes
                .dispute_upsert(
                    tx,
                    &scope,
                    tenant,
                    "dsp-1",
                    "pay-1",
                    DisputeVariant::CashHold,
                    1,
                    &usd("10"),
                    &usd("10"),
                )
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let sidecar = Arc::new(queued_won(&db, tenant));
    assert!(
        check(&db, &sidecar, &scope).await.is_ok(),
        "a dispute still OPENED at the cycle passes the check"
    );

    // The other outcome resolves the dispute first.
    retry_transaction(&db, |tx| {
        let disputes = disputes.clone();
        let scope = scope.clone();
        Box::pin(async move {
            disputes
                .dispute_advance(
                    tx,
                    &scope,
                    tenant,
                    "dsp-1",
                    DisputePhase::Lost,
                    1,
                    &usd("10"),
                )
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    assert!(
        matches!(
            check(&db, &sidecar, &scope).await,
            Err(DomainError::InvalidDisputeTransition(_))
        ),
        "on a dispute already at LOST the wrapper answers the inner refusal"
    );
}
