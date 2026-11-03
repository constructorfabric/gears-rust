//! The registry: C1 and C2 at build, one built backend per entry (AC-4), and
//! no settings value in an error or a log line (AC-5).

use std::sync::Arc;

use event_broker_sdk::backend::{Provider, Role};
use memory_event_broker_plugin::MemoryBackendProvider;
use serde_json::json;
use toolkit_gts::GtsInstanceId;

use super::{BuildError, Registry};
use crate::config::checks::{self, Deployment};
use crate::config::{DeploymentMode, EventBrokerConfig};
use crate::domain::backend::BackendResolver;
use crate::domain::model::Topic;
use crate::domain::resolution::{Declaration, resolve};

const MEMORY_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~";
const POSTGRES_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~";
const TOPIC_TYPE: &str = "gts.cf.core.events.topic.v1~";
const AUDIT_TOPIC: &str = "gts.cf.core.events.topic.v1~vendor.app.audit.topic.v1";
const ORDERS_TOPIC: &str = "gts.cf.core.events.topic.v1~vendor.app.orders.topic.v1";
const BILLING_TOPIC: &str = "gts.cf.core.events.topic.v1~vendor.app.billing.topic.v1";

/// A value that stands for a credential: it must never surface.
const SENTINEL: &str = "s3cr3t-sentinel";

fn config(sections: &serde_json::Value) -> EventBrokerConfig {
    let mut cfg = json!({ "mode": "standalone" });
    for (key, value) in sections.as_object().expect("an object literal") {
        cfg[key] = value.clone();
    }
    serde_json::from_value(cfg).expect("test configuration deserializes")
}

fn deployment(cfg: &EventBrokerConfig) -> Deployment {
    checks::check(cfg, DeploymentMode::Standalone)
        .expect("the configuration keeps every rule")
        .expect("standalone holds a registry")
}

fn memory_only() -> Vec<Box<dyn Provider>> {
    vec![Box::new(MemoryBackendProvider::default())]
}

fn topic(cfg: &EventBrokerConfig, id: &str) -> Topic {
    let id = GtsInstanceId::try_new(id).expect("a valid topic id");
    let routing = checks::routing(cfg).expect("routes every topic");
    let settings = resolve(cfg, &routing, &id, &Declaration::default()).expect("resolves");
    Topic {
        id,
        description: "a topic".to_owned(),
        retention: None,
        settings,
    }
}

/// AC-4: each entry is built once, and every topic routed to it shares it.
#[tracing_test::traced_test]
#[tokio::test]
async fn two_topics_routed_to_one_entry_share_one_built_backend() {
    let cfg = config(&json!({
        "backends": {
            "main": { "type": MEMORY_BACKEND },
            "audit": { "type": MEMORY_BACKEND, "store": "audit" },
        },
        "backend_routing": { TOPIC_TYPE: "main", AUDIT_TOPIC: "audit" },
    }));
    let registry = Registry::build(deployment(&cfg), &memory_only(), Role::Standalone)
        .await
        .expect("every entry builds");

    let orders = registry.resolve(&topic(&cfg, ORDERS_TOPIC));
    let billing = registry.resolve(&topic(&cfg, BILLING_TOPIC));
    let audit = registry.resolve(&topic(&cfg, AUDIT_TOPIC));
    assert!(
        Arc::ptr_eq(&orders, &billing),
        "both take the instance-less route"
    );
    assert!(
        !Arc::ptr_eq(&orders, &audit),
        "the audit topic has its own entry"
    );
    assert_eq!(
        registry.backends().len(),
        2,
        "one backend per entry, not per topic"
    );
    logs_assert(|lines: &[&str]| {
        let built: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|line| line.contains("storage backend built"))
            .collect();
        let named = |name: &str| {
            built
                .iter()
                .filter(|line| line.contains(&format!("backend={name}")))
                .count()
        };
        match (built.len(), named("audit"), named("main")) {
            (2, 1, 1) => Ok(()),
            _ => Err(format!("expected each entry built once, got {built:?}")),
        }
    });
}

/// The record's own resolved name is what picks the backend: the routing is
/// consulted only when that name is not in this registry.
#[tokio::test]
async fn a_topic_naming_an_entry_this_registry_holds_resolves_to_it_over_the_routing() {
    let previous = config(&json!({
        "backends": {
            "main": { "type": MEMORY_BACKEND },
            "audit": { "type": MEMORY_BACKEND, "store": "audit" },
        },
        "backend_routing": { TOPIC_TYPE: "main", ORDERS_TOPIC: "audit" },
    }));
    let current = config(&json!({
        "backends": {
            "main": { "type": MEMORY_BACKEND },
            "audit": { "type": MEMORY_BACKEND, "store": "audit" },
        },
        "backend_routing": { TOPIC_TYPE: "main", AUDIT_TOPIC: "audit" },
    }));
    let registry = Registry::build(deployment(&current), &memory_only(), Role::Standalone)
        .await
        .expect("every entry builds");

    let recorded = registry.resolve(&topic(&previous, ORDERS_TOPIC));
    let audit = registry.resolve(&topic(&current, AUDIT_TOPIC));
    let main = registry.resolve(&topic(&current, ORDERS_TOPIC));
    assert!(Arc::ptr_eq(&recorded, &audit), "the recorded entry wins");
    assert!(!Arc::ptr_eq(&recorded, &main));
}

