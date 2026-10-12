//! `read_unallocated` before any settle (no pool row): the client answers a zero
//! balance at the currency's registry scale, and an unprovisioned currency is a
//! client 400 rather than a fabricated zero. SQLite-backed, so it runs without
//! Docker.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use bss_ledger_sdk::api::LedgerClientV1;
use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use sea_orm_migration::MigratorTrait;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::api::local_client::LedgerLocalClient;
use crate::domain::ports::metrics::NoopLedgerMetrics;
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::seller_guard::{SellerGuard, TenantTypeReader};

const SELLER_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.bss.ledger.seller.v1~");

/// Always-allow PDP that scopes every request to the subject's own tenant.
struct AllowAuthZ;

#[async_trait]
impl AuthZResolverApi for AllowAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant_id = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::nil);
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        toolkit_security::pep_properties::OWNER_TENANT_ID,
                        [tenant_id],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

struct SellerReader;

#[async_trait]
impl TenantTypeReader for SellerReader {
    async fn tenant_type(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<Option<String>, CanonicalError> {
        Ok(Some(SELLER_TYPE.to_owned()))
    }
}

async fn client() -> LedgerLocalClient {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    LedgerLocalClient::new(
        DBProvider::<DbError>::new(db),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(PolicyEnforcer::new(Arc::new(AllowAuthZ))),
        Arc::new(SellerGuard::new(
            Arc::new(SellerReader),
            [SELLER_TYPE.to_owned()],
        )),
        Arc::new(NoopLedgerMetrics),
        crate::config::FxConfig::default(),
        crate::config::PaymentsConfig::default(),
        crate::infra::period_close::CloseControlFeeds::inert(),
    )
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type(gts_id!("cf.core.security.subject_user.v1~"))
        .token_scopes(vec!["*".to_owned()])
        .build()
        .unwrap()
}

#[tokio::test]
async fn an_unsettled_payer_reads_zero_at_the_registry_scale() {
    let client = client().await;
    let tenant = Uuid::now_v7();
    let payer = Uuid::now_v7();
    let view = client
        .read_unallocated(&ctx(tenant), tenant, payer, "USD".to_owned())
        .await
        .unwrap();
    assert_eq!(view.payer_tenant_id, payer);
    assert_eq!(
        view.balance,
        PostedMoney::try_new(
            rust_decimal::Decimal::ZERO,
            CurrencySpec::try_new("USD".to_owned(), 2).unwrap()
        )
        .unwrap()
    );
    // A zero-decimal currency keeps its own registry scale.
    let jpy = client
        .read_unallocated(&ctx(tenant), tenant, payer, "JPY".to_owned())
        .await
        .unwrap();
    assert_eq!(jpy.balance.currency().scale(), 0);
    assert!(jpy.balance.amount().is_zero());
}

#[tokio::test]
async fn an_unprovisioned_currency_is_a_client_error_not_a_zero() {
    let client = client().await;
    let tenant = Uuid::now_v7();
    let err = client
        .read_unallocated(&ctx(tenant), tenant, Uuid::now_v7(), "ZZZZ".to_owned())
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 400);
}
