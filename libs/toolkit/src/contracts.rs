use async_trait::async_trait;
use axum::Router;
use tokio_util::sync::CancellationToken;

pub use crate::api::openapi_registry::OpenApiRegistry;

/// System capability: the gear belongs to the **system group**.
///
/// Only the platform gears the application group's preparation needs carry it:
/// the types registry, service discovery, cluster, and the REST and gRPC
/// hosts. The system group runs its whole startup (`pre_init`, migrations,
/// `init`, wiring, `post_init`, transport, `start`) before the application
/// group is prepared, so it serves while the application group waits for its
/// GTS admission. A system gear therefore:
///
/// - does not publish GTS documents with `gts(...)`;
/// - does not depend on (`deps`) or consume a local application gear;
/// - serves its bootstrap routes with platform-plane auth only (public probes
///   aside), since the tenant plane is not wired yet.
///
/// The general lifecycle hooks live on [`Gear`]; this trait only carries the
/// runtime internals the platform gears are wired with.
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

    /// **Legacy, temporary**: runs once, after every application `init` and
    /// the remaining proxy wiring, before the application `post_init`.
    ///
    /// Exists only for the types registry's in-memory ready mode, which
    /// validates what every gear registered during `init` in one commit. It
    /// goes away with that ready mode; nothing else should implement it.
    async fn legacy_finalize_after_application_init(
        &self,
        _sys: &crate::runtime::SystemContext,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Core gear: lifecycle hooks and DI/wiring.
///
/// Every gear passes through `pre_init`, `init` and `post_init` once, within
/// its startup group (the system group first, then the application group):
///
/// 1. [`pre_init`](Self::pre_init), before the group's DB migrations;
/// 2. [`init`](Self::init), after the group's migrations (and, in the
///    application group, after the GTS admission of every publication);
/// 3. [`post_init`](Self::post_init), after every `init` of the group and the
///    proxy wiring of its consumed clients, before its routes and `start`.
///
/// For a gear with a configured database handle, DB migrations have already
/// run when `init` is called, so such a gear may rely on its own migrated
/// schema there. Gears without a configured database get no such guarantee.
/// Use `init` to build the gear's own state and register its own client(s).
/// Do **not** call other gears there: ordinary consumed-contract clients
/// (`#[toolkit::consumes]`) are wired *after* all `init`s of the group
/// complete, so resolve them lazily on first use or in `post_init`. The one
/// exception is the registry clients the application group's GTS publication
/// needs, wired before its `init`.
#[async_trait]
pub trait Gear: Send + Sync + 'static {
    /// Optional hook before the DB migrations of the gear's group; also run
    /// in migration-only mode.
    ///
    /// Its context carries no database handle and no admitted GTS manifest:
    /// nothing is migrated or published yet, and no other gear of the group
    /// has initialized, so it must not rely on any provider of its group.
    /// Default: nothing.
    ///
    /// # Errors
    /// Returns an error to fail startup.
    async fn pre_init(&self, _ctx: &crate::context::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }

    async fn init(&self, ctx: &crate::context::GearCtx) -> anyhow::Result<()>;

    /// Optional hook after every `init` of the gear's group and the proxy
    /// wiring of its consumed clients, before the group's routes and `start`.
    /// In the application group it also follows the legacy registry
    /// finalization.
    ///
    /// It must not wait for a remote service: the group's listeners are not
    /// serving its routes yet. Default: nothing.
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

/// API Gateway capability: handles gateway hosting with prepare/finalize phases.
/// Must be sync. Runs during REST phase, but doesn't start the server.
#[allow(dead_code)]
pub trait ApiGatewayCapability: Send + Sync + 'static {
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

    /// Serve the routes of the system gears before the application
    /// group is prepared: probes, and platform-plane calls with the
    /// platform credential; every other request gets `503`. Called once,
    /// after [`rest_prepare`](Self::rest_prepare) and before the host starts;
    /// [`rest_finalize`](Self::rest_finalize) later replaces these routes on
    /// the same listener with the full surface.
    ///
    /// A host that cannot replace its routes while serving cannot be a
    /// system gear; the default refuses.
    ///
    /// # Errors
    /// The host does not support early serving, or the routes do not build.
    fn rest_bootstrap(
        &self,
        _ctx: &crate::context::GearCtx,
        _router: Router,
    ) -> anyhow::Result<()> {
        anyhow::bail!("this REST host cannot serve routes before the application group starts")
    }

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

/// Represents a gRPC service registration callback used by the gRPC hub.
///
/// Each gear that exposes gRPC services provides one or more of these.
/// The `register` closure adds the service into the provided `RoutesBuilder`.
#[cfg(feature = "otel")]
pub struct RegisterGrpcServiceFn {
    pub service_name: &'static str,
    pub register: Box<dyn Fn(&mut tonic::service::RoutesBuilder) + Send + Sync>,
}

#[cfg(not(feature = "otel"))]
pub struct RegisterGrpcServiceFn {
    pub service_name: &'static str,
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