/// A topic record resolved under another configuration may name an entry this
/// registry does not hold; the routing answers for it instead.
#[tokio::test]
async fn a_topic_naming_an_entry_this_registry_lacks_falls_back_to_its_route() {
    let previous = config(&json!({
        "backends": {
            "main": { "type": MEMORY_BACKEND },
            "retired": { "type": MEMORY_BACKEND, "store": "retired" },
        },
        "backend_routing": { TOPIC_TYPE: "main", AUDIT_TOPIC: "retired" },
    }));
    let current = config(&json!({
        "backends": {
            "main": { "type": MEMORY_BACKEND },
            "audit": { "type": MEMORY_BACKEND, "store": "audit" },
        },
        "backend_routing": { TOPIC_TYPE: "main", AUDIT_TOPIC: "audit" },
    }));
    let registry = Registry::build(deployment(&current), &memory_only(), Role::Standalone)
        .await
        .expect("every entry builds");

    let stale = registry.resolve(&topic(&previous, AUDIT_TOPIC));
    let fresh = registry.resolve(&topic(&current, AUDIT_TOPIC));
    assert!(Arc::ptr_eq(&stale, &fresh));
}

/// C1. The error names the entry, its type and what the build links.
#[tokio::test]
async fn an_entry_whose_type_no_linked_provider_serves_stops_the_build() {
    let cfg = config(&json!({
        "backends": { "main": { "type": POSTGRES_BACKEND, "url": SENTINEL } },
        "backend_routing": { TOPIC_TYPE: "main" },
    }));

    let err = Registry::build(deployment(&cfg), &memory_only(), Role::Standalone)
        .await
        .err()
        .expect("C1");
    assert!(matches!(err, BuildError::UnservedType { .. }));
    assert_eq!(
        err.to_string(),
        format!(
            "backends: entry 'main' names backend type '{POSTGRES_BACKEND}', which no provider linked into this build serves; it links [\"{MEMORY_BACKEND}\"]; every entry's type is served by a linked provider (feature 0005 section 2.4, C1)"
        )
    );
}

/// C2, and AC-5: the provider's refusal names the setting, never its value.
#[tracing_test::traced_test]
#[tokio::test]
async fn an_entry_whose_settings_its_provider_refuses_stops_the_build_without_the_value() {
    let cfg = config(&json!({
        "backends": { "main": { "type": MEMORY_BACKEND, "password": SENTINEL } },
        "backend_routing": { TOPIC_TYPE: "main" },
    }));

    let err = Registry::build(deployment(&cfg), &memory_only(), Role::Standalone)
        .await
        .err()
        .expect("C2");
    assert!(matches!(err, BuildError::RefusedSettings { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        format!(
            "backends: entry 'main' (type '{MEMORY_BACKEND}') was refused by its provider: invalid backend config: `password` is not a memory backend setting; every entry's settings are accepted by its provider (feature 0005 section 2.4, C2)"
        )
    );
    for rendering in [err.to_string(), format!("{err:?}"), format!("{cfg:?}")] {
        assert!(!rendering.contains(SENTINEL), "{rendering}");
    }
    assert!(!logs_contain(SENTINEL));
}

/// AC-5 for the checks: no check error reproduces a settings value.
#[test]
fn no_check_error_reproduces_a_settings_value() {
    let broken = [
        (
            json!({ "backends": { "main": { "type": MEMORY_BACKEND, "k": SENTINEL } }, "backend_routing": {} }),
            checks::Error::MissingBaseRoute,
        ),
        (
            json!({ "backends": { "main": { "type": MEMORY_BACKEND, "k": SENTINEL } }, "backend_routing": { TOPIC_TYPE: SENTINEL } }),
            checks::Error::UnknownBackend {
                key: TOPIC_TYPE.to_owned(),
            },
        ),
        (
            json!({ "backends": {
                "main": { "type": MEMORY_BACKEND },
                "spare": { "type": MEMORY_BACKEND, "k": SENTINEL },
            }, "backend_routing": { TOPIC_TYPE: "main" } }),
            checks::Error::UnreferencedBackend {
                name: crate::domain::backend::Name::parse("spare").expect("a valid name"),
            },
        ),
    ];
    for (sections, expected) in broken {
        let err = checks::check(&config(&sections), DeploymentMode::Standalone)
            .expect_err("each breaks a rule");
        assert_eq!(err, expected);
        assert!(!err.to_string().contains(SENTINEL), "{err}");
        assert!(!format!("{err:?}").contains(SENTINEL), "{err:?}");
    }
}
