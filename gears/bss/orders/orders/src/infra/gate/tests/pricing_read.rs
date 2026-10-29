//! The `CatalogReads` adapter over hand-written Pricing/Products doubles (D-187, D-194).
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use super::doubles::{FakePricing, FakeProducts, FakeResolver, PlanResource, bound_cell, date};
use crate::config::{CommercialIdentityConfig, GateConfig};
use crate::domain::gate::ports::{GatePort, PortAnswer, Unavailable};
use crate::domain::gate::pricing_map::{LineCatalogRequest, ReadFailure, SelectedItem};
use crate::infra::gate::GatePorts;
use crate::infra::gate::authority::{
    AUTHORITY_EXCHANGE_DEADLINE, AuthnAuthority, CommercialAuthority, DeadlineAuthority,
    NotProvisioned, WarnOnce,
};
use crate::infra::gate::budget::{Budget, RunBudget};
use crate::infra::gate::pricing_read::{
    CatalogReads, CatalogReadsUnavailable, PricingCatalogReads,
};
use crate::infra::gate::signals::NoGateSignals;
use authn_resolver_sdk::AuthNResolverError;
use bss_pricing_sdk::read::RevisionRef;
use std::sync::Arc;
use std::time::Duration;
use time::Month;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const SELLER: Uuid = Uuid::from_u128(0x5e11);
const SERVICE: Uuid = Uuid::from_u128(0x0bd5);
const PLAN: Uuid = Uuid::from_u128(1);
const REVISION: Uuid = Uuid::from_u128(2);
const ITEM_A: Uuid = Uuid::from_u128(3);
const ITEM_B: Uuid = Uuid::from_u128(4);
const SKU_1: Uuid = Uuid::from_u128(10);
const SKU_2: Uuid = Uuid::from_u128(11);

fn current() -> RevisionRef {
    RevisionRef {
        plan_id: PLAN,
        revision_id: REVISION,
        revision_no: 1,
    }
}

fn item(item_id: Uuid, dim: Option<&str>) -> SelectedItem {
    SelectedItem {
        item_id,
        quantity: "1".to_owned(),
        selected_dim_value: dim.map(str::to_owned),
    }
}

/// Three selected slots bound to two distinct SKUs.
fn request() -> LineCatalogRequest {
    LineCatalogRequest {
        line_id: Uuid::from_u128(9),
        plan_id: PLAN,
        revision_id: REVISION,
        items: vec![
            item(ITEM_A, None),
            item(ITEM_A, Some("eu")),
            item(ITEM_B, None),
        ],
    }
}

fn pricing() -> FakePricing {
    FakePricing::new(
        current(),
        vec![
            bound_cell(ITEM_A, None, SKU_1),
            bound_cell(ITEM_A, Some("eu"), SKU_1),
            bound_cell(ITEM_B, None, SKU_2),
        ],
    )
}

fn authority() -> Arc<dyn CommercialAuthority> {
    authority_over(FakeResolver::new(SERVICE, SELLER))
}

fn authority_over(resolver: FakeResolver) -> Arc<dyn CommercialAuthority> {
    let identity = CommercialIdentityConfig {
        seller_tenant_id: SELLER,
        client_id: "orders-gate".to_owned(),
        client_secret: secrecy::SecretString::from("x"),
        scopes: Vec::new(),
    };
    // The principal's home tenant is not the seller: the catalog must still be the seller's.
    // Production wraps the authority once in `CatalogBinding::new`; the double does the same.
    Arc::new(DeadlineAuthority::new(
        Arc::new(AuthnAuthority::new(Arc::new(resolver), &[identity])),
        AUTHORITY_EXCHANGE_DEADLINE,
    ))
}

fn reads(
    pricing: &Arc<FakePricing>,
    products: &Arc<FakeProducts>,
    authority: Arc<dyn CommercialAuthority>,
) -> PricingCatalogReads {
    PricingCatalogReads::new(
        pricing.clone(),
        products.clone(),
        authority,
        Arc::new(Budget::from_config(
            &GateConfig::default(),
            Arc::new(NoGateSignals),
        )),
    )
}

