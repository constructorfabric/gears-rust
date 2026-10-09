#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use admission_control_sdk::{
    AdmissionEnginePluginClientV1, AdmissionEnginePluginSpecV1, EngineFailure, EngineRequest,
    EngineResult,
};
use async_trait::async_trait;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_gts::PluginV1;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::{MockTypesRegistryClient, internal, make_test_instance};

use super::{EngineResolveError, LazyEngine, resolve_engine};
use crate::config::EngineConfig;
use crate::domain::service::{EngineSource, EngineUnavailable};

struct Stub;

#[async_trait]
impl AdmissionEnginePluginClientV1 for Stub {
    async fn evaluate(
        &self,
        _ctx: &SecurityContext,
        _request: &EngineRequest,
    ) -> Result<EngineResult, EngineFailure> {
        Ok(EngineResult::Permit {
            shadow_denials: Vec::new(),
        })
    }
}

fn instance_id(segment: &str) -> String {
    format!(
        "{}{segment}",
        <AdmissionEnginePluginSpecV1 as gts::GtsSchema>::TYPE_ID
    )
}

fn registered(segment: &str, vendor: &str, priority: i16) -> types_registry_sdk::GtsInstance {
    let (id, content) =
        PluginV1::<AdmissionEnginePluginSpecV1>::build_registration(segment, vendor, priority)
            .unwrap();
    make_test_instance(id.as_ref(), content)
}

fn hub_with(segments: &[&str]) -> ClientHub {
    let hub = ClientHub::new();
    for segment in segments {
        let plugin: Arc<dyn AdmissionEnginePluginClientV1> = Arc::new(Stub);
        hub.register_scoped::<dyn AdmissionEnginePluginClientV1>(
            ClientScope::gts_id(&instance_id(segment)),
            plugin,
        );
    }
    hub
}

fn selection(vendor: &str, pinned: Option<&str>) -> EngineConfig {
    EngineConfig {
        vendor: vendor.to_owned(),
        instance_id: pinned.map(instance_id),
    }
}

#[tokio::test]
async fn lowest_priority_of_the_vendor_wins_and_ties_break_by_id() {
    let hub = hub_with(&[
        "acme.test.engine.a.v1",
        "acme.test.engine.b.v1",
        "acme.test.engine.c.v1",
    ]);
    let registry = MockTypesRegistryClient::new().with_instances([
        registered("acme.test.engine.c.v1", "acme", 1),
        registered("acme.test.engine.a.v1", "acme", 5),
        registered("acme.test.engine.b.v1", "acme", 1),
    ]);
    let engine = resolve_engine(&hub, &registry, &selection("acme", None))
        .await
        .unwrap();
    assert_eq!(engine.id, instance_id("acme.test.engine.b.v1"));
}

#[tokio::test]
async fn pinned_instance_wins_over_priority() {
    let hub = hub_with(&["acme.test.engine.a.v1", "acme.test.engine.b.v1"]);
    let registry = MockTypesRegistryClient::new().with_instances([
        registered("acme.test.engine.a.v1", "acme", 5),
        registered("acme.test.engine.b.v1", "acme", 1),
    ]);
    let pinned = selection("acme", Some("acme.test.engine.a.v1"));
    let engine = resolve_engine(&hub, &registry, &pinned).await.unwrap();
    assert_eq!(engine.id, instance_id("acme.test.engine.a.v1"));
}

#[tokio::test]
async fn unresolvable_selections_are_errors() {
    let hub = hub_with(&["acme.test.engine.a.v1"]);
    let acme = selection("acme", None);

    let empty = MockTypesRegistryClient::new();
    let err = resolve_engine(&hub, &empty, &acme).await.unwrap_err();
    assert!(matches!(err, EngineResolveError::Selection(_)), "{err}");

    let registry = MockTypesRegistryClient::new().with_instances([registered(
        "acme.test.engine.a.v1",
        "acme",
        1,
    )]);
    let missing = selection("acme", Some("acme.test.engine.missing.v1"));
    let err = resolve_engine(&hub, &registry, &missing).await.unwrap_err();
    assert!(
        matches!(err, EngineResolveError::PinnedInstanceNotFound(_)),
        "{err}"
    );

    let no_client = MockTypesRegistryClient::new().with_instances([registered(
        "acme.test.engine.b.v1",
        "acme",
        1,
    )]);
    let err = resolve_engine(&hub, &no_client, &acme).await.unwrap_err();
    assert!(
        matches!(err, EngineResolveError::ClientNotRegistered(_)),
        "{err}"
    );

    let failing = MockTypesRegistryClient::new().with_list_error(internal("registry down"));
    let err = resolve_engine(&hub, &failing, &acme).await.unwrap_err();
    assert!(matches!(err, EngineResolveError::Registry(_)), "{err}");
}

#[tokio::test]
async fn only_the_configured_vendor_is_eligible() {
    let hub = hub_with(&["acme.test.engine.a.v1", "other.test.engine.a.v1"]);
    // The other vendor's instance has the better (lower) priority.
    let registry = MockTypesRegistryClient::new().with_instances([
        registered("acme.test.engine.a.v1", "acme", 5),
        registered("other.test.engine.a.v1", "other", 1),
    ]);
    let engine = resolve_engine(&hub, &registry, &selection("acme", None))
        .await
        .unwrap();
    assert_eq!(engine.id, instance_id("acme.test.engine.a.v1"));

    let engine = resolve_engine(&hub, &registry, &selection("other", None))
        .await
        .unwrap();
    assert_eq!(engine.id, instance_id("other.test.engine.a.v1"));
}

#[tokio::test]
async fn a_pinned_instance_of_another_vendor_or_an_unknown_vendor_is_refused() {
    let hub = hub_with(&["acme.test.engine.a.v1", "other.test.engine.a.v1"]);
    let registry = MockTypesRegistryClient::new().with_instances([
        registered("acme.test.engine.a.v1", "acme", 1),
        registered("other.test.engine.a.v1", "other", 1),
    ]);

    let foreign_pin = selection("acme", Some("other.test.engine.a.v1"));
    let err = resolve_engine(&hub, &registry, &foreign_pin)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineResolveError::Selection(_)), "{err}");

    let err = resolve_engine(&hub, &registry, &selection("nobody", None))
        .await
        .unwrap_err();
    assert!(matches!(err, EngineResolveError::Selection(_)), "{err}");
}

#[tokio::test]
async fn a_client_registered_after_the_first_attempt_is_found_on_the_next() {
    // A failure is not cached, so a plugin that registers late is picked up
    // on the next call without a restart.
    let hub = Arc::new(ClientHub::new());
    let registry = MockTypesRegistryClient::new().with_instances([registered(
        "acme.test.engine.a.v1",
        "acme",
        1,
    )]);
    let engine = LazyEngine::new(
        Arc::clone(&hub),
        Arc::new(registry),
        selection("acme", None),
    );
    let err = engine.resolve().await.unwrap_err();
    assert!(
        matches!(err, EngineResolveError::ClientNotRegistered(_)),
        "{err}"
    );
    assert_eq!(engine.engine().await.unwrap_err(), EngineUnavailable);

    let plugin: Arc<dyn AdmissionEnginePluginClientV1> = Arc::new(Stub);
    hub.register_scoped::<dyn AdmissionEnginePluginClientV1>(
        ClientScope::gts_id(&instance_id("acme.test.engine.a.v1")),
        plugin,
    );
    let resolved = engine.engine().await.unwrap();
    assert_eq!(resolved.id, instance_id("acme.test.engine.a.v1"));
}
