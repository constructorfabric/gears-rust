use async_trait::async_trait;
use axum::Router;
use tokio_util::sync::CancellationToken;

pub use crate::api::openapi_registry::OpenApiRegistry;
pub use crate::grpc_registration::RegisterGrpcServiceFn;

/// Infrastructure that starts before application preparation: types registry,
/// service discovery, cluster and gRPC hub.
///
/// The system group completes startup before application preparation and serves
/// during GTS admission. System gears cannot publish via `gts_publication(...)`
/// or depend on/consume local application gears. Early `OoP` HTTP routes allow
/// only platform-plane and anonymous calls; tenant auth is not wired yet.
///
/// Embedded hosts serve probes and system platform routes before application startup.
/// Lifecycle hooks live on [`Gear`]; this trait receives runtime internals.
#[async_trait]
pub trait SystemCapability: Send + Sync {
    /// Receive runtime internals, once, before the gear's `pre_init`.
    ///
    /// Service discovery takes the gear manager; the gRPC hub takes the
    /// installer store and the process instance id. Default: nothing.
    ///
    /// # Errors
    /// Returns an error if the gear cannot take the runtime internals.
    fn wire_runtime(&self, _sys: &crate::runtime::SystemContext) -> anyhow::Result<()> {
        Ok(())
    }

