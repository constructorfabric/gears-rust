//! Review regressions through real commercial providers.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use bss_pricing::{
    api::{pricing_acceptance::PricingAcceptanceProvider, sellability::SellabilityProvider},
    infra::commercial_terms::CommercialTermsService,
};
use std::sync::Arc;

struct PriceGrant {
    tenant: Uuid,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for PriceGrant {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use authz_resolver_sdk::{
            Constraint, EvaluationResponse, EvaluationResponseContext, InPredicate, Predicate,
        };
        let mut predicates = vec![Predicate::In(InPredicate::new(
            toolkit_security::pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        ))];
        if request.resource.resource_type == "gts.cf.bss.pricing.price.v1~" {
            predicates.push(Predicate::In(InPredicate::new(
                toolkit_security::pep_properties::RESOURCE_ID,
                vec![Uuid::from_u128(0xdead)],
            )));
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                deny_reason: None,
            },
        })
    }
}
fn scoped(f: &AcceptanceFixture) -> Arc<CommercialTermsService> {
    Arc::new(CommercialTermsService::new(
        f.fixture.state.clone(),
        Arc::new(authz_resolver_sdk::PolicyEnforcer::new(Arc::new(
            PriceGrant {
                tenant: f.ctx.subject_tenant_id(),
            },
        ))),
        f.clock.clone(),
        bss_pricing::config::SellerHoldPolicy::default(),
    ))
}
#[tokio::test]
async fn review_m1_other_price_grant_cannot_issue_receipt() {
    let f = AcceptanceFixture::new().await;
    let result = SellabilityProvider::new(scoped(&f))
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await;
    assert_eq!(result.unwrap_err().status_code(), 403);
    assert_eq!(f.counts().await, (0, 0, 0));
}
#[tokio::test]
async fn review_l1_unreadable_price_is_forbidden_for_both_fulfilment_doors() {
    let f = AcceptanceFixture::new().await;
    let receipt = f
        .sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap();
    let q = AcceptanceFixture::fulfilment_query(&receipt);
    let service = scoped(&f);
    assert_eq!(
        SellabilityProvider::new(service.clone())
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        PricingAcceptanceProvider::new(service)
            .hold(&f.ctx, q, meta("hold"))
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(f.counts().await, (1, 1, 1));
}
#[tokio::test]
async fn review_m2_empty_activation_window_stores_nothing() {
    let f = AcceptanceFixture::new().await;
    for hours in [48, 24] {
        let mut q = f.query.clone();
        q.start_at += time::Duration::hours(hours);
        let error = f
            .sellability
            .check(&f.ctx, q, f.meta.clone())
            .await
            .unwrap_err();
        assert_eq!(
            bss_pricing::infra::commercial_terms::errors::commercial_reason(&error).as_deref(),
            Some("ActivationOutsideAcceptedWindow"),
            "{error:?}"
        );
        assert_eq!(f.counts().await, (0, 0, 0));
    }
}
#[tokio::test]
async fn review_m2_start_inside_window_can_hold() {
    let f = AcceptanceFixture::new().await;
    let mut q = f.query.clone();
    q.start_at += time::Duration::hours(12);
    let receipt = f
        .sellability
        .check(&f.ctx, q, f.meta.clone())
        .await
        .unwrap();
    let held = f
        .acceptance
        .hold(
            &f.ctx,
            AcceptanceFixture::fulfilment_query(&receipt),
            meta("hold"),
        )
        .await
        .unwrap();
    assert_eq!(held.acceptance_id, receipt.acceptance_id);
}
#[tokio::test]
async fn review_l3_sdk_keys_match_rest_validation() {
    let f = AcceptanceFixture::new().await;
    for key in ["nul\0key".to_owned(), "x".repeat(4096)] {
        let error = f
            .sellability
            .check(&f.ctx, f.query.clone(), meta(&key))
            .await
            .unwrap_err();
        assert_eq!(error.status_code(), 400);
        assert!(matches!(
            error,
            toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
        ));
        assert_eq!(f.counts().await, (0, 0, 0));
    }
    let receipt = f
        .sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap();
    for key in ["nul\0key".to_owned(), "x".repeat(4096)] {
        let error = f
            .acceptance
            .hold(
                &f.ctx,
                AcceptanceFixture::fulfilment_query(&receipt),
                meta(&key),
            )
            .await
            .unwrap_err();
        assert_eq!(error.status_code(), 400);
        assert!(matches!(
            error,
            toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
        ));
        assert_eq!(f.counts().await, (1, 1, 1));
    }
}
#[tokio::test]
async fn review_l4_products_contention_is_unavailable() {
    let f = AcceptanceFixture::new().await;
    f.catalog.contended.store(true, Ordering::SeqCst);
    assert_eq!(
        f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap_err()
            .status_code(),
        503
    );
    assert_eq!(f.counts().await, (0, 0, 0));
    f.catalog.contended.store(false, Ordering::SeqCst);
    let receipt = f
        .sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap();
    f.catalog.contended.store(true, Ordering::SeqCst);
    let q = AcceptanceFixture::fulfilment_query(&receipt);
    assert_eq!(
        f.sellability
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .unwrap_err()
            .status_code(),
        503
    );
    assert_eq!(
        f.acceptance
            .hold(&f.ctx, q, meta("hold"))
            .await
            .unwrap_err()
            .status_code(),
        503
    );
    assert_eq!(f.counts().await, (1, 1, 1));
}

#[tokio::test]
async fn review_l2_policyless_fingerprint_matches_pre_seam() {
    use bss_pricing::infra::storage::repo::{plan_revision_repo, price_book_entry_repo};
    let f = AcceptanceFixture::new().await;
    let conn = f.fixture.db.conn().unwrap();
    let scope = crate::plan_support::scope(&f.fixture);
    let tenant = f.ctx.subject_tenant_id();
    let mut revision = plan_revision_repo::find(&conn, &scope, tenant, f.query.plan_revision_id)
        .await
        .unwrap()
        .unwrap();
    let mut items = crate::plan_support::items(&f.fixture, revision.id).await;
    let mut entry =
        price_book_entry_repo::find(&conn, &scope, tenant, items[0].price_book_entry_id.unwrap())
            .await
            .unwrap()
            .unwrap();
    revision.id = Uuid::from_u128(1);
    revision.book_id = Uuid::from_u128(2);
    revision.available_from = None;
    items[0].sku_id = Uuid::from_u128(3);
    items[0].price_book_entry_id = Some(Uuid::from_u128(4));
    entry.id = Uuid::from_u128(4);
    entry.usage_policy_id = None;
    entry.usage_policy_version = None;
    entry.usage_policy_digest = None;
    let after = bss_pricing::infra::plan_revisions::content(&revision, &items, &[entry]);
    let fingerprint = bss_approval::hash::snapshot_hash(
        &[bss_approval::ItemRef {
            item_type: "plan_revision".into(),
            item_id: revision.id,
            created_by: revision.created_by,
            before: None,
            after,
        }],
        None,
    );
    // Frozen by executing edb532550 content() on these rows, before applying the fix.
    assert_eq!(
        fingerprint,
        "8050fe8ab4ba2b9617f0f04ee304cf893221b2b34710d05be911dd1675ac8b94"
    );
}
