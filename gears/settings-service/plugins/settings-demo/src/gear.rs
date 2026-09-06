// Created: 2026-09-06 by Constructor Tech
//! The gear: contribute the sample catalogue once the platform is ready for it.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use settings_service_sdk::SettingsContributionClient;
use toolkit::client_hub::ClientHub;
use toolkit::contracts::SystemCapability;
use toolkit::{Gear, GearCtx};
use toolkit_security::SecurityContext;
use tracing::{info, warn};

use crate::catalog;

/// Contributes the demo declarations to the Settings Service.
///
/// Everything happens in `post_init`, which runs after every gear's `init`.
/// Two orderings force that: the types registry admits the schemas registered
/// during the init phase — the value-type catalogue among them — into its
/// readable store only when it switches to ready mode in its own `post_init`,
/// so a declaration reconciled earlier finds its value type unknown; and the
/// `system` capability that grants the hook also moves this gear's `init`
/// ahead of the ordinary gears, `settings-service` included, so the
/// contribution client cannot be resolved there either. `init` keeps only the
/// hub; `deps` still names the provider so the topological order holds for
/// `post_init`, where system gears run in registry order.
#[toolkit::gear(name = "settings-demo", deps = [settings_service], capabilities = [system])]
#[derive(Default)]
pub struct SettingsDemo {
    hub: OnceLock<Arc<ClientHub>>,
}

#[async_trait]
impl Gear for SettingsDemo {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        self.hub
            .set(ctx.client_hub())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        Ok(())
    }
}

#[async_trait]
impl SystemCapability for SettingsDemo {
    async fn post_init(&self, _sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        let hub = self
            .hub
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} post_init before init", Self::MODULE_NAME))?;
        let contributions = hub.get::<dyn SettingsContributionClient>()?;
        let result = contributions
            .register_declarations(
                &SecurityContext::anonymous(),
                Self::MODULE_NAME.to_owned(),
                catalog::declarations()?,
            )
            .await?;

        // A refused item is the demo's own defect, reported per key; it does
        // not fail the boot, because the rest of the set landed.
        for error in &result.errors {
            warn!(
                key = %error.key,
                code = %error.code,
                message = %error.message,
                "settings-demo declaration refused"
            );
        }
        info!(
            registered = result.registered,
            updated = result.updated,
            reactivated = result.reactivated,
            refused = result.errors.len(),
            "settings-demo declarations reconciled"
        );
        Ok(())
    }
}
