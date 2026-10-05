//! Provider boundary tests over the same durable repositories as 5a.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::{database, pool, restart_runtime, row};
use bss_pricing::{
    api::rest::authoring::AuthoringState,
    api::{pricing_acceptance::PricingAcceptanceProvider, sellability::SellabilityProvider},
    config::SellerHoldPolicy,
    infra::{
        clock::Clock,
        commercial_terms::{CommercialTermsService, wire},
        storage::repo::acceptance_repo,
    },
};
use bss_pricing_sdk::acceptance::{
    AcceptanceQuery, AcceptanceRef, CommandMeta, FulfilmentQuery, PricingAcceptanceV1,
    SellabilityV1,
};
use bss_pricing_sdk::read::CatalogRef;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

struct FixedClock {
    instant: parking_lot::Mutex<time::OffsetDateTime>,
    observed: parking_lot::Mutex<Vec<time::OffsetDateTime>>,
}
impl FixedClock {
    fn advance(&self, by: time::Duration) {
        *self.instant.lock() += by;
    }
}
impl Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        let now = *self.instant.lock();
        self.observed.lock().push(now);
        now
    }
}
struct Pdp {
    tenants: Vec<Uuid>,
    requests: parking_lot::Mutex<Vec<authz_resolver_sdk::EvaluationRequest>>,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Pdp {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
        use authz_resolver_sdk::*;
        self.requests.lock().push(request.clone());
        if request.subject.subject_type.as_deref() == Some("outage") {
            return Err(CanonicalError::service_unavailable()
                .with_detail("test PDP unavailable")
                .create());
        }
        Ok(EvaluationResponse {
            decision: request.subject.subject_type.as_deref() == Some(request.action.name.as_str()),
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.tenants.clone(),
                    ))],
                }],
                deny_reason: Some(DenyReason {
                    error_code: "COMMERCIAL_ACCESS_DENIED".into(),
                    details: None,
                }),
            },
        })
    }
}
fn ctx(kind: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(777))
        .subject_tenant_id(Uuid::from_u128(888))
        .subject_type(kind)
        .build()
        .unwrap()
}
async fn service(
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    pdp: Arc<Pdp>,
    clock: Arc<FixedClock>,
) -> Arc<CommercialTermsService> {
    Arc::new(CommercialTermsService::new(
        Arc::new(
            AuthoringState::new(db, Arc::new(toolkit::ClientHub::new()))
                .await
                .unwrap(),
        ),
        Arc::new(authz_resolver_sdk::PolicyEnforcer::new(pdp)),
        clock,
        SellerHoldPolicy::default(),
    ))
}
fn clock() -> Arc<FixedClock> {
    Arc::new(FixedClock {
        instant: parking_lot::Mutex::new(time::OffsetDateTime::UNIX_EPOCH),
        observed: parking_lot::Mutex::new(vec![]),
    })
}
fn pdp(tenants: Vec<Uuid>) -> Arc<Pdp> {
    Arc::new(Pdp {
        tenants,
        requests: parking_lot::Mutex::new(vec![]),
    })
}
fn query(tenant: Uuid, id: Uuid) -> AcceptanceQuery {
    AcceptanceQuery {
        catalog: CatalogRef { tenant_id: tenant },
        acceptance_id: id,
    }
}

#[test]
fn authorized_receipt_read_is_immutable_after_restart() {
    let (before, _dir, dsn, a) = restart_runtime().block_on(async {
        let (db, dir, dsn) = database().await;
        let a = row();
        acceptance_repo::insert(
            &db.conn().unwrap(),
            &AccessScope::for_tenant(a.tenant_id),
            a.clone(),
        )
        .await
        .unwrap();
        let provider =
            PricingAcceptanceProvider::new(service(db, pdp(vec![a.tenant_id]), clock()).await);
        let before = provider
            .acceptance(&ctx("read"), query(a.tenant_id, a.id))
            .await
            .unwrap();
        assert_eq!(wire::encode_acceptance(&before).unwrap(), a.receipt_json);
        (before, dir, dsn, a)
    });
    restart_runtime().block_on(async {
        let provider = PricingAcceptanceProvider::new(
            service(pool(&dsn).await, pdp(vec![a.tenant_id]), clock()).await,
        );
        let after = provider
            .acceptance(&ctx("read"), query(a.tenant_id, a.id))
            .await
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(wire::encode_acceptance(&after).unwrap(), a.receipt_json);
    });
}

#[tokio::test]
async fn receipt_lookup_is_restricted_to_the_explicit_catalog_even_with_two_tenant_grants() {
    let (db, _dir, _) = database().await;
    let a = row();
    let foreign = Uuid::new_v4();
    acceptance_repo::insert(
        &db.conn().unwrap(),
        &AccessScope::for_tenant(a.tenant_id),
        a.clone(),
    )
    .await
    .unwrap();
    let provider =
        PricingAcceptanceProvider::new(service(db, pdp(vec![a.tenant_id, foreign]), clock()).await);
    for q in [query(foreign, a.id), query(a.tenant_id, Uuid::new_v4())] {
        let e = provider.acceptance(&ctx("read"), q).await.unwrap_err();
        assert_eq!(e.status_code(), 404);
        assert!(e.to_string().contains("ReceiptNotFound"));
    }
    assert_eq!(
        provider
            .acceptance(&ctx("denied"), query(a.tenant_id, a.id))
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        provider
            .acceptance(&ctx("read"), query(Uuid::new_v4(), a.id))
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        provider
            .acceptance(&SecurityContext::anonymous(), query(a.tenant_id, a.id))
            .await
            .unwrap_err()
            .status_code(),
        401
    );
}

