//! The D-194 commercial authority over the `AuthN` client-credentials exchange.
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use super::doubles::FakeResolver;
use crate::config::{CommercialIdentityConfig, GateConfig};
use crate::infra::gate::CatalogBinding;
use crate::infra::gate::authority::{
    AUTHORITY_EXCHANGE_DEADLINE, AuthnAuthority, AuthorityError, CommercialAuthority,
    DeadlineAuthority, NotProvisioned, commercial_authority,
};
use crate::infra::gate::budget::Budget;
use crate::infra::gate::readiness::AuthorityState;
use crate::infra::gate::signals::NoGateSignals;
use authn_resolver_sdk::AuthNResolverError;
use std::sync::Arc;
use uuid::Uuid;

const SELLER: Uuid = Uuid::from_u128(0x5e11);
const SERVICE: Uuid = Uuid::from_u128(0x0bd5);
const ORDERS_HOME: Uuid = Uuid::from_u128(0x40e);

fn identity(seller: Uuid) -> CommercialIdentityConfig {
    CommercialIdentityConfig {
        seller_tenant_id: seller,
        client_id: "orders-gate".to_owned(),
        client_secret: secrecy::SecretString::from("x"),
        scopes: vec!["pricing.read".to_owned()],
    }
}

#[tokio::test]
async fn service_context_is_the_exchanged_principal_of_the_configured_seller() {
    let resolver = Arc::new(FakeResolver::new(SERVICE, ORDERS_HOME));
    let authority = AuthnAuthority::new(resolver.clone(), &[identity(SELLER)]);
    let ctx = authority.service_context(SELLER).await.unwrap();
    assert_eq!(ctx.subject_id(), SERVICE);
    assert_eq!(ctx.subject_tenant_id(), ORDERS_HOME);
    assert_eq!(
        resolver.requests.lock().as_slice(),
        [("orders-gate".to_owned(), vec!["pricing.read".to_owned()])]
    );
}

#[tokio::test]
async fn an_unconfigured_seller_is_not_provisioned_and_never_exchanges() {
    let resolver = Arc::new(FakeResolver::new(SERVICE, ORDERS_HOME));
    let authority = AuthnAuthority::new(resolver.clone(), &[identity(SELLER)]);
    let other = Uuid::from_u128(0x0e);
    assert!(matches!(
        authority.service_context(other).await,
        Err(AuthorityError::NotProvisioned)
    ));
    assert_eq!(resolver.exchanges(), 0);
    assert!(matches!(
        NotProvisioned.service_context(SELLER).await,
        Err(AuthorityError::NotProvisioned)
    ));
}

#[tokio::test]
async fn empty_identities_bind_the_not_provisioned_adapter() {
    let resolver = Arc::new(FakeResolver::new(SERVICE, ORDERS_HOME));
    let authority = commercial_authority(Some(resolver.clone()), &[]).unwrap();
    assert!(matches!(
        authority.service_context(SELLER).await,
        Err(AuthorityError::NotProvisioned)
    ));
    assert_eq!(resolver.exchanges(), 0);
}

#[test]
fn configured_identities_without_a_resolver_refuse_startup() {
    let error = commercial_authority(None, &[identity(SELLER)])
        .err()
        .expect("a configured seller without a resolver refuses startup")
        .to_string();
    assert!(error.contains("gate.commercial_identities"), "{error}");
    assert!(error.contains("AuthNResolverClient"), "{error}");
    assert!(commercial_authority(None, &[]).is_ok());
}

#[tokio::test]
async fn exchange_failures_are_typed() {
    let mut resolver = FakeResolver::new(SERVICE, ORDERS_HOME);
    resolver.failure = Some(|| AuthNResolverError::TokenAcquisitionFailed("bad".to_owned()));
    let authority = AuthnAuthority::new(Arc::new(resolver), &[identity(SELLER)]);
    assert!(matches!(
        authority.service_context(SELLER).await,
        Err(AuthorityError::Exchange(
            AuthNResolverError::TokenAcquisitionFailed(_)
        ))
    ));
    let mut resolver = FakeResolver::new(SERVICE, ORDERS_HOME);
    resolver.failure = Some(|| AuthNResolverError::NoPluginAvailable);
    let authority = AuthnAuthority::new(Arc::new(resolver), &[identity(SELLER)]);
    assert!(matches!(
        authority.service_context(SELLER).await,
        Err(AuthorityError::Unavailable)
    ));
}

#[tokio::test(start_paused = true)]
async fn a_hung_exchange_times_out_at_the_configured_deadline() {
    let mut resolver = FakeResolver::new(SERVICE, ORDERS_HOME);
    resolver.stall = true;
    let inner = Arc::new(AuthnAuthority::new(Arc::new(resolver), &[identity(SELLER)]));
    let authority = DeadlineAuthority::new(inner, AUTHORITY_EXCHANGE_DEADLINE);
    let started = tokio::time::Instant::now();
    assert!(matches!(
        authority.service_context(SELLER).await,
        Err(AuthorityError::Deadline)
    ));
    assert_eq!(started.elapsed(), AUTHORITY_EXCHANGE_DEADLINE);
}

/// The wiring: the binding the gear hands to `SubmitService` and the catalog reads is bounded by
/// the budget's deadline (`gate.authority_exchange_deadline_ms`), whatever authority it wraps.
#[tokio::test(start_paused = true)]
async fn the_catalog_binding_bounds_its_authority_at_the_budget_deadline() {
    let config = GateConfig {
        authority_exchange_deadline_ms: Some(75),
        ..GateConfig::default()
    };
    let budget = Arc::new(Budget::from_config(&config, Arc::new(NoGateSignals)));
    let mut resolver = FakeResolver::new(SERVICE, ORDERS_HOME);
    resolver.stall = true;
    let inner = Arc::new(AuthnAuthority::new(Arc::new(resolver), &[identity(SELLER)]));
    let binding = CatalogBinding::new(inner, AuthorityState::Bound, budget);
    let started = tokio::time::Instant::now();
    assert!(matches!(
        binding.authority().service_context(SELLER).await,
        Err(AuthorityError::Deadline)
    ));
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(75));
}

#[tokio::test]
async fn every_call_exchanges_and_no_context_is_cached() {
    let resolver = Arc::new(FakeResolver::new(SERVICE, ORDERS_HOME));
    let authority = AuthnAuthority::new(resolver.clone(), &[identity(SELLER)]);
    authority.service_context(SELLER).await.unwrap();
    authority.service_context(SELLER).await.unwrap();
    assert_eq!(resolver.exchanges(), 2);
}

#[test]
fn debug_never_prints_the_secret() {
    let resolver = Arc::new(FakeResolver::new(SERVICE, ORDERS_HOME));
    let mut id = identity(SELLER);
    id.client_secret = secrecy::SecretString::from("hunter2");
    let text = format!("{:?}", AuthnAuthority::new(resolver, &[id]));
    assert!(!text.contains("hunter2"), "{text}");
}
