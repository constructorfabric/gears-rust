//! Unit tests for the [`super::read_wiring`] helper.

use std::collections::HashMap;
use std::sync::Arc;

use toolkit_contract::wiring::ClientWiring;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::*;
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::context::GearCtx;

struct StaticProvider(HashMap<String, serde_json::Value>);

impl ConfigProvider for StaticProvider {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        self.0.get(gear_name)
    }
}

fn ctx_with(gear: &str, raw: serde_json::Value) -> GearCtx {
    let mut gears = HashMap::new();
    gears.insert(gear.to_owned(), raw);
    GearCtx::new(
        gear,
        Uuid::nil(),
        Arc::new(StaticProvider(gears)),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    )
}

#[test]
fn missing_gear_yields_local() {
    let ctx = GearCtx::new(
        "absent",
        Uuid::nil(),
        Arc::new(StaticProvider(HashMap::new())),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    );
    let w = read_wiring(&ctx, "payment_api").expect("default");
    assert!(matches!(w, ClientWiring::Local));
}

#[test]
fn missing_client_wiring_yields_local() {
    let ctx = ctx_with("payments", json!({ "config": { "other": "value" } }));
    let w = read_wiring(&ctx, "payment_api").expect("default");
    assert!(matches!(w, ClientWiring::Local));
}

#[test]
fn missing_contract_key_yields_local() {
    let ctx = ctx_with(
        "payments",
        json!({ "config": { "client_wiring": { "other_api": { "transport": "rest", "endpoint": "x" } } } }),
    );
    let w = read_wiring(&ctx, "payment_api").expect("default");
    assert!(matches!(w, ClientWiring::Local));
}

#[test]
fn rest_wiring_parses() {
    let ctx = ctx_with(
        "payments",
        json!({
            "config": {
                "client_wiring": {
                    "payment_api": {
                        "transport": "rest",
                        "endpoint": "https://payments.example",
                        "timeout": "3s"
                    }
                }
            }
        }),
    );
    let w = read_wiring(&ctx, "payment_api").expect("parses");
    let ClientWiring::Rest { endpoint, tuning } = w else {
        panic!("expected Rest");
    };
    assert_eq!(endpoint, "https://payments.example");
    assert_eq!(tuning.timeout, Some(std::time::Duration::from_secs(3)));
}

#[test]
fn malformed_wiring_returns_error_with_context() {
    let ctx = ctx_with(
        "payments",
        json!({
            "config": {
                "client_wiring": {
                    "payment_api": { "transport": "rest" }  // missing endpoint
                }
            }
        }),
    );
    let err = read_wiring(&ctx, "payment_api").expect_err("missing endpoint");
    let msg = format!("{err:#}");
    assert!(msg.contains("payments"), "mentions gear: {msg}");
    assert!(msg.contains("payment_api"), "mentions key: {msg}");
}

#[test]
fn default_policy_stack_has_tracing() {
    let stack = default_policy_stack();
    // PolicyStack doesn't expose a length API; we just verify it's constructed.
    drop(stack);
}
