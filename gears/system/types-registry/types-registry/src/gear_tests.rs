use std::sync::Arc;

use toolkit::ClientHub;

use super::{held_while, refuse_remote_wiring};

/// Stands in for the seeded client: what is held is irrelevant to the handoff.
struct Held;

#[tokio::test]
async fn the_entry_is_held_only_while_wiring_succeeds() {
    let hub = ClientHub::default();
    let result = held_while(&hub, Arc::new(Held), async {
        assert!(hub.try_get::<Held>().is_some(), "held while wiring");
        Ok(())
    })
    .await;
    assert!(result.is_ok());
    assert!(hub.try_get::<Held>().is_none(), "removed after success");
}

#[tokio::test]
async fn a_wiring_failure_is_returned_and_the_entry_removed() {
    let hub = ClientHub::default();
    let result = held_while(&hub, Arc::new(Held), async {
        anyhow::bail!("wiring failed")
    })
    .await;
    assert_eq!(
        result.expect_err("the failure is returned").to_string(),
        "wiring failed"
    );
    assert!(hub.try_get::<Held>().is_none(), "removed after failure");
}

/// Dropping the handoff mid-wiring — a cancelled `init` — still removes the entry.
#[tokio::test]
async fn an_unfinished_handoff_removes_the_entry_when_dropped() {
    let hub = ClientHub::default();
    {
        let handoff = held_while(&hub, Arc::new(Held), std::future::pending());
        let mut handoff = std::pin::pin!(handoff);
        std::future::poll_fn(|cx| {
            assert!(
                handoff.as_mut().poll(cx).is_pending(),
                "wiring never finishes"
            );
            std::task::Poll::Ready(())
        })
        .await;
        assert!(hub.try_get::<Held>().is_some(), "held while wiring");
    }
    assert!(hub.try_get::<Held>().is_none(), "removed when dropped");
}

/// A panic in wiring still removes the entry.
#[tokio::test]
async fn a_panicking_wiring_removes_the_entry() {
    let hub = Arc::new(ClientHub::default());
    let task = tokio::spawn({
        let hub = Arc::clone(&hub);
        async move { held_while(&hub, Arc::new(Held), async { panic!("wiring panicked") }).await }
    });
    assert!(task.await.expect_err("the wiring panicked").is_panic());
    assert!(hub.try_get::<Held>().is_none(), "removed after the panic");
}

struct Gear(serde_json::Value);

impl toolkit::config::ConfigProvider for Gear {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        (gear == "types-registry").then_some(&self.0)
    }
}

#[test]
fn a_remote_binding_for_a_provided_contract_is_refused() {
    for key in ["platform_types_registry_api", "types_registry_api"] {
        let gear = Gear(serde_json::json!({
            "clients": { "provides": { key: { "transport": "rest", "endpoint": "http://r:1" } } }
        }));
        let err = refuse_remote_wiring(&gear, "types-registry")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("clients.provides.{key}")),
            "names the entry: {err}"
        );
    }
}

#[test]
fn local_or_absent_wiring_is_accepted() {
    for gear in [
        Gear(serde_json::json!({ "config": {} })),
        Gear(serde_json::json!({
            "clients": { "provides": { "types_registry_api": { "transport": "local" } } }
        })),
    ] {
        refuse_remote_wiring(&gear, "types-registry").unwrap();
    }
}
