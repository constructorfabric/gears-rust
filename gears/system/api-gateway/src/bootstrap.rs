//! One listener serves probes, system routes, then the complete API.
//! Each router receives its auth layers before publication.

use std::sync::Arc;

use axum::{
    Router,
    response::{IntoResponse as _, Response},
};
use tokio_util::sync::CancellationToken;
use toolkit::contracts::ApiGatewayCapability as _;
use tower::ServiceExt as _;

use super::ApiGateway;
use crate::config::HealthServeMode;

impl ApiGateway {
    pub(super) async fn bootstrap_rest(
        &self,
        ctx: &toolkit::GearCtx,
        health: Arc<toolkit::RestHealthcheckRegistry>,
        cancel: CancellationToken,
    ) -> anyhow::Result<Router> {
        self.configure_transport(ctx).await?;
        let cfg = self.get_cached_config();
        let base = self.rest_prepare(ctx, Router::new(), Arc::clone(&health))?;
        let mut probes = Router::new().fallback(|| async { starting_response() });
        if matches!(
            cfg.health.serve,
            HealthServeMode::Main | HealthServeMode::Both
        ) {
            probes = probes.merge(Self::health_routes(
                health,
                std::time::Duration::from_millis(cfg.healthcheck_timeout_ms),
            ));
        }
        let prefix = Self::normalize_prefix_path(&cfg.prefix_path)?;
        self.bootstrap_routes
            .store(Arc::new(Self::apply_prefix(probes, &prefix).with_state(())));
        let addr = Self::parse_bind_address(&cfg.bind_addr)?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let bound = listener.local_addr()?;
        let health_bound = self.bind_health_listener(&cfg).await?;
        let live = Arc::clone(&self.bootstrap_routes);
        let outer = Router::new().fallback_service(tower::service_fn(move |request| {
            let router = live.load_full();
            async move { router.as_ref().clone().oneshot(request).await }
        }));
        let stop = cancel.child_token();
        self.bootstrap_cancel
            .set(stop.clone())
            .map_err(|_| anyhow::anyhow!("REST bootstrap already started"))?;
        let task = tokio::spawn(async move {
            let main = axum::serve(
                listener,
                outer.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(stop.clone().cancelled_owned());
            if let Some((listener, router)) = health_bound {
                let health = axum::serve(listener, router.into_make_service())
                    .with_graceful_shutdown(stop.cancelled_owned());
                tokio::try_join!(main.into_future(), health.into_future())?;
            } else {
                main.await?;
            }
            Ok(())
        });
        *self.bootstrap_task.lock() = Some(tokio_util::task::AbortOnDropHandle::new(task));
        self.publish_bound_endpoint(&cfg, bound);
        Ok(base)
    }

    pub(super) async fn serve_bootstrap_rest(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let mut task = self
            .bootstrap_task
            .lock()
            .take()
            .ok_or_else(|| anyhow::anyhow!("REST bootstrap server is missing"))?;
        tokio::select! {
            result = &mut task => result?,
            () = cancel.cancelled() => {
                if let Some(stop) = self.bootstrap_cancel.get() {
                    stop.cancel();
                }
                task.await?
            }
        }
    }

    pub(super) async fn shutdown_bootstrap_rest(&self) -> anyhow::Result<()> {
        if let Some(stop) = self.bootstrap_cancel.get() {
            stop.cancel();
        }
        let task = self.bootstrap_task.lock().take();
        if let Some(task) = task {
            task.await??;
        }
        Ok(())
    }
}

pub(super) fn starting_response() -> Response {
    toolkit_canonical_errors::CanonicalError::service_unavailable()
        .with_retry_after_seconds(1)
        .with_detail("application preparation has not completed")
        .create()
        .into_response()
}

/// Reject tenant calls until application authentication is ready.
pub(super) fn restrict_to_system_routes(
    router: Router,
    policy: crate::middleware::auth::GatewayRoutePolicy,
) -> Router {
    use crate::middleware::auth::AuthRequirement;
    router.layer(axum::middleware::from_fn(
        move |mut request: axum::extract::Request, next: axum::middleware::Next| {
            let requirement = policy.requirement_of(&request);
            async move {
                match requirement {
                    AuthRequirement::Required => return starting_response(),
                    AuthRequirement::None => {
                        request
                            .extensions_mut()
                            .insert(toolkit_security::SecurityContext::anonymous());
                    }
                    AuthRequirement::Platform => {}
                }
                next.run(request).await
            }
        },
    ))
}
