//! The admission-control gear: wiring and lifecycle.
//!
//! - **init**: loads and validates `gears.admission-control.config` (an
//!   invalid one fails startup), registers the refusal event type (a rejection
//!   fails startup; an unreachable registry is retried in the background),
//!   and registers `dyn AdmissionClientV1` in `ClientHub` with a configured
//!   engine that resolves on first use, so a call that arrives before the
//!   serve phase reaches the engine instead of finding none.
//! - **serve**: resolves the engine once eagerly (every plugin has finished
//!   `init` by now; a configured engine that does not resolve fails startup),
//!   starts the publisher task and signals ready. Readiness is "serving".
//!
//! Refusal events are published under the gate's own identity
//! ([`GATE_SUBJECT_ID`]); a caller's context is never used to publish.

use std::sync::{Arc, Mutex, OnceLock};

use admission_control_sdk::AdmissionClientV1;
use anyhow::Context as _;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::lifecycle::ReadySignal;
use toolkit::{ClientHub, Gear, GearCtx};
use toolkit_security::SecurityContext;
use toolkit_security::constants::DEFAULT_TENANT_ID;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use crate::config::AdmissionControlConfig;
use crate::domain::local_client::AdmissionLocalClient;
use crate::domain::service::{AdmissionMetrics, AdmissionService, EngineSource};
use crate::infra::engine::LazyEngine;
use crate::infra::metrics::AdmissionControlMetrics;
use crate::infra::publisher::{QueuePublisher, QueuedEvent, register_event_type, run};

/// Subject identifier the gate publishes its events under: a hand-picked
/// constant (version nibble 0, so it cannot collide with a v4/v5 identity).
pub const GATE_SUBJECT_ID: Uuid = uuid::uuid!("00000000-0000-cf01-0000-61646d63746c");

/// Subject type of the gate's own publishing identity.
pub const GATE_SUBJECT_TYPE: &str = "admission_control.system";

/// Token scope the gate's publishing identity presents: the first-party
/// wildcard. The authorization resolver's token-scope enforcer refuses an
/// empty `token_scopes` before RBAC is consulted, which would drop every
/// refusal event at the broker. It widens nothing: what the gate may publish
/// is still RBAC's to decide, through the grant held by [`GATE_SUBJECT_ID`].
/// `settings-service` and `keycloak-idp-plugin` present the same scope for
/// the same reason.
pub const GATE_TOKEN_SCOPE: &str = "*";

/// Everything `init` wires, consumed by `serve`.
struct Wired {
    service: Arc<AdmissionService>,
    registry: Arc<dyn TypesRegistryClient>,
    hub: Arc<ClientHub>,
    engine: Option<Arc<LazyEngine>>,
    events: Mutex<Option<mpsc::Receiver<QueuedEvent>>>,
    registered: bool,
    metrics: Arc<AdmissionControlMetrics>,
}

/// The admission-control gear. See the module documentation.
#[toolkit::gear(
    name = "admission-control",
    deps = [types_registry],
    capabilities = [stateful],
    lifecycle(entry = "serve", stop_timeout = "10s", await_ready)
)]
#[derive(Default)]
pub struct AdmissionControl {
    wired: OnceLock<Wired>,
}

impl std::fmt::Debug for AdmissionControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionControl")
            .field("initialised", &self.wired.get().is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Gear for AdmissionControl {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config: AdmissionControlConfig = ctx
            .config_or_default()
            .context("admission-control: invalid configuration")?;
        config
            .validate()
            .context("admission-control: invalid configuration")?;

        let hub = ctx.client_hub();
        let registry = hub.get::<dyn TypesRegistryClient>().map_err(|e| {
            anyhow::anyhow!("admission-control: types-registry client unavailable: {e}")
        })?;
        let registered = register_event_type(registry.as_ref())
            .await
            .context("admission-control: refusal event type registration rejected")?;

        let metrics = Arc::new(AdmissionControlMetrics::global());
        let (publisher, events) =
            QueuePublisher::new(config.event_queue_capacity, Arc::clone(&metrics));
        let engine = config.engine.clone().map(|selection| {
            Arc::new(LazyEngine::new(
                Arc::clone(&hub),
                Arc::clone(&registry),
                selection,
            ))
        });
        let service = Arc::new(AdmissionService::new(
            config.service_settings(),
            engine.clone().map(|engine| engine as Arc<dyn EngineSource>),
            Arc::new(publisher),
            Arc::clone(&metrics) as Arc<dyn AdmissionMetrics>,
        ));

        let wired = Wired {
            service: Arc::clone(&service),
            registry,
            hub: Arc::clone(&hub),
            engine,
            events: Mutex::new(Some(events)),
            registered,
            metrics,
        };
        self.wired
            .set(wired)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        hub.register::<dyn AdmissionClientV1>(Arc::new(AdmissionLocalClient::new(service)));
        tracing::info!(
            engine_configured = config.engine.is_some(),
            "admission-control initialised"
        );
        Ok(())
    }
}

impl AdmissionControl {
    /// The admission service, once `init` has run. Exposed for tests.
    #[doc(hidden)]
    #[must_use]
    pub fn admission_service(&self) -> Option<&Arc<AdmissionService>> {
        self.wired.get().map(|wired| &wired.service)
    }

    /// The serve phase. See the module documentation.
    ///
    /// # Errors
    ///
    /// When `init` has not run, or when a configured engine cannot be
    /// resolved: startup fails.
    pub async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let wired = self
            .wired
            .get()
            .ok_or_else(|| anyhow::anyhow!("{}: not initialised", Self::MODULE_NAME))?;
        check_engine(wired.engine.as_deref()).await?;

        let events = wired
            .events
            .lock()
            .map_err(|_| anyhow::anyhow!("admission-control: event queue lock poisoned"))?
            .take()
            .ok_or_else(|| anyhow::anyhow!("admission-control: already serving"))?;
        let publisher = tokio::spawn(run(
            events,
            Arc::clone(&wired.hub),
            Arc::clone(&wired.registry),
            gate_identity()?,
            wired.registered,
            Arc::clone(&wired.metrics),
            cancel.child_token(),
        ));
        ready.notify();

        cancel.cancelled().await;
        if let Err(error) = publisher.await {
            tracing::error!(error = %error, "admission-control: publisher task failed");
        }
        Ok(())
    }
}

/// Resolves the configured engine once, eagerly: one that cannot be
/// resolved fails startup. Calls before this resolve it themselves.
async fn check_engine(engine: Option<&LazyEngine>) -> anyhow::Result<()> {
    let Some(engine) = engine else {
        tracing::warn!("admission-control: no engine configured; refusing every admission request");
        return Ok(());
    };
    let resolved = engine
        .resolve()
        .await
        .context("admission-control: configured engine cannot be resolved")?;
    tracing::info!(engine_id = %resolved.id, "admission engine resolved");
    Ok(())
}

/// The gate's own publishing identity. See [`GATE_SUBJECT_ID`].
///
/// # Errors
///
/// When the context cannot be built (both identifiers are constants, so this
/// does not happen in practice; it fails startup rather than panicking).
pub fn gate_identity() -> anyhow::Result<SecurityContext> {
    SecurityContext::builder()
        .subject_id(GATE_SUBJECT_ID)
        .subject_type(GATE_SUBJECT_TYPE)
        .subject_tenant_id(DEFAULT_TENANT_ID)
        .token_scopes(vec![GATE_TOKEN_SCOPE.to_owned()])
        .build()
        .context("admission-control: gate identity could not be built")
}
