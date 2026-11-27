//! Tests for [`LazyTenantOwnershipChecker`].
//!
//! Pins the contract the gear relies on:
//!
//!   * vendor match → the plugin's answer drives the ownership count;
//!   * no matching plugin → retryable `service_unavailable` (no silent
//!     fallback to "owns nothing"), and the failure is not cached, so a
//!     plugin that registers later is picked up;
//!   * plugin failures and timeouts → `service_unavailable`;
//!   * the cleanup hook maps `Retryable` / `Terminal` / unresolvable /
//!     timeout onto the hard-delete pipeline's `HookError`.

use std::sync::Arc;
use std::time::Duration;

use account_management_sdk::{
    TenantOwnershipFailure, TenantOwnershipPluginClient, TenantOwnershipPluginSpecV1,
};
use async_trait::async_trait;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use super::LazyTenantOwnershipChecker;
use crate::domain::error::DomainError;
use crate::domain::tenant::hooks::HookError;
use crate::domain::tenant::resource_checker::ResourceOwnershipChecker;

const VENDOR: &str = "acme";

/// Build an `(instance_id, content)` advertisement for the plugin spec.
fn build_instance(segment: &str, vendor: &str, priority: i16) -> (String, serde_json::Value) {
    let (id, payload) =
        PluginV1::<TenantOwnershipPluginSpecV1>::build_registration(segment, vendor, priority)
            .expect("build_registration must succeed for the marker spec");
    (id.as_ref().to_owned(), payload)
}

/// Scripted plugin: fixed answers for both methods, optional delay.
struct ScriptedPlugin {
    owns: Result<bool, TenantOwnershipFailure>,
    cleanup: Result<(), TenantOwnershipFailure>,
    delay: Duration,
}

impl ScriptedPlugin {
    fn answering(owns: bool) -> Self {
        Self {
            owns: Ok(owns),
            cleanup: Ok(()),
            delay: Duration::ZERO,
        }
    }
}

#[async_trait]
impl TenantOwnershipPluginClient for ScriptedPlugin {
    async fn owns_anything(&self, _tenant_id: Uuid) -> Result<bool, TenantOwnershipFailure> {
        tokio::time::sleep(self.delay).await;
        self.owns.clone()
    }

    async fn cleanup_tenant(&self, _tenant_id: Uuid) -> Result<(), TenantOwnershipFailure> {
        tokio::time::sleep(self.delay).await;
        self.cleanup.clone()
    }
}

/// A hub + catalogue with one advertised instance of `VENDOR`; the plugin
/// is bound under its scope only when `plugin` is `Some`.
fn setup(
    plugin: Option<ScriptedPlugin>,
) -> (Arc<ClientHub>, String, Arc<LazyTenantOwnershipChecker>) {
    let (gts_id, instance) = build_instance("cf.test.tenant_ownership.plugin.v1", VENDOR, 100);
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(&gts_id, instance);
    let hub = Arc::new(ClientHub::new());
    if let Some(plugin) = plugin {
        let plugin: Arc<dyn TenantOwnershipPluginClient> = Arc::new(plugin);
        hub.register_scoped::<dyn TenantOwnershipPluginClient>(
            ClientScope::gts_id(&gts_id),
            plugin,
        );
    }
    let checker = Arc::new(
        LazyTenantOwnershipChecker::new(Arc::clone(&hub), registry, VENDOR.to_owned())
            .with_timeouts(Duration::from_millis(10), Duration::from_millis(10)),
    );
    (hub, gts_id, checker)
}

fn tenant() -> Uuid {
    Uuid::from_u128(0x7E7A)
}

#[tokio::test]
async fn owns_anything_true_counts_as_owned() {
    let (_, _, checker) = setup(Some(ScriptedPlugin::answering(true)));
    assert_eq!(checker.count_ownership_links(tenant()).await.unwrap(), 1);
}

#[tokio::test]
async fn owns_anything_false_counts_as_nothing_owned() {
    let (_, _, checker) = setup(Some(ScriptedPlugin::answering(false)));
    assert_eq!(checker.count_ownership_links(tenant()).await.unwrap(), 0);
}

