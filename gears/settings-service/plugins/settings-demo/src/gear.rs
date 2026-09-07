// Created: 2026-09-06 by Constructor Tech
//! The gear: contribute the sample catalogue once the platform is ready for it.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use settings_service_sdk::models::GetEffectiveRequest;
use settings_service_sdk::{SettingsContributionClient, SettingsReaderClient};
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

        // Read one setting back the way a consuming gear would, so a boot log
        // shows the in-process read path answering at platform scope.
        let reader = hub.get::<dyn SettingsReaderClient>()?;
        let key = catalog::declarations()?
            .into_iter()
            .next()
            .map(|d| d.key)
            .ok_or_else(|| anyhow::anyhow!("the demo catalogue is empty"))?;
        match reader
            .get_effective(
                &SecurityContext::anonymous(),
                GetEffectiveRequest {
                    key: key.clone(),
                    scope: "/".to_owned(),
                },
            )
            .await
        {
            Ok(effective) => info!(
                key = %key,
                value = %effective.value,
                source = ?effective.source,
                "settings-demo read its first setting back"
            ),
            Err(error) => {
                warn!(key = %key, %error, "settings-demo could not read its setting back");
            }
        }

        // The machine path for the demo's secret, the way a consuming gear takes
        // it: the value comes back as an opaque handle, and resolving it is
        // authorized per setting and audited. The plaintext itself is never
        // logged; an unconfigured secret answers `SecretNotConfigured`.
        if let Some(secret) = catalog::declarations()?
            .into_iter()
            .find(|d| d.key.as_str().contains("api_token"))
        {
            let ctx = SecurityContext::anonymous();
            let request = GetEffectiveRequest {
                key: secret.key.clone(),
                scope: "/".to_owned(),
            };
            let outcome = match reader.get_effective(&ctx, request).await {
                Ok(effective) => match effective.value.as_str() {
                    Some(token) => reader
                        .resolve_secret(&ctx, settings_service_sdk::SecretHandle::new(token))
                        .await
                        .map(|plaintext| format!("plaintext of {} bytes", plaintext.len()))
                        .map_err(|e| settings_service_sdk::SettingsError::from(e).to_string()),
                    None => Err("the effective value carries no handle".to_owned()),
                },
                Err(error) => Err(error.to_string()),
            };
            info!(key = %secret.key, ?outcome, "settings-demo took the machine path for its secret");
        }
        Ok(())
    }
}