    /// Temporary hook for the legacy registry's in-memory ready mode only.
    /// Validates all legacy registrations once, after application `init` and proxy
    /// wiring, before application `post_init`. Removed with that ready mode.
    async fn legacy_finalize_after_application_init(
        &self,
        _sys: &crate::runtime::SystemContext,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Core lifecycle and dependency wiring, once per gear within its startup group.
/// The system group completes before the application group begins.
///
/// 1. [`pre_init`](Self::pre_init), before the group's DB migrations;
/// 2. [`init`](Self::init), after migrations and, for applications, GTS admission;
/// 3. [`post_init`](Self::post_init), after all group `init`s and consumed-client
///    wiring, before routes and `start`.
///
/// `init` can use its configured, migrated database and register local clients.
/// Ordinary `#[toolkit::consumes]` clients may be absent until proxy wiring;
/// resolve them in `post_init` or lazily. System-provider and publication registry
/// clients are wired earlier; `deps` can order local client registration.
#[async_trait]
pub trait Gear: Send + Sync + 'static {
    /// Optional hook before this group's migrations; also runs in migration-only mode.
    /// The context has no DB handle or admitted manifest. No gear in this group has
    /// initialized yet, so do not rely on its providers. Default: no-op.
    ///
    /// # Errors
    /// Returns an error to fail startup.
    async fn pre_init(&self, _ctx: &crate::context::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }

    async fn init(&self, ctx: &crate::context::GearCtx) -> anyhow::Result<()>;

    /// Optional hook after the group's `init`s and proxy wiring, before routes and
    /// `start`. Application hooks also follow legacy registry finalization.
    /// Do not wait for remote services: this group's routes are not served yet.
    /// Default: no-op.
    ///
    /// # Errors
    /// Returns an error to fail startup.
    async fn post_init(&self, _ctx: &crate::context::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Database capability: gears provide migrations, runtime executes them.
///
/// # Security
///
/// Gears MUST NOT receive raw database connections. They only return migration definitions.
#[cfg(feature = "db")]
pub trait DatabaseCapability: Send + Sync {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>>;
}

/// REST API capability: Pure wiring; must be sync. Runs AFTER DB migrations.
pub trait RestApiCapability: Send + Sync {
    /// Register REST routes for this gear.
    ///
    /// # Errors
    /// Returns an error if route registration fails.
    fn register_rest(
        &self,
        ctx: &crate::context::GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router>;

    /// Readiness healthcheck for this gear, run on every `/readyz` and `/health`.
    /// `None` (default) opts out.
    ///
    /// Return **one composite check per gear** that aggregates the gear's critical
    /// dependencies, rather than many fine-grained probes — the aggregate is what decides
    /// whether the pod stays in traffic. External SaaS/LLM dependencies should usually **not**
    /// gate readiness: a provider outage would evict every pod from rotation and take the
    /// whole service down. Report such dependencies as `degraded` (kept in rotation), reserving
    /// `unhealthy` for failures the gear itself cannot serve through.
    fn healthcheck(
        &self,
        _ctx: &crate::context::GearCtx,
    ) -> Option<std::sync::Arc<dyn crate::healthcheck::Healthcheck>> {
        None
    }

    /// Whether the in-process host advertises this gear as a REST provider in the
    /// service directory, under its gear name. Asked after `init`. `false` keeps a gear
    /// that serves only part of its contracts here from being discovered as their
    /// provider; its routes are still served. Defaults to `true`.
    fn advertised_in_directory(&self) -> bool {
        true
    }
}

/// REST hosting with optional early probes and system routes during preparation.
#[allow(dead_code)]
#[async_trait]
pub trait ApiGatewayCapability: Send + Sync + 'static {
    /// Bind probes without application clients; return a prepared router for staged hosting.
    /// System routes attach first, then application routes. Stop on `cancel`, even if init fails.
    async fn rest_bootstrap(
        &self,
        _ctx: &crate::context::GearCtx,
        _hc_registry: std::sync::Arc<crate::healthcheck::RestHealthcheckRegistry>,
        _cancel: CancellationToken,
    ) -> anyhow::Result<Option<Router>> {
        Ok(None)
    }

    /// Serve system routes during admission; reject tenant calls until `AuthN` is ready.
    ///
    /// # Errors
    /// Staged serving is unsupported or composition fails.
    fn rest_finalize_bootstrap(
        &self,
        _ctx: &crate::context::GearCtx,
        _router: Router,
        _hc_registry: std::sync::Arc<crate::healthcheck::RestHealthcheckRegistry>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("this REST host does not support staged serving")
    }

    /// Join a bootstrap listener whose application lifecycle never started.
    async fn rest_shutdown_bootstrap(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Prepare a base Router (e.g., global middlewares) and optionally touch `OpenAPI` meta.
    /// Do NOT start the server here. `hc_registry` is the runtime's shared healthcheck
    /// registry, passed explicitly (not via `ClientHub`, which is for inter-gear clients).
    ///
    /// # Errors
    /// Returns an error if router preparation fails.
    fn rest_prepare(
        &self,
        ctx: &crate::context::GearCtx,
        router: Router,
        hc_registry: std::sync::Arc<crate::healthcheck::RestHealthcheckRegistry>,
    ) -> anyhow::Result<Router>;

    /// Finalize before start: attach /openapi.json, /docs, health endpoints, persist the
    /// Router internally if needed. Do NOT start the server here.
    ///
    /// `hc_registry` is the same registry instance passed to [`rest_prepare`](Self::rest_prepare).
    ///
    /// # Errors
    /// Returns an error if router finalization fails.
    fn rest_finalize(
        &self,
        ctx: &crate::context::GearCtx,
        router: Router,
        hc_registry: std::sync::Arc<crate::healthcheck::RestHealthcheckRegistry>,
    ) -> anyhow::Result<Router>;

    // Return OpenAPI registry of the gear, e.g., to register endpoints
    fn as_registry(&self) -> &dyn OpenApiRegistry;

    /// The bound REST base URL (e.g. `http://127.0.0.1:8080`) once the gateway's
    /// server is listening, or `None` before the listener binds.
    ///
    /// Mirrors [`GrpcHubCapability::bound_endpoint`]. The runtime polls this in
    /// its directory-register phase to advertise REST providers in the service
    /// directory. Defaults to `None` for hosts that don't expose an address.
    fn bound_endpoint(&self) -> Option<String> {
        None
    }
}

/// Capability for gears that have a long-running background task.
///
/// # Shutdown Contract
///
/// The `stop` method receives a **deadline token** that implements two-phase shutdown:
///
/// 1. **Graceful stop request**: When `stop(deadline_token)` is called, the `deadline_token`
///    is *not* cancelled. This is the signal to begin graceful shutdown.
///
/// 2. **Hard-stop deadline**: After the runtime's `shutdown_deadline` expires (default 35s),
///    the `deadline_token` is cancelled. This signals that graceful shutdown time is over
///    and the gear should abort immediately.
///
/// ## Recommended Implementation Pattern
///
/// ```ignore
/// async fn stop(&self, deadline_token: CancellationToken) -> anyhow::Result<()> {
///     // 1. Request cooperative shutdown of child tasks
///     self.request_graceful_shutdown();
///
///     // 2. Wait for graceful completion OR hard-stop deadline
///     tokio::select! {
///         _ = self.wait_for_graceful_completion() => {
///             // Graceful shutdown succeeded
///         }
///         _ = deadline_token.cancelled() => {
///             // Deadline reached, force abort
///             self.force_abort();
///         }
///     }
///     Ok(())
/// }
/// ```
///
/// ## Important Notes
///
/// - The `deadline_token` passed to `stop()` is a **fresh token**, not the root cancellation
///   token that triggered the shutdown. This allows gears to implement real graceful shutdown.
/// - Gears should NOT assume the token is already cancelled when `stop()` is called.
/// - The `WithLifecycle` wrapper handles this contract automatically via its `stop_timeout`.
#[async_trait]
pub trait RunnableCapability: Send + Sync {
    /// Start the gear's background task.
    ///
    /// The `cancel` token is a child of the runtime's root cancellation token.
    /// When cancelled, the gear should stop its background work.
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()>;

    /// Stop the gear's background task.
    ///
    /// The `deadline_token` implements two-phase shutdown:
    /// - Initially not cancelled: begin graceful shutdown
    /// - When cancelled: graceful period expired, abort immediately
    ///
    /// See trait-level documentation for the full shutdown contract.
    async fn stop(&self, deadline_token: CancellationToken) -> anyhow::Result<()>;
}

/// gRPC Service capability: gears that export gRPC services.
///
/// The runtime will call this during the gRPC registration phase to collect
/// all services that should be exposed on the shared gRPC server.
#[async_trait]
pub trait GrpcServiceCapability: Send + Sync {
    /// Returns all gRPC services this gear wants to expose.
    ///
    /// Each installer adds one service to the `tonic::Server` builder.
    async fn get_grpc_services(
        &self,
        ctx: &crate::context::GearCtx,
    ) -> anyhow::Result<Vec<RegisterGrpcServiceFn>>;
}

/// gRPC Hub capability: hosts the gRPC server.
///
/// This trait is implemented by the single gear responsible for hosting
/// the `tonic::Server` instance. Only one gear per process should implement this.
pub trait GrpcHubCapability: Send + Sync {
    /// Returns the bound endpoint after the server starts listening.
    ///
    /// Examples:
    /// - TCP: `http://127.0.0.1:50652`
    /// - Unix socket: `unix:///path/to/socket`
    /// - Named pipe: `pipe://\\.\pipe\name`
    ///
    /// Returns `None` if the server hasn't started listening yet.
    fn bound_endpoint(&self) -> Option<String>;

    /// Whether the hub has settled on binding nothing (`listen_addr: none`, or
    /// no services to serve), so [`bound_endpoint`](Self::bound_endpoint)
    /// stays `None` for good.
    fn serves_no_endpoint(&self) -> bool {
        false
    }
}
