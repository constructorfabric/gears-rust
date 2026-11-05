//! Retain the prepared router between stages; layer a clone for publication.

use std::sync::Arc;

use axum::Router;

use super::HostRuntime;
use crate::contracts::OpenApiRegistry;
use crate::healthcheck::RestHealthcheckRegistry;
use crate::registry::{GearEntry, RegistryError, RestApiCap};

pub(super) struct RestComposition {
    router: Router,
    health: Arc<RestHealthcheckRegistry>,
}

impl HostRuntime {
    pub(super) fn rest_health_registry(&self) -> Arc<RestHealthcheckRegistry> {
        let health = Arc::new(RestHealthcheckRegistry::with_cancellation(
            self.owned.clone(),
        ));
        health.register(
            "readiness",
            Arc::new(crate::runtime::ReadinessHealthcheck::new(Arc::clone(
                &self.dep_checker,
            ))),
        );
        health
    }

    pub(super) async fn bind_rest_bootstrap(&self) -> anyhow::Result<Option<RestComposition>> {
        let Some((entry, host)) = self.rest_host() else {
            return Ok(None);
        };
        let ctx = self.ctx_builder.for_gear(entry.name).await?;
        let health = self.rest_health_registry();
        let router = host
            .rest_bootstrap(&ctx, Arc::clone(&health), self.owned.clone())
            .await?;
        Ok(router.map(|router| RestComposition { router, health }))
    }

    pub(super) async fn add_rest_routes(
        &self,
        mut router: Router,
        health: &Arc<RestHealthcheckRegistry>,
        entries: &[&GearEntry],
        registry: &dyn OpenApiRegistry,
    ) -> Result<Router, RegistryError> {
        for entry in entries {
            let Some(rest) = entry.caps.query::<RestApiCap>() else {
                continue;
            };
            let ctx = self
                .ctx_builder
                .for_gear(entry.name)
                .await
                .map_err(|source| RegistryError::RestRegister {
                    gear: entry.name,
                    source,
                })?;
            router = rest
                .register_rest(&ctx, router, registry)
                .map_err(|source| RegistryError::RestRegister {
                    gear: entry.name,
                    source,
                })?;
            if let Some(check) = rest.healthcheck(&ctx) {
                health.register(entry.name, check);
            }
        }
        Ok(router)
    }

    pub(super) async fn publish_system_rest(
        &self,
        composition: &mut RestComposition,
        system: &[&GearEntry],
    ) -> anyhow::Result<()> {
        let (entry, host) = self
            .rest_host()
            .ok_or(RegistryError::RestHostNotFoundAfterValidation)?;
        composition.router = self
            .add_rest_routes(
                composition.router.clone(),
                &composition.health,
                system,
                host.as_registry(),
            )
            .await?;
        let ctx = self.ctx_builder.for_gear(entry.name).await?;
        host.rest_finalize_bootstrap(
            &ctx,
            composition.router.clone(),
            Arc::clone(&composition.health),
        )?;
        self.run_directory_register_phase(system).await?;
        Ok(())
    }

    pub(super) async fn finish_rest_bootstrap(
        &self,
        composition: RestComposition,
        app: &[&GearEntry],
    ) -> anyhow::Result<()> {
        let (entry, host) = self
            .rest_host()
            .ok_or(RegistryError::RestHostNotFoundAfterValidation)?;
        let router = self
            .add_rest_routes(
                composition.router,
                &composition.health,
                app,
                host.as_registry(),
            )
            .await?;
        let ctx = self.ctx_builder.for_gear(entry.name).await?;
        drop(host.rest_finalize(&ctx, router, composition.health)?);
        Ok(())
    }
}