async fn read(reads: &PricingCatalogReads) -> crate::domain::gate::pricing_map::CatalogAnswer {
    reads
        .read_line(
            SELLER,
            &request(),
            date(2026, Month::October, 22),
            &RunBudget::new(),
            &CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn not_provisioned_is_missing_everywhere_and_never_calls_pricing() {
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    let answer = read(&reads(&pricing, &products, Arc::new(NotProvisioned))).await;
    let missing = ReadFailure::Unavailable(Unavailable::Missing);
    assert_eq!(answer.current, Err(missing.clone()));
    assert_eq!(answer.resolved.as_ref().err(), Some(&missing));
    assert!(answer.skus.is_empty());
    assert_eq!(pricing.calls(), 0);
    assert_eq!(products.calls(), 0);
}

#[tokio::test]
async fn pep_denial_of_current_revision_is_unavailable_denied() {
    let mut fake = pricing();
    fake.deny_current = true;
    let (pricing, products) = (Arc::new(fake), Arc::new(FakeProducts::default()));
    let answer = read(&reads(&pricing, &products, authority())).await;
    assert_eq!(
        answer.current,
        Err(ReadFailure::Unavailable(Unavailable::Denied))
    );
    assert!(answer.resolved.is_ok());
}

#[tokio::test(start_paused = true)]
async fn a_stalled_current_revision_is_unavailable_deadline() {
    let mut fake = pricing();
    fake.stall_current = true;
    let (pricing, products) = (Arc::new(fake), Arc::new(FakeProducts::default()));
    let answer = read(&reads(&pricing, &products, authority())).await;
    assert_eq!(
        answer.current,
        Err(ReadFailure::Unavailable(Unavailable::Deadline))
    );
}

#[tokio::test]
async fn three_bound_slots_with_two_skus_read_each_sku_once() {
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    let answer = read(&reads(&pricing, &products, authority())).await;
    assert_eq!(answer.current, Ok(current()));
    assert_eq!(products.calls(), 2);
    assert_eq!(
        answer.skus.keys().copied().collect::<Vec<_>>(),
        [SKU_1, SKU_2]
    );
    assert!(
        answer
            .skus
            .values()
            .all(|a| matches!(a, PortAnswer::Answered(_)))
    );
}

#[tokio::test]
async fn every_query_names_the_seller_catalog_never_the_principal_tenant() {
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    read(&reads(&pricing, &products, authority())).await;
    let catalogs = pricing.catalogs.lock().clone();
    assert_eq!(catalogs, [SELLER, SELLER]);
    let tenants = products.tenants.lock().clone();
    assert_eq!(tenants, [SELLER, SELLER]);
}

#[tokio::test]
async fn the_unavailable_adapters_answer_missing() {
    let ports = GatePorts::unavailable();
    let answer = ports
        .catalog
        .read_line(
            SELLER,
            &request(),
            date(2026, Month::October, 22),
            &RunBudget::new(),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(
        answer.current,
        Err(ReadFailure::Unavailable(Unavailable::Missing))
    );
    let outage = CatalogReadsUnavailable(Unavailable::Outage)
        .read_line(
            SELLER,
            &request(),
            date(2026, Month::October, 22),
            &RunBudget::new(),
            &CancellationToken::new(),
        )
        .await;
    assert!(outage.skus.is_empty());
    let ctx = toolkit_security::SecurityContext::anonymous();
    let query = crate::domain::gate::answers::TaxQuery {
        payer_tenant_id: Uuid::from_u128(0xb),
        seller_tenant_id: SELLER,
        lines: Vec::new(),
    };
    let tax = ports
        .indicative_tax
        .estimate(&ctx, &query, &RunBudget::new(), &CancellationToken::new())
        .await;
    assert_eq!(tax, PortAnswer::Unavailable(Unavailable::Missing));
}

fn outage() -> ReadFailure {
    ReadFailure::Unavailable(Unavailable::Outage)
}

#[tokio::test]
async fn a_failed_exchange_is_an_outage_everywhere_and_never_calls_pricing() {
    let mut resolver = FakeResolver::new(SERVICE, SELLER);
    resolver.failure = Some(|| AuthNResolverError::Unauthorized("no".to_owned()));
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    let answer = read(&reads(&pricing, &products, authority_over(resolver))).await;
    assert_eq!(answer.current, Err(outage()));
    assert_eq!(answer.resolved.as_ref().err(), Some(&outage()));
    assert!(answer.skus.is_empty());
    assert_eq!(pricing.calls(), 0);
    assert_eq!(products.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_hung_exchange_is_unavailable_deadline_at_the_authority_deadline() {
    let mut resolver = FakeResolver::new(SERVICE, SELLER);
    resolver.stall = true;
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    let reads = reads(&pricing, &products, authority_over(resolver));
    let started = tokio::time::Instant::now();
    let answer = read(&reads).await;
    assert_eq!(started.elapsed(), AUTHORITY_EXCHANGE_DEADLINE);
    assert_eq!(
        answer.current,
        Err(ReadFailure::Unavailable(Unavailable::Deadline))
    );
    assert_eq!(
        answer.resolved.as_ref().err(),
        Some(&ReadFailure::Unavailable(Unavailable::Deadline))
    );
    assert_eq!(pricing.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_the_exchange_is_an_outage() {
    let mut resolver = FakeResolver::new(SERVICE, SELLER);
    resolver.stall = true;
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    let reads = reads(&pricing, &products, authority_over(resolver));
    let cancel = CancellationToken::new();
    let (req, run) = (request(), RunBudget::new());
    let started = tokio::time::Instant::now();
    let (answer, ()) = tokio::join!(
        reads.read_line(SELLER, &req, date(2026, Month::October, 22), &run, &cancel,),
        async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel.cancel();
        }
    );
    assert_eq!(started.elapsed(), Duration::from_millis(10));
    assert_eq!(answer.current, Err(outage()));
    assert_eq!(answer.resolved.as_ref().err(), Some(&outage()));
    assert_eq!(pricing.calls(), 0);
}

#[tokio::test]
async fn a_failed_resolve_reads_no_sku() {
    let mut fake = pricing();
    fake.resolve_error = Some(|| {
        PlanResource::permission_denied()
            .with_reason("denied")
            .create()
    });
    let (pricing, products) = (Arc::new(fake), Arc::new(FakeProducts::default()));
    let answer = read(&reads(&pricing, &products, authority())).await;
    assert!(answer.resolved.is_err());
    assert!(answer.skus.is_empty());
    assert_eq!(products.calls(), 0);
}

#[tokio::test]
async fn a_failed_sku_read_is_unavailable_for_that_sku_only() {
    let products = Arc::new(FakeProducts {
        fail_sku: Some(SKU_2),
        ..FakeProducts::default()
    });
    let pricing = Arc::new(pricing());
    let answer = read(&reads(&pricing, &products, authority())).await;
    assert!(matches!(
        answer.skus.get(&SKU_2),
        Some(PortAnswer::Unavailable(_))
    ));
    assert!(matches!(
        answer.skus.get(&SKU_1),
        Some(PortAnswer::Answered(_))
    ));
}

#[tokio::test]
async fn plan_unpublished_from_current_revision_is_the_typed_failure() {
    let mut fake = pricing();
    fake.current_error = Some(|| {
        PlanResource::aborted("PLAN_UNPUBLISHED")
            .with_reason("PLAN_UNPUBLISHED")
            .create()
    });
    let (pricing, products) = (Arc::new(fake), Arc::new(FakeProducts::default()));
    let answer = read(&reads(&pricing, &products, authority())).await;
    assert_eq!(answer.current, Err(ReadFailure::PlanUnpublished));
}

#[test]
fn the_warned_set_stops_growing_at_its_cap() {
    let warned = WarnOnce::default();
    assert!(warned.first(SELLER));
    assert!(!warned.first(SELLER));
    for n in 0..10_000u128 {
        warned.first(Uuid::from_u128(0x1_0000 + n));
    }
    assert!(!warned.first(Uuid::from_u128(0xffff_ffff)));
}

/// The authority over a resolver the test keeps, to count the exchanges.
fn counted_authority() -> (Arc<FakeResolver>, Arc<dyn CommercialAuthority>) {
    let resolver = Arc::new(FakeResolver::new(SERVICE, SELLER));
    let identity = CommercialIdentityConfig {
        seller_tenant_id: SELLER,
        client_id: "orders-gate".to_owned(),
        client_secret: secrecy::SecretString::from("x"),
        scopes: Vec::new(),
    };
    let authority = Arc::new(AuthnAuthority::new(resolver.clone(), &[identity]));
    (resolver, authority)
}

fn line(n: u128, revision_id: Uuid) -> LineCatalogRequest {
    LineCatalogRequest {
        line_id: Uuid::from_u128(1000 + n),
        revision_id,
        ..request()
    }
}

#[tokio::test]
async fn a_basket_on_one_revision_shares_one_exchange_current_and_resolve() {
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    let (resolver, authority) = counted_authority();
    let reads = reads(&pricing, &products, authority);
    let lines: Vec<_> = (0..3).map(|n| line(n, REVISION)).collect();
    let answers = reads
        .read_lines(
            SELLER,
            &lines,
            date(2026, Month::October, 22),
            &RunBudget::new(),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(resolver.exchanges(), 1);
    assert_eq!(pricing.currents(), 1);
    assert_eq!(pricing.resolves(), 1);
    // Two distinct SKUs across the basket, each read once.
    assert_eq!(products.calls(), 2);
    assert_eq!(answers.len(), 3);
    for l in &lines {
        let a = &answers[&l.line_id];
        assert_eq!(a.current, Ok(current()));
        assert!(a.resolved.is_ok());
        assert_eq!(a.skus.keys().copied().collect::<Vec<_>>(), [SKU_1, SKU_2]);
    }
}

#[tokio::test]
async fn two_hundred_lines_on_two_revisions_resolve_twice_under_the_caps() {
    const REVISION_2: Uuid = Uuid::from_u128(22);
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    let (resolver, authority) = counted_authority();
    let reads = reads(&pricing, &products, authority);
    let lines: Vec<_> = (0..200)
        .map(|n| line(n, if n % 2 == 0 { REVISION } else { REVISION_2 }))
        .collect();
    let run = RunBudget::new();
    let answers = reads
        .read_lines(
            SELLER,
            &lines,
            date(2026, Month::October, 22),
            &run,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(resolver.exchanges(), 1);
    assert_eq!(pricing.currents(), 1);
    assert_eq!(pricing.resolves(), 2);
    assert_eq!(run.subrequests(GatePort::Resolve), 2);
    assert_eq!(run.subrequests(GatePort::CurrentRevision), 1);
    assert_eq!(run.subrequests(GatePort::SkuRead), 2);
    assert_eq!(answers.len(), 200);
    for l in &lines {
        let a = &answers[&l.line_id];
        assert!(a.current.is_ok());
        let rb = a.resolved.as_ref().unwrap();
        assert_eq!(rb.revision_id, l.revision_id);
        assert!(
            a.skus
                .values()
                .all(|s| matches!(s, PortAnswer::Answered(_)))
        );
    }
}

/// Frozen run dates survive asynchronous reads; only a new run uses the next UTC date.
#[cfg(feature = "gate-dev-stubs")]
#[tokio::test(start_paused = true)]
async fn a_catalog_read_crossing_utc_midnight_keeps_the_run_resolve_date() {
    use super::doubles::Fault;
    let (pricing, products) = (Arc::new(pricing()), Arc::new(FakeProducts::default()));
    pricing.fail_with(Some(Fault::Delay(Duration::from_millis(20))));
    let reads = reads(&pricing, &products, authority());
    let midnight = date(2026, Month::October, 23)
        .with_hms(0, 0, 0)
        .unwrap()
        .assume_utc();
    let started_utc = midnight - time::Duration::milliseconds(10);
    let started = tokio::time::Instant::now();
    let answer = reads
        .read_line(
            SELLER,
            &request(),
            started_utc.date(),
            &RunBudget::new(),
            &CancellationToken::new(),
        )
        .await;
    assert!(answer.resolved.is_ok());
    let completed_utc = started_utc + time::Duration::try_from(started.elapsed()).unwrap();
    assert_eq!(completed_utc.date(), midnight.date());
    assert_eq!(*pricing.resolve_dates.lock(), [started_utc.date()]);
    pricing.fail_with(None);
    let answer = reads
        .read_line(
            SELLER,
            &request(),
            completed_utc.date(),
            &RunBudget::new(),
            &CancellationToken::new(),
        )
        .await;
    assert!(answer.resolved.is_ok());
    assert_eq!(
        *pricing.resolve_dates.lock(),
        [started_utc.date(), midnight.date()]
    );
}
