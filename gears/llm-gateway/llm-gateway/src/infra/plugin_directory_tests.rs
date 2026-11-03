//! Plugin selection: provider-type match, priority tie-break, no-match error.

use std::sync::Arc;

use llm_gateway_sdk::{LlmGatewayError, LlmGatewayProviderPluginClientV1};
use toolkit::client_hub::{ClientHub, ClientScope};
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::{MockTypesRegistryClient, internal, make_test_instance};

use super::{GtsPluginDirectory, select_plugin_instance};
use crate::domain::ports::PluginDirectory;
use crate::domain::test_support::{CallLog, FakePlugin};

const OPENAI: &str = "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~";
const ANTHROPIC: &str = "gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~";
const PLUGIN_PREFIX: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.llmgw.provider.plugin.v1~";

/// A registered plugin instance `(instance id, instance JSON)`.
fn instance(name: &str, provider_type: &str, priority: i16) -> (String, serde_json::Value) {
    let id = format!("{PLUGIN_PREFIX}{name}.plugin.v1");
    let value = serde_json::json!({
        "id": id,
        "vendor": "constructorfabric",
        "priority": priority,
        "properties": { "provider_type": provider_type, "tool_namespace": "ns" },
    });
    (id, value)
}

#[test]
fn selects_the_plugin_whose_provider_type_matches() {
    let instances = vec![
        instance("cf.builtin.anthropic", ANTHROPIC, 100),
        instance("cf.builtin.openai", OPENAI, 100),
    ];
    let chosen = select_plugin_instance(OPENAI, &instances).expect("openai plugin matches");
    assert_eq!(chosen, instances[1].0);
}

#[test]
fn lowest_priority_value_wins_among_matching_plugins() {
    let instances = vec![
        instance("cf.vendor_a.openai", OPENAI, 100),
        instance("cf.vendor_b.openai", OPENAI, 10),
        instance("cf.vendor_c.openai", OPENAI, 50),
    ];
    let chosen = select_plugin_instance(OPENAI, &instances).expect("a match exists");
    assert_eq!(chosen, instances[1].0);
}

#[test]
fn priority_does_not_override_the_provider_type_match() {
    let instances = vec![
        instance("cf.builtin.anthropic", ANTHROPIC, 1),
        instance("cf.builtin.openai", OPENAI, 500),
    ];
    let chosen = select_plugin_instance(OPENAI, &instances).expect("openai plugin matches");
    assert_eq!(chosen, instances[1].0);
}

#[test]
fn no_matching_plugin_is_provider_not_supported() {
    let instances = vec![instance("cf.builtin.anthropic", ANTHROPIC, 100)];
    let err = select_plugin_instance(OPENAI, &instances).expect_err("no openai plugin");
    match err {
        LlmGatewayError::ProviderNotSupported { provider_type, .. } => {
            assert_eq!(provider_type, OPENAI);
        }
        other => panic!("expected provider_not_supported, got {other}"),
    }
}

#[test]
fn empty_directory_is_provider_not_supported() {
    let err = select_plugin_instance(OPENAI, &[]).expect_err("nothing registered");
    assert_eq!(err.code(), "provider_not_supported");
}

// ── GtsPluginDirectory ──────────────────────────────────────────────────────

fn registry_with(instances: &[(String, serde_json::Value)]) -> Arc<MockTypesRegistryClient> {
    Arc::new(
        MockTypesRegistryClient::new().with_instances(
            instances
                .iter()
                .map(|(id, value)| make_test_instance(id, value.clone())),
        ),
    )
}

fn hub_with(registry: &Arc<MockTypesRegistryClient>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    let client: Arc<dyn TypesRegistryClient> = registry.clone();
    hub.register::<dyn TypesRegistryClient>(client);
    hub
}

