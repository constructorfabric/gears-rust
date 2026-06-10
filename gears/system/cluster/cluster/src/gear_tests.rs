// Created: 2026-06-16 by Constructor Tech
//! End-to-end lifecycle test for the `cluster` gear: drive `init` → `start` →
//! `stop` through a mock `GearCtx` and assert backends register under the
//! configured profile and unbind on stop.

use std::sync::Arc;

use cluster_sdk::{ClusterCacheV1, ClusterError, ClusterProfile};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit::contracts::RunnableCapability;
use toolkit::{ConfigProvider, Gear, GearCtx};

use super::ClusterGear;

/// Returns the `cluster` gear's config entry for `cluster`, and nothing else.
struct MockConfig(serde_json::Value);

impl ConfigProvider for MockConfig {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        (gear_name == "cluster").then_some(&self.0)
    }
}

#[derive(Clone, Copy)]
struct DefaultProfile;
impl ClusterProfile for DefaultProfile {
    const NAME: &'static str = "default";
}

#[tokio::test]
async fn gear_lifecycle_registers_then_unbinds() {
    let hub = Arc::new(ClientHub::default());
    // The provider returns the gear entry; `ctx.config()` reads its `config` field.
    let provider = Arc::new(MockConfig(serde_json::json!({
        "config": { "profiles": { "default": { "cache": { "provider": "standalone" } } } }
    })));
    let ctx = GearCtx::new(
        "cluster",
        uuid::Uuid::new_v4(),
        provider,
        Arc::clone(&hub),
        CancellationToken::new(),
    );

    let gear = ClusterGear::default();
    gear.init(&ctx)
        .await
        .expect("init parses config and captures the hub");
    gear.start(CancellationToken::new())
        .await
        .expect("start wires backends from config");

    // The configured cache (and the omit-default trio over it) resolves.
    assert!(
        ClusterCacheV1::resolver(&hub)
            .profile(DefaultProfile)
            .resolve()
            .is_ok(),
        "the standalone cache is registered for the `default` profile"
    );

    gear.stop(CancellationToken::new())
        .await
        .expect("stop tears the wiring down");

    assert!(
        matches!(
            ClusterCacheV1::resolver(&hub)
                .profile(DefaultProfile)
                .resolve(),
            Err(ClusterError::ProfileNotBound { .. })
        ),
        "stop deregisters the profile's backends"
    );
}

#[tokio::test]
async fn gear_with_no_config_starts_empty() {
    // No `cluster` entry → default (empty) config → start binds nothing, no panic.
    let hub = Arc::new(ClientHub::default());
    let provider = Arc::new(MockConfig(serde_json::json!({})));
    let ctx = GearCtx::new(
        "other-gear",
        uuid::Uuid::new_v4(),
        provider,
        Arc::clone(&hub),
        CancellationToken::new(),
    );

    let gear = ClusterGear::default();
    gear.init(&ctx).await.expect("init");
    gear.start(CancellationToken::new())
        .await
        .expect("start with empty config");
    gear.stop(CancellationToken::new()).await.expect("stop");
}
