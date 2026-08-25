//! Shared `PluginV1<Spec>` registration for every rate-provider plugin gear —
//! the source plugins (`RateProviderSourcePluginSpecV1`) and the core
//! composite gear (`bss_ledger_sdk::RateProviderPluginSpecV1`) all publish a
//! `PluginV1` instance and a scoped `RateProviderV1` client the same way.

use std::sync::Arc;

use bss_ledger_sdk::RateProviderV1;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

/// Register a `PluginV1<Spec>` instance in types-registry, then register the
/// scoped `RateProviderV1` client the discoverer resolves via the returned
/// instance id. Returns the instance id (for the caller's own log line).
///
/// # Errors
/// Propagates types-registry unavailability, a registration rejection (e.g. a
/// malformed `instance_segment`), or a partial-batch registration failure.
pub async fn register_rate_provider_plugin<Spec>(
    ctx: &GearCtx,
    instance_segment: &str,
    vendor: impl Into<String>,
    priority: i16,
    provider: Arc<dyn RateProviderV1>,
) -> anyhow::Result<String>
where
    Spec: gts::GtsSchema + gts::GtsSerialize + Default,
{
    let (instance_id, instance_json) =
        PluginV1::<Spec>::build_registration(instance_segment, vendor, priority)?;

    let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
    let results = registry.register(vec![instance_json]).await?;
    RegisterResult::ensure_all_ok(&results)?;

    ctx.client_hub()
        .register_scoped::<dyn RateProviderV1>(ClientScope::gts_id(&instance_id), provider);

    Ok(instance_id.to_string())
}

/// Test helper: assert a `PluginV1<Spec>` built for `instance_segment` parses
/// as a valid GTS id. Shared by every rate-provider plugin gear's
/// `gts_id_tests` module.
///
/// # Panics
/// Panics if `build_registration` fails, or the built id fails GTS parsing —
/// intended for use inside `#[test]` functions only.
#[allow(
    clippy::expect_used,
    reason = "test-only helper: panicking here is exactly a failed assertion in the caller's #[test] fn"
)]
pub fn assert_registration_builds_valid_gts_id<Spec>(
    instance_segment: &str,
    vendor: &str,
    priority: i16,
) where
    Spec: gts::GtsSchema + gts::GtsSerialize + Default,
{
    let (id, _json) = PluginV1::<Spec>::build_registration(instance_segment, vendor, priority)
        .expect("build_registration should succeed for a well-formed instance segment");
    assert!(
        gts::GtsId::try_new(id.as_ref()).is_ok(),
        "instance id is not a valid GTS id: {}",
        id.as_ref()
    );
}
