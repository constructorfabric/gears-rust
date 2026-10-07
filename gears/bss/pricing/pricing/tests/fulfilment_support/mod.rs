//! Detached Products observation probes, never a substitute commercial implementation.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::{
    acceptance_support::{AcceptanceFixture, FixedClock},
    plan_support::Catalog,
};
use bss_products_sdk::{
    PricingReferenceRegistry, ReferenceRegistryV1,
    models::{ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuVersion},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;
pub struct Probe {
    catalog: Arc<Catalog>,
    dsn: String,
    clock: Arc<FixedClock>,
    sql: Option<String>,
    advance: Option<time::Duration>,
    repeat: bool,
    pub calls: AtomicUsize,
}
impl Probe {
    pub fn install(
        f: &AcceptanceFixture,
        sql: Option<&str>,
        advance: Option<time::Duration>,
        repeat: bool,
    ) -> Arc<Self> {
        let probe = Arc::new(Self {
            catalog: f.catalog.clone(),
            dsn: f.fixture.dsn.to_string(),
            clock: f.clock.clone(),
            sql: sql.map(str::to_owned),
            advance,
            repeat,
            calls: AtomicUsize::new(0),
        });
        f.fixture
            .state
            .hub
            .register::<PricingReferenceRegistry>(Arc::new(PricingReferenceRegistry(
                probe.clone(),
            )));
        probe
    }
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for Probe {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku: Uuid,
        kind: ReferenceKind,
        id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.catalog.reserve(ctx, tenant, sku, kind, id).await
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.catalog.confirm(ctx, tenant, id).await
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.catalog.release(ctx, tenant, id).await
    }
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        self.catalog.states(ctx, tenant, ids).await
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.catalog.sku_version_as_of(ctx, tenant, id, date).await
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 || self.repeat {
            if let Some(sql) = &self.sql {
                use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
                Database::connect(&self.dsn)
                    .await
                    .unwrap()
                    .execute_raw(Statement::from_string(DbBackend::Sqlite, sql))
                    .await
                    .unwrap();
            }
            if let Some(by) = self.advance {
                self.clock.advance(by);
            }
        }
        let mut sku = self.catalog.sku_for_write(ctx, tenant, id).await?;
        sku.sellable = false;
        sku.name = "Today's off-sale SKU".into();
        sku.unit = Some("changed-unit".into());
        Ok(sku)
    }
}

/// A grant limited to a single acceptance, even when the PDP's boolean answer is allow.
pub struct ReceiptGrant {
    pub acceptance_id: parking_lot::Mutex<Uuid>,
    pub tenant: Uuid,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for ReceiptGrant {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
        use authz_resolver_sdk::{
            Constraint, EvaluationResponse, EvaluationResponseContext, InPredicate, Predicate,
        };
        let id = if request.resource.resource_type == "gts.cf.bss.pricing.acceptance.v1~" {
            *self.acceptance_id.lock()
        } else {
            request.resource.id.unwrap()
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![
                        Predicate::In(InPredicate::new(
                            toolkit_security::pep_properties::OWNER_TENANT_ID,
                            vec![self.tenant],
                        )),
                        Predicate::In(InPredicate::new(
                            toolkit_security::pep_properties::RESOURCE_ID,
                            vec![id],
                        )),
                    ],
                }],
                deny_reason: None,
            },
        })
    }
}
