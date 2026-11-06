//! Drives the wire fn `#[toolkit::consumes]` generates, rather than a copy of
//! its body, against gear views of one `ClientHub`.

use std::sync::Arc;

use api_contracts_sdk::contract::PaymentApi;
use cf_api_contracts::domain::local_client::PaymentLocalClient;
use cf_api_contracts::domain::service::PaymentDomainService;
use toolkit::ClientHub;
use toolkit::contract_support::wiring::ClientTuning;
use toolkit::discovery::{NullEndpointResolver, WireOutcome};
use toolkit_contract::policy::PolicyStack;

#[toolkit::consumes(contract = api_contracts_sdk::PaymentApi, from = "payment-provider")]
#[expect(dead_code, reason = "only the generated wire fn is used")]
struct PaymentConsumer;

fn wire(hub: &ClientHub) -> anyhow::Result<WireOutcome> {
    __toolkit_wire_payment_api_from_payment_provider(
        hub,
        Arc::new(NullEndpointResolver),
        ClientTuning::default(),
    )
}

#[test]
fn each_gear_view_gets_its_own_proxy() {
    let hub = ClientHub::new();
    let (a, b) = (hub.for_gear("a"), hub.for_gear("b"));

    assert_eq!(wire(&a).unwrap(), WireOutcome::Remote);
    assert_eq!(wire(&b).unwrap(), WireOutcome::Remote);

    let (a, b) = (
        a.get::<dyn PaymentApi>().unwrap(),
        b.get::<dyn PaymentApi>().unwrap(),
    );
    assert!(!Arc::ptr_eq(&a, &b));
    assert!(hub.try_get::<dyn PaymentApi>().is_none());
}

#[test]
fn wiring_one_gear_twice_is_an_error() {
    let hub = ClientHub::new();
    let a = hub.for_gear("orders");

    wire(&a).unwrap();
    let err = wire(&a).unwrap_err().to_string();

    assert!(err.contains("gear `orders`"), "{err}");
    assert!(err.contains("PaymentApi"), "{err}");
    assert!(err.contains("more than once"), "{err}");
}

#[test]
fn the_process_wide_hub_reuses_its_proxy() {
    let hub = ClientHub::new();

    assert_eq!(wire(&hub).unwrap(), WireOutcome::Remote);
    let first = hub.get::<dyn PaymentApi>().unwrap();
    assert_eq!(wire(&hub).unwrap(), WireOutcome::Remote);

    assert!(Arc::ptr_eq(&first, &hub.get::<dyn PaymentApi>().unwrap()));
}

#[test]
fn a_local_impl_wins_in_every_view() {
    let hub = ClientHub::new();
    hub.register::<dyn PaymentApi>(Arc::new(PaymentLocalClient::new(
        Arc::new(PaymentDomainService::new()),
        Arc::new(PolicyStack::new()),
    )));

    assert_eq!(wire(&hub.for_gear("a")).unwrap(), WireOutcome::Local);
    assert!(!hub.for_gear("a").has_remote_proxy::<dyn PaymentApi>());
}
