use bss_pricing_sdk::read::{CatalogRef, PlanQuery, PriceQuery, PricingReadV1, RevisionRef};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{FakePricingCatalog, PricingCall};

const TENANT: Uuid = Uuid::from_u128(0xA);

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(TENANT)
        .subject_type("bss-rating.system")
        .build()
        .unwrap()
}

fn plan(n: u128) -> PlanQuery {
    PlanQuery {
        catalog: CatalogRef { tenant_id: TENANT },
        plan_id: Uuid::from_u128(n),
    }
}

fn revision(plan: &PlanQuery, revision_no: i32) -> RevisionRef {
    RevisionRef {
        plan_id: plan.plan_id,
        revision_id: Uuid::from_u128(u128::from(revision_no.unsigned_abs())),
        revision_no,
    }
}

fn price(n: u128) -> PriceQuery {
    PriceQuery {
        catalog: CatalogRef { tenant_id: TENANT },
        price_id: Uuid::from_u128(n),
    }
}

#[tokio::test]
async fn the_latest_script_for_a_query_wins() {
    let fake = FakePricingCatalog::new();
    let query = plan(1);
    fake.on_current_revision(query.clone(), Ok(revision(&query, 1)));
    fake.on_current_revision(query.clone(), Ok(revision(&query, 2)));
    let answered = fake.current_revision(&caller(), query.clone()).await;
    assert_eq!(answered.unwrap(), revision(&query, 2));
}

#[tokio::test]
async fn a_scripted_refusal_is_answered_as_scripted() {
    let fake = FakePricingCatalog::new();
    let query = plan(1);
    fake.on_current_revision(
        query.clone(),
        Err(CanonicalError::service_unavailable().create()),
    );
    let refused = fake.current_revision(&caller(), query).await.unwrap_err();
    assert_eq!(refused.status_code(), 503);
}

#[tokio::test]
async fn an_unscripted_query_is_not_found_even_beside_a_scripted_one() {
    let fake = FakePricingCatalog::new();
    let scripted = plan(1);
    fake.on_current_revision(scripted.clone(), Ok(revision(&scripted, 1)));
    let unscripted = fake.current_revision(&caller(), plan(2)).await.unwrap_err();
    assert_eq!(unscripted.status_code(), 404);
    assert_eq!(
        fake.price(&caller(), price(1))
            .await
            .unwrap_err()
            .status_code(),
        404
    );
}

#[tokio::test]
async fn every_call_is_recorded_in_order_answered_or_not() {
    let fake = FakePricingCatalog::new();
    let first = plan(1);
    fake.on_current_revision(first.clone(), Ok(revision(&first, 1)));
    let ctx = caller();
    fake.current_revision(&ctx, first.clone()).await.unwrap();
    let _refused = fake.price(&ctx, price(7)).await;
    let _refused = fake.current_revision(&ctx, plan(2)).await;
    assert_eq!(
        fake.calls(),
        vec![
            PricingCall::CurrentRevision(first),
            PricingCall::Price(price(7)),
            PricingCall::CurrentRevision(plan(2)),
        ]
    );
}
