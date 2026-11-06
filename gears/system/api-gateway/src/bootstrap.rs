//! One listener serves probes, system routes, then the complete API.
//! Each router receives its auth layers before publication.

use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use axum::{
    Router,
    response::{IntoResponse as _, Response},
};
use parking_lot::Mutex;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

/// Owns the early listener and the routers published on it during startup.
#[derive(Default)]
pub(super) struct BootstrapServer {
    routes: Arc<ArcSwap<Router>>,
    task: Mutex<Option<tokio_util::task::AbortOnDropHandle<anyhow::Result<()>>>>,
    cancel: OnceLock<CancellationToken>,
}

impl BootstrapServer {
    /// Materialize a router once before making it visible to incoming requests.
    pub(super) fn publish(&self, router: Router) {
        self.routes.store(Arc::new(router.with_state(())));
    }

    /// Present once the listener has started; also owns startup middleware tasks.
    pub(super) fn cancellation_token(&self) -> Option<&CancellationToken> {
        self.cancel.get()
    }

    /// Serve the bound main and optional health listeners on the published router.
    pub(super) fn start(
        &self,
        listener: TcpListener,
        health_bound: Option<(TcpListener, Router)>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let live = Arc::clone(&self.routes);
        let outer = Router::new().fallback_service(tower::service_fn(move |request| {
            let router = live.load_full();
            async move { router.as_ref().clone().oneshot(request).await }
        }));
        let stop = cancel.child_token();
        self.cancel
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
        *self.task.lock() = Some(tokio_util::task::AbortOnDropHandle::new(task));
        Ok(())
    }

    /// Transfer the early server task to the gateway's running lifecycle.
    pub(super) async fn serve(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let mut task = self
            .task
            .lock()
            .take()
            .ok_or_else(|| anyhow::anyhow!("REST bootstrap server is missing"))?;
        tokio::select! {
            result = &mut task => result?,
            () = cancel.cancelled() => {
                if let Some(stop) = self.cancel.get() {
                    stop.cancel();
                }
                task.await?
            }
        }
    }

    /// Stop and join a server whose task has not been taken by the lifecycle.
    pub(super) async fn shutdown(&self) -> anyhow::Result<()> {
        if let Some(stop) = self.cancel.get() {
            stop.cancel();
        }
        let task = self.task.lock().take();
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