#[tokio::test]
async fn commercial_methods_authorize_each_action_and_bind_the_security_context_caller() {
    let (db, _dir, _) = database().await;
    let receipt = wire::decode_acceptance(&row().receipt_json).unwrap();
    let tenant = receipt.query.tenant_axes.seller_tenant_id;
    let pdp = pdp(vec![tenant]);
    let clock = clock();
    let service = service(db, pdp.clone(), clock.clone()).await;
    let hub = toolkit::ClientHub::new();
    hub.register::<dyn SellabilityV1>(Arc::new(SellabilityProvider::new(service.clone())));
    hub.register::<dyn PricingAcceptanceV1>(Arc::new(PricingAcceptanceProvider::new(service)));
    let sell = hub.get::<dyn SellabilityV1>().unwrap();
    let accept = hub.get::<dyn PricingAcceptanceV1>().unwrap();
    let fq = FulfilmentQuery {
        tenant_axes: receipt.query.tenant_axes.clone(),
        acceptance: AcceptanceRef {
            acceptance_id: receipt.acceptance_id,
            terms_digest: receipt.terms_digest,
        },
        current_market: receipt.query.market.clone(),
        activation_at: receipt.query.start_at,
    };
    // UUID-looking command data must never become the authenticated principal.
    let meta = CommandMeta {
        idempotency_key: Uuid::from_u128(999).to_string(),
    };
    for (kind, expected) in [("denied", 403), ("outage", 503)] {
        let e = sell
            .check(&ctx(kind), receipt.query.clone(), meta.clone())
            .await
            .unwrap_err();
        assert_eq!(e.status_code(), expected);
        let problem =
            serde_json::to_string(&toolkit_canonical_errors::Problem::from(e.clone())).unwrap();
        if expected == 503 {
            assert!(problem.contains("authorization unavailable"), "{problem}");
            assert!(!problem.contains("test PDP unavailable"), "{problem}");
        }
        if expected == 403 {
            assert!(problem.contains("COMMERCIAL_ACCESS_DENIED"), "{problem}");
        }
    }
    clock.advance(time::Duration::hours(25));
    for (kind, expected) in [("create", 403), ("read", 404), ("outage", 503)] {
        assert_eq!(
            sell.check_fulfilment(&ctx(kind), fq.clone())
                .await
                .unwrap_err()
                .status_code(),
            expected
        );
    }
    for (kind, expected) in [("read", 403), ("hold", 404), ("outage", 503)] {
        assert_eq!(
            accept
                .hold(&ctx(kind), fq.clone(), meta.clone())
                .await
                .unwrap_err()
                .status_code(),
            expected
        );
    }
    assert!(
        clock.observed.lock().is_empty(),
        "missing receipts never sample eligibility time"
    );
    for request in pdp.requests.lock().iter() {
        assert_eq!(request.subject.id, ctx("read").subject_id());
        assert_eq!(
            request.subject.properties["tenant_id"],
            serde_json::json!(ctx("read").subject_tenant_id())
        );
        assert_eq!(
            request.resource.properties[pep_properties::OWNER_TENANT_ID],
            serde_json::json!(tenant)
        );
        assert_eq!(
            request.resource.id,
            (request.action.name != "create").then_some(receipt.acceptance_id)
        );
        assert_eq!(
            request.resource.resource_type,
            "gts.cf.bss.pricing.acceptance.v1~"
        );
    }
}

#[test]
fn boundary_failures_keep_canonical_categories_and_concrete_metadata() {
    use bss_pricing::infra::commercial_terms::errors::UnconfiguredDependency;
    use bss_pricing_sdk::acceptance::CommercialReason;
    for (reason, status) in [
        (CommercialReason::UnsupportedModel, 400),
        (CommercialReason::AcceptanceMismatch, 409),
        (CommercialReason::PermissionDenied, 403),
        (CommercialReason::ReceiptNotFound, 404),
    ] {
        let e: CanonicalError = reason.into();
        assert_eq!(e.status_code(), status);
        let problem = serde_json::to_string(&toolkit_canonical_errors::Problem::from(e)).unwrap();
        assert!(problem.contains(reason.as_str()), "{problem}");
        assert!(problem.contains("pricing.acceptance.v1"));
    }
    let e: CanonicalError = UnconfiguredDependency {
        dependency: "UsageMeterSemanticsV1",
    }
    .into();
    let problem = serde_json::to_value(toolkit_canonical_errors::Problem::from(e)).unwrap();
    assert_eq!(
        problem["context"]["violations"][0]["type"], "UNCONFIGURED_DEPENDENCY",
        "{problem}"
    );
    assert_eq!(
        problem["context"]["violations"][0]["description"],
        "unconfigured dependency: UsageMeterSemanticsV1"
    );
    assert_eq!(
        problem["context"]["violations"][0]["subject"],
        "UsageMeterSemanticsV1"
    );
}
