use std::time::Duration;

use serde_json::{Value, json};
use toolkit_contract::wiring::ClientWiring;

use super::{consumer_wiring, provided_wiring};
use crate::config::ConfigProvider;

struct Gear(Value);

impl ConfigProvider for Gear {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        (gear_name == "orders").then_some(&self.0)
    }
}

fn clients(section: &Value) -> Gear {
    Gear(json!({ "config": {}, "clients": section }))
}

fn error(result: anyhow::Result<impl std::fmt::Debug>) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn no_section_keeps_discovery_and_local() {
    let gear = Gear(json!({ "config": { "anything": 1 } }));
    assert!(
        consumer_wiring(&gear, "orders", "billing")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        provided_wiring(&gear, "orders", "payment_api").unwrap(),
        ClientWiring::Local
    ));
}

#[test]
fn a_consumes_entry_is_read() {
    let gear = clients(&json!({
        "consumes": { "billing": { "timeout": "2s", "endpoint": "http://billing:8080" } }
    }));
    let wiring = consumer_wiring(&gear, "orders", "billing")
        .unwrap()
        .unwrap();
    assert_eq!(wiring.endpoint.as_deref(), Some("http://billing:8080"));
    assert_eq!(wiring.tuning.timeout, Some(Duration::from_secs(2)));
    assert!(
        consumer_wiring(&gear, "orders", "shipping")
            .unwrap()
            .is_none()
    );
}

#[test]
fn defaults_apply_to_every_entry_and_the_entry_wins() {
    let gear = clients(&json!({
        "defaults": {
            "timeout": "5s",
            "max_concurrent_requests": 64,
            "circuit_breaker": { "open_for": "30s", "minimum_calls": 20 },
        },
        "consumes": {
            "billing": { "timeout": "2s", "circuit_breaker": { "open_for": "1m" } },
        },
    }));

    let billing = consumer_wiring(&gear, "orders", "billing")
        .unwrap()
        .unwrap();
    assert_eq!(billing.tuning.timeout, Some(Duration::from_secs(2)));
    assert_eq!(billing.tuning.max_concurrent_requests, Some(64));
    let breaker = billing.tuning.circuit_breaker.unwrap();
    assert_eq!(breaker.open_for(), Duration::from_secs(60));
    assert_eq!(
        breaker.minimum_calls(),
        20,
        "nested keys merge, not replace"
    );

    // A dep with no entry of its own still gets the defaults.
    let shipping = consumer_wiring(&gear, "orders", "shipping")
        .unwrap()
        .unwrap();
    assert_eq!(shipping.tuning.timeout, Some(Duration::from_secs(5)));
    assert!(shipping.endpoint.is_none());
}

#[test]
fn a_provides_entry_gets_the_defaults_but_defaults_alone_stay_local() {
    let gear = clients(&json!({
        "defaults": { "timeout": "5s" },
        "provides": { "payment_api": { "transport": "rest", "endpoint": "http://payments:8080" } },
    }));
    let ClientWiring::Rest { endpoint, tuning } =
        provided_wiring(&gear, "orders", "payment_api").unwrap()
    else {
        panic!("expected the REST arm");
    };
    assert_eq!(endpoint, "http://payments:8080");
    assert_eq!(tuning.timeout, Some(Duration::from_secs(5)));

    assert!(matches!(
        provided_wiring(&gear, "orders", "other_api").unwrap(),
        ClientWiring::Local
    ));
}

#[test]
fn unknown_keys_are_rejected_with_their_path() {
    let cases = [
        (
            json!({ "consume": {} }),
            "gears.orders.clients.consume: unknown key",
        ),
        (
            json!({ "defaults": { "timout": "1s" } }),
            "gears.orders.clients.defaults.timout: unknown key",
        ),
        (
            json!({ "consumes": { "billing": { "timout": "1s" } } }),
            "gears.orders.clients.consumes.billing.timout: unknown key",
        ),
    ];
    for (section, expected) in cases {
        let err = error(consumer_wiring(&clients(&section), "orders", "billing"));
        assert!(err.contains(expected), "{err}");
    }

    let err = error(provided_wiring(
        &clients(
            &json!({ "provides": { "payment_api": { "transport": "rest", "endpoint": "http://p:1", "grpc_endpoint": "http://p:2" } } }),
        ),
        "orders",
        "payment_api",
    ));
    assert!(
        err.contains("gears.orders.clients.provides.payment_api.grpc_endpoint: unknown key"),
        "{err}"
    );
}

#[test]
fn nested_settings_reject_unknown_keys() {
    let gear = clients(&json!({
        "consumes": { "billing": { "retry": { "max_attempt": 3 } } }
    }));
    let err = error(consumer_wiring(&gear, "orders", "billing"));
    assert!(
        err.starts_with("gears.orders.clients.consumes.billing:"),
        "{err}"
    );
    assert!(err.contains("max_attempt"), "{err}");
}

#[test]
fn an_endpoint_or_transport_in_defaults_is_rejected() {
    for key in ["endpoint", "grpc_endpoint", "transport"] {
        let gear = clients(&json!({ "defaults": { key: "http://x:1" } }));
        let err = error(consumer_wiring(&gear, "orders", "billing"));
        assert!(
            err.contains(&format!("gears.orders.clients.defaults.{key}: unknown key")),
            "{err}"
        );
    }
}

#[test]
fn non_object_shapes_are_rejected() {
    let cases = [
        (json!("x"), "gears.orders.clients must be an object"),
        (
            json!({ "consumes": "x" }),
            "gears.orders.clients.consumes must be an object",
        ),
        (
            json!({ "consumes": { "billing": "http://billing:8080" } }),
            "gears.orders.clients.consumes.billing must be an object",
        ),
    ];
    for (section, expected) in cases {
        let err = error(consumer_wiring(&clients(&section), "orders", "billing"));
        assert_eq!(err, expected);
    }
}

#[test]
fn an_invalid_value_names_the_entry() {
    let gear = clients(&json!({
        "consumes": { "billing": { "endpoint": "billing:8080" } }
    }));
    let err = error(consumer_wiring(&gear, "orders", "billing"));
    assert!(
        err.starts_with("gears.orders.clients.consumes.billing:"),
        "{err}"
    );
    assert!(err.contains("not an absolute URI"), "{err}");
}