fn register_plugin(hub: &ClientHub, instance_id: &str) -> Arc<FakePlugin> {
    let plugin = FakePlugin::echo_id(Arc::new(CallLog::default()));
    let client: Arc<dyn LlmGatewayProviderPluginClientV1> = plugin.clone();
    hub.register_scoped::<dyn LlmGatewayProviderPluginClientV1>(
        ClientScope::gts_id(instance_id),
        client,
    );
    plugin
}

fn same_client(got: &Arc<dyn LlmGatewayProviderPluginClientV1>, want: &Arc<FakePlugin>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(got), Arc::as_ptr(want))
}

#[tokio::test]
async fn non_empty_instance_list_is_cached_after_the_first_load() {
    let instances = vec![instance("cf.builtin.openai", OPENAI, 100)];
    let registry = registry_with(&instances);
    let hub = hub_with(&registry);
    let plugin = register_plugin(&hub, &instances[0].0);
    let directory = GtsPluginDirectory::new(hub);

    for _ in 0..2 {
        let got = directory.resolve(OPENAI).await.expect("plugin resolves");
        assert!(same_client(&got.client, &plugin));
    }
    assert_eq!(registry.list_instance_calls(), 1);
}

#[tokio::test]
async fn empty_instance_list_is_not_cached() {
    let registry = registry_with(&[]);
    let directory = GtsPluginDirectory::new(hub_with(&registry));

    for _ in 0..2 {
        let err = directory
            .resolve(OPENAI)
            .await
            .err()
            .expect("nothing registered");
        assert_eq!(err.code(), "provider_not_supported");
    }
    assert_eq!(registry.list_instance_calls(), 2);
}

#[tokio::test]
async fn types_registry_failure_is_an_internal_error() {
    let registry = Arc::new(MockTypesRegistryClient::new().with_list_error(internal("boom")));
    let directory = GtsPluginDirectory::new(hub_with(&registry));

    let err = directory
        .resolve(OPENAI)
        .await
        .err()
        .expect("listing fails");
    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn missing_types_registry_client_is_an_internal_error() {
    let directory = GtsPluginDirectory::new(Arc::new(ClientHub::new()));
    let err = directory
        .resolve(OPENAI)
        .await
        .err()
        .expect("no registry client");
    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn matching_instance_without_a_scoped_client_is_provider_not_supported() {
    let instances = vec![instance("cf.builtin.openai", OPENAI, 100)];
    let registry = registry_with(&instances);
    let directory = GtsPluginDirectory::new(hub_with(&registry));

    let err = directory
        .resolve(OPENAI)
        .await
        .err()
        .expect("client is missing");
    assert_eq!(err.code(), "provider_not_supported");
}

#[tokio::test]
async fn the_client_is_looked_up_under_the_instance_gts_id_scope() {
    let instances = vec![
        instance("cf.vendor_a.openai", OPENAI, 10),
        instance("cf.vendor_b.openai", OPENAI, 50),
    ];
    let registry = registry_with(&instances);
    let hub = hub_with(&registry);
    let directory = GtsPluginDirectory::new(hub.clone());

    // Only the losing instance has a client: the winner's scope is empty.
    let other = register_plugin(&hub, &instances[1].0);
    let err = directory
        .resolve(OPENAI)
        .await
        .err()
        .expect("winner has no client");
    assert_eq!(err.code(), "provider_not_supported");

    let winner = register_plugin(&hub, &instances[0].0);
    let got = directory.resolve(OPENAI).await.expect("winner resolves");
    assert!(same_client(&got.client, &winner));
    assert!(!same_client(&got.client, &other));
}

#[tokio::test]
async fn resolved_plugin_carries_the_tool_namespace_of_its_instance() {
    let instances = vec![instance("cf.builtin.openai", OPENAI, 100)];
    let registry = registry_with(&instances);
    let hub = hub_with(&registry);
    register_plugin(&hub, &instances[0].0);
    let directory = GtsPluginDirectory::new(hub);

    let got = directory.resolve(OPENAI).await.expect("plugin resolves");
    assert_eq!(got.tool_namespace, "ns");
}