#[tokio::test]
async fn unresolvable_plugin_is_service_unavailable_not_zero() {
    let (_, _, checker) = setup(None);
    let err = checker
        .count_ownership_links(tenant())
        .await
        .expect_err("no plugin must not read as 'owns nothing'");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn vendor_mismatch_is_service_unavailable() {
    let (gts_id, instance) = build_instance("cf.test.other_ownership.plugin.v1", "other", 100);
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(&gts_id, instance);
    let hub = Arc::new(ClientHub::new());
    let plugin: Arc<dyn TenantOwnershipPluginClient> = Arc::new(ScriptedPlugin::answering(false));
    hub.register_scoped::<dyn TenantOwnershipPluginClient>(ClientScope::gts_id(&gts_id), plugin);
    let checker = LazyTenantOwnershipChecker::new(hub, registry, VENDOR.to_owned());
    let err = checker
        .count_ownership_links(tenant())
        .await
        .expect_err("mismatch");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn failed_resolution_is_not_cached() {
    let (hub, gts_id, checker) = setup(None);
    checker
        .count_ownership_links(tenant())
        .await
        .expect_err("not registered yet");
    // The owning gear finishes its init after AM: the next call resolves.
    let plugin: Arc<dyn TenantOwnershipPluginClient> = Arc::new(ScriptedPlugin::answering(true));
    hub.register_scoped::<dyn TenantOwnershipPluginClient>(ClientScope::gts_id(&gts_id), plugin);
    assert_eq!(checker.count_ownership_links(tenant()).await.unwrap(), 1);
}

#[tokio::test]
async fn plugin_failure_is_service_unavailable() {
    let (_, _, checker) = setup(Some(ScriptedPlugin {
        owns: Err(TenantOwnershipFailure::Retryable {
            detail: "db down".into(),
        }),
        cleanup: Ok(()),
        delay: Duration::ZERO,
    }));
    let err = checker
        .count_ownership_links(tenant())
        .await
        .expect_err("failure");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn slow_plugin_times_out_as_service_unavailable() {
    let (_, _, checker) = setup(Some(ScriptedPlugin {
        owns: Ok(false),
        cleanup: Ok(()),
        delay: Duration::from_secs(60),
    }));
    let err = checker
        .count_ownership_links(tenant())
        .await
        .expect_err("timeout");
    assert!(err.to_string().contains("timeout"), "{err}");
}

#[tokio::test]
async fn cleanup_hook_success() {
    let (_, _, checker) = setup(Some(ScriptedPlugin::answering(false)));
    checker.cleanup_hook()(tenant()).await.expect("cleanup ok");
}

#[tokio::test]
async fn cleanup_hook_maps_terminal_and_retryable() {
    let (_, _, terminal) = setup(Some(ScriptedPlugin {
        owns: Ok(false),
        cleanup: Err(TenantOwnershipFailure::Terminal {
            detail: "still owns resources".into(),
        }),
        delay: Duration::ZERO,
    }));
    assert!(matches!(
        terminal.cleanup_hook()(tenant()).await,
        Err(HookError::Terminal { .. })
    ));

    let (_, _, retryable) = setup(Some(ScriptedPlugin {
        owns: Ok(false),
        cleanup: Err(TenantOwnershipFailure::Retryable {
            detail: "db down".into(),
        }),
        delay: Duration::ZERO,
    }));
    assert!(matches!(
        retryable.cleanup_hook()(tenant()).await,
        Err(HookError::Retryable { .. })
    ));
}

#[tokio::test]
async fn cleanup_hook_unresolvable_plugin_is_retryable() {
    let (_, _, checker) = setup(None);
    assert!(matches!(
        checker.cleanup_hook()(tenant()).await,
        Err(HookError::Retryable { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn cleanup_hook_over_budget_is_retryable() {
    let (_, _, checker) = setup(Some(ScriptedPlugin {
        owns: Ok(false),
        cleanup: Ok(()),
        delay: Duration::from_secs(60),
    }));
    assert!(matches!(
        checker.cleanup_hook()(tenant()).await,
        Err(HookError::Retryable { .. })
    ));
}
