//! gRPC Hub Gear definition
//!
//! Contains the `GrpcHub` gear struct and its trait implementations.

use anyhow::Context;
use async_trait::async_trait;
use toolkit::{
    DirectoryClient,
    client_hub::ClientHub,
    context::GearCtx,
    contracts::{Gear, SystemCapability},
    lifecycle::ReadySignal,
    runtime::{GearInstallers, GrpcInstallerData, GrpcInstallerStore},
};

use parking_lot::RwLock;
use serde::Deserialize;
#[cfg(unix)]
use std::path::PathBuf;
use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, OnceLock},
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::{service::RoutesBuilder, transport::Server};

use toolkit_security::{DynInternalAuthenticator, InternalAuthConfig};
use toolkit_transport_grpc::{InternalAuthEnforcement, InternalAuthGrpcLayer};

#[cfg(windows)]
use toolkit_transport_grpc::create_named_pipe_incoming;

const DEFAULT_LISTEN_ADDR: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 50051));

/// Default seconds a positive platform-plane validation result is cached.
/// Derived from `toolkit_security::DEFAULT_TOKEN_REVIEW_CACHE_TTL` (not just
/// mirrored as a literal) so the config surface cannot drift from the shared
/// cache implementation while still not depending on the optional `k8s-auth`
/// feature.
const DEFAULT_INTERNAL_AUTH_CACHE_TTL_SECS: u64 =
    toolkit_security::DEFAULT_TOKEN_REVIEW_CACHE_TTL.as_secs();
/// Upper bound on the internal-auth cache TTL. Caps how long a revoked or
/// expired token can keep validating from cache, so a misconfiguration cannot
/// widen the revocation window unboundedly. Derived from
/// `toolkit_security::MAX_TOKEN_REVIEW_CACHE_TTL` for the same reason.
const MAX_INTERNAL_AUTH_CACHE_TTL_SECS: u64 =
    toolkit_security::MAX_TOKEN_REVIEW_CACHE_TTL.as_secs();

/// Configuration for the gRPC Hub gear.
///
/// Supports multiple transport types via `listen_addr`:
/// - TCP: `"127.0.0.1:50051"` or `"0.0.0.0:0"` for ephemeral port
/// - Unix Domain Socket (Unix only): `"uds:///path/to/socket.sock"`
/// - Named Pipe (Windows only): `"pipe://\\.\pipe\my_pipe"` or `"npipe://\\.\pipe\my_pipe"`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GrpcHubConfig {
    /// Listen address for the gRPC server.
    /// Defaults to `0.0.0.0:50051` if not specified.
    pub listen_addr: String,

    /// The address that gRPC Hub advertises to the directory service for discovery.
    ///
    /// Accepted forms:
    /// - `host:<u16>` — literal host and port (`:0` is treated as "use the
    ///   actual bound port").
    /// - `host` (no `:`) — the actual bound port is appended at serve time.
    ///
    /// The address is parsed and validated during gear `init`; an
    /// unparsable port segment (e.g. `host:abc`) causes `init` to fail.
    pub advertise_addr: Option<String>,

    /// Platform-plane (internal) authentication for **all** inbound gRPC RPCs
    /// served by this hub. When set, an [`InternalAuthGrpcLayer`] validates the
    /// `x-toolkit-internal-token` on every non-exempt RPC
    /// (`cpt-cf-adr-platform-plane-auth`); when absent, enforcement is disabled
    /// (Profile 1 / in-process only).
    ///
    /// Mirrors the REST `oop_http.internal_auth` surface: a single flat
    /// `InternalAuthConfig` provider, with the gRPC-only enforcement / exempt /
    /// cache knobs as sibling fields below.
    pub internal_auth: Option<InternalAuthConfig>,

    /// How an **absent** internal credential is treated when `internal_auth` is
    /// set. Defaults to [`InternalAuthEnforcement::Required`]. Has no effect
    /// when `internal_auth` is `None`. (gRPC-only: the REST middleware is always
    /// permissive.)
    pub internal_auth_enforcement: InternalAuthEnforcement,

    /// Optional override of the gRPC method-path prefixes exempt from
    /// enforcement. When `None`, the transport default
    /// (`toolkit_transport_grpc::DEFAULT_EXEMPT_PREFIXES` — health + reflection)
    /// applies. An empty vector enforces on every method.
    pub internal_auth_exempt_methods: Option<Vec<String>>,

    /// Seconds a **successful** platform-plane validation is cached, collapsing a
    /// burst of calls carrying the same token into a single validation-backend
    /// round-trip. Only positive results are cached, and only providers that make
    /// a remote call benefit — in practice `kube` (the shared-secret provider is
    /// a local comparison and is never wrapped). `0` disables caching (every call
    /// re-validates). Bounded at boot by [`MAX_INTERNAL_AUTH_CACHE_TTL_SECS`] to
    /// keep the token-revocation window tight.
    pub internal_auth_cache_ttl_secs: u64,
}

impl Default for GrpcHubConfig {
    fn default() -> Self {
        Self {
            listen_addr: DEFAULT_LISTEN_ADDR.to_string(),
            advertise_addr: None,
            internal_auth: None,
            internal_auth_enforcement: InternalAuthEnforcement::default(),
            internal_auth_exempt_methods: None,
            internal_auth_cache_ttl_secs: DEFAULT_INTERNAL_AUTH_CACHE_TTL_SECS,
        }
    }
}

/// Configuration for the listen address
#[derive(Clone)]
pub(crate) enum ListenConfig {
    Tcp(SocketAddr),
    #[cfg(unix)]
    Uds(PathBuf),
    #[cfg(windows)]
    NamedPipe(String),
}

/// Parse and validate a user-supplied advertise address.
///
/// # Errors
/// Returns an error when the input contains a `:` but the trailing
/// segment cannot be parsed as a `u16` (e.g. `"host:abc"`).
fn parse_advertise_addr(advertise_addr: &str) -> anyhow::Result<(String, Option<u16>)> {
    if let Some((host, port_str)) = advertise_addr.rsplit_once(':') {
        let port = port_str
            .parse::<u16>()
            .with_context(|| format!("invalid port in advertise_addr '{advertise_addr}'"))?;

        Ok((host.to_owned(), if port == 0 { None } else { Some(port) }))
    } else {
        Ok((advertise_addr.to_owned(), None))
    }
}

/// Build the inbound platform-plane validator from configuration.
///
/// The dependency-light shared-secret provider is built directly. The
/// Kubernetes `TokenReview` provider requires the `k8s-auth` feature and is
/// constructed (with the caching decision) by the single shared
/// `toolkit_k8s_auth::build_cached_k8s_authenticator` helper also used by the
/// `OoP` HTTP bootstrap, so the two never drift on what `provider: kube`
/// means or how it is cached. Without the feature, configuring `provider:
/// kube` is a hard error rather than a silent enforcement downgrade — as is
/// any other unrecognized provider value.
#[cfg_attr(
    not(feature = "k8s-auth"),
    allow(clippy::unused_async, unused_variables)
)]
async fn build_internal_authenticator(
    cfg: Option<&InternalAuthConfig>,
    cache_ttl_secs: u64,
) -> anyhow::Result<Option<DynInternalAuthenticator>> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };

    // Shared-secret (and any future dependency-light provider) builds here.
    if let Some(auth) = cfg.build_authenticator() {
        return Ok(Some(auth));
    }

    #[cfg(feature = "k8s-auth")]
    {
        if cfg.is_kube() {
            let audiences = cfg.kube_audiences().unwrap_or_default().to_vec();
            // `0` disables caching; any positive TTL wraps the validator in the
            // short-lived positive cache.
            let cache_ttl =
                (cache_ttl_secs > 0).then(|| std::time::Duration::from_secs(cache_ttl_secs));
            let auth = toolkit_k8s_auth::build_cached_k8s_authenticator(audiences, cache_ttl)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("failed to init Kubernetes TokenReview authenticator: {e}")
                })?;
            return Ok(Some(auth));
        }
    }
    #[cfg(not(feature = "k8s-auth"))]
    {
        if cfg.is_kube() {
            anyhow::bail!("grpc-hub internal_auth provider=kube requires the `k8s-auth` feature");
        }
    }

    anyhow::bail!(
        "internal_auth is configured but no authenticator could be built for the selected provider"
    )
}

/// Assemble the [`InternalAuthGrpcLayer`] from a built `authenticator` and the
/// configured enforcement mode / exempt-method override.
///
/// `authenticator` being `None` is a deliberate, explicit
/// [`InternalAuthGrpcLayer::disabled`] — never the fully-open configuration
/// reached merely by *forgetting* to supply one
/// (`cpt-cf-adr-platform-plane-auth`).
fn assemble_auth_layer(
    authenticator: Option<DynInternalAuthenticator>,
    enforcement: InternalAuthEnforcement,
    exempt_methods: Option<Vec<String>>,
) -> InternalAuthGrpcLayer {
    let mut layer = match authenticator {
        Some(authenticator) => InternalAuthGrpcLayer::new(authenticator),
        None => InternalAuthGrpcLayer::disabled(),
    }
    .with_enforcement(enforcement);
    if let Some(prefixes) = exempt_methods {
        layer = layer.with_exempt_prefixes(prefixes);
    }
    layer
}

/// Reject an exempt-method entry that could never match a gRPC method path:
/// an empty string matches every path via `starts_with`, and a prefix
/// missing the leading `/` can never match one at all.
///
/// # Errors
/// Returns an error naming the offending entry.
fn validate_exempt_method(entry: &str) -> anyhow::Result<()> {
    if entry.is_empty() || !entry.starts_with('/') {
        anyhow::bail!(
            "grpc-hub internal_auth_exempt_methods entry {entry:?} is invalid: must be \
             non-empty and start with '/'"
        );
    }
    Ok(())
}

/// The gRPC Hub gear.
/// This gear is responsible for hosting the gRPC server and managing the gRPC services.
#[toolkit::gear(
    name = "grpc-hub",
    capabilities = [stateful, system, grpc_hub],
    lifecycle(entry = "serve", await_ready)
)]
pub struct GrpcHub {
    pub(crate) listen_cfg: RwLock<ListenConfig>,
    pub(crate) advertise_addr: OnceLock<(String, Option<u16>)>,
    pub(crate) installer_store: OnceLock<Arc<GrpcInstallerStore>>,
    pub(crate) client_hub: OnceLock<Arc<ClientHub>>,
    pub(crate) instance_id: OnceLock<String>,
    pub(crate) bound_endpoint: RwLock<Option<String>>,
    /// Platform-plane middleware applied to every served gRPC RPC; a
    /// pass-through layer when `internal_auth` is unset.
    pub(crate) auth_layer: OnceLock<InternalAuthGrpcLayer>,
}

impl Default for GrpcHub {
    fn default() -> Self {
        Self {
            listen_cfg: RwLock::new(ListenConfig::Tcp(DEFAULT_LISTEN_ADDR)),
            advertise_addr: OnceLock::new(),
            installer_store: OnceLock::new(),
            client_hub: OnceLock::new(),
            instance_id: OnceLock::new(),
            bound_endpoint: RwLock::new(None),
            auth_layer: OnceLock::new(),
        }
    }
}

impl GrpcHub {
    /// Update the listen address to TCP (primarily used by tests/config).
    pub fn set_listen_addr_tcp(&self, addr: SocketAddr) {
        *self.listen_cfg.write() = ListenConfig::Tcp(addr);
    }

    /// Current TCP listen address (returns None if using UDS or named pipe).
    pub fn listen_addr_tcp(&self) -> Option<SocketAddr> {
        match *self.listen_cfg.read() {
            ListenConfig::Tcp(addr) => Some(addr),
            #[cfg(unix)]
            ListenConfig::Uds(_) => None,
            #[cfg(windows)]
            ListenConfig::NamedPipe(_) => None,
        }
    }

    /// Set listen address to Windows named pipe (primarily used by tests).
    #[cfg(windows)]
    pub fn set_listen_named_pipe(&self, name: impl Into<String>) {
        *self.listen_cfg.write() = ListenConfig::NamedPipe(name.into());
    }

    /// Get the actual bound endpoint after the server has started.
    ///
    /// Returns the full endpoint URL (e.g., `http://127.0.0.1:50652` for TCP,
    /// `unix:///path/to/socket` for UDS, or `pipe://\\.\pipe\name` for named pipes).
    /// Returns `None` if the server hasn't started yet.
    fn get_bound_endpoint(&self) -> Option<String> {
        self.bound_endpoint.read().clone()
    }

    /// Set the bound endpoint after the server has started listening.
    fn set_bound_endpoint(&self, endpoint: String) {
        *self.bound_endpoint.write() = Some(endpoint);
    }

    /// The platform-plane middleware to install on the server.
    ///
    /// `init` always populates `auth_layer` — with an explicit
    /// [`InternalAuthGrpcLayer::disabled`] when `internal_auth` is unset, never
    /// a bare absence. An unset `auth_layer` here means `init` never ran, a
    /// startup-ordering bug: this refuses to serve rather than silently
    /// downgrading to a pass-through layer (this is the *only* inbound
    /// enforcement point, `cpt-cf-adr-platform-plane-auth`).
    ///
    /// # Errors
    /// Returns an error if `init` has not run.
    fn effective_auth_layer(&self) -> anyhow::Result<InternalAuthGrpcLayer> {
        self.auth_layer.get().cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "GrpcHub auth_layer not initialized: Gear::init must run before serving \
                 (refusing to serve with platform-plane enforcement undetermined)"
            )
        })
    }

    /// Pick the endpoint to register with Directory for a TCP listener.
    ///
    /// Returns `None` when the bound address is unspecified (e.g. `0.0.0.0`)
    /// and no explicit `advertise_addr` is configured — in that case the
    /// endpoint is not routable and registration must be skipped.
    fn tcp_directory_endpoint(&self, bound_addr: SocketAddr) -> Option<String> {
        if let Some((host, port)) = self.advertise_addr.get() {
            let resolved = format!("{}:{}", host, port.unwrap_or(bound_addr.port()));

            Some(format!("http://{resolved}"))
        } else if !bound_addr.ip().is_unspecified() {
            Some(format!("http://{bound_addr}"))
        } else {
            None
        }
    }

    /// Resolve `DirectoryClient` lazily from the stored `ClientHub`.
    /// Returns `None` if no `DirectoryClient` has been registered.
    fn resolve_directory_client(&self) -> Option<Arc<dyn DirectoryClient>> {
        self.client_hub
            .get()
            .and_then(|hub| hub.get::<dyn DirectoryClient>().ok())
    }

    /// Parse and apply listen address configuration.
    ///
    /// Supports:
    /// - TCP: `"127.0.0.1:50051"` or `"0.0.0.0:0"` for ephemeral port
    /// - Unix Domain Socket (Unix only): `"uds:///path/to/socket.sock"`
    /// - Named Pipe (Windows only): `"pipe://\\.\pipe\my_pipe"` or `"npipe://\\.\pipe\my_pipe"`
    ///
    /// # Errors
    /// Returns an error if the address format is invalid or unsupported on the platform.
    pub fn apply_listen_config(&self, listen_addr: &str) -> anyhow::Result<()> {
        // First, try platform-specific parsing
        if self.apply_platform_specific(listen_addr)? {
            return Ok(());
        }

        // Fall back to TCP SocketAddr parsing
        let addr = listen_addr
            .parse::<SocketAddr>()
            .with_context(|| format!("invalid listen_addr '{listen_addr}'"))?;
        *self.listen_cfg.write() = ListenConfig::Tcp(addr);
        tracing::info!(%addr, "gRPC hub listen address configured for TCP");

        Ok(())
    }

    /// Platform-specific address parsing.
    ///
    /// Returns `Ok(true)` if the address was fully handled by this method,
    /// `Ok(false)` if the caller should fall back to TCP parsing.
    #[cfg(windows)]
    fn apply_platform_specific(&self, listen_addr: &str) -> anyhow::Result<bool> {
        // Handle Windows named pipes: pipe:// or npipe://
        if let Some(pipe_name) = listen_addr
            .strip_prefix("pipe://")
            .or_else(|| listen_addr.strip_prefix("npipe://"))
        {
            let pipe_name = pipe_name.to_owned();
            *self.listen_cfg.write() = ListenConfig::NamedPipe(pipe_name.clone());
            tracing::info!(
                name = %pipe_name,
                "gRPC hub listen address configured for Windows named pipe"
            );
            return Ok(true);
        }

        // Explicitly reject UDS on Windows
        if listen_addr.starts_with("uds://") {
            anyhow::bail!("UDS listen_addr is not supported on Windows: '{listen_addr}'");
        }

        // Not a platform-specific address, fall back to TCP
        Ok(false)
    }

    /// Platform-specific address parsing.
    ///
    /// Returns `Ok(true)` if the address was fully handled by this method,
    /// `Ok(false)` if the caller should fall back to TCP parsing.
    #[cfg(unix)]
    fn apply_platform_specific(&self, listen_addr: &str) -> anyhow::Result<bool> {
        // Explicitly reject named pipes on Unix
        if listen_addr.starts_with("pipe://") || listen_addr.starts_with("npipe://") {
            tracing::warn!(
                listen_addr = %listen_addr,
                "Named pipe listen_addr is configured but named pipes are not supported on this platform"
            );
            anyhow::bail!(
                "Named pipe listen_addr is not supported on this platform: '{listen_addr}'"
            );
        }

        // Handle Unix Domain Sockets: uds://
        if let Some(uds_path) = listen_addr.strip_prefix("uds://") {
            let path = std::path::PathBuf::from(uds_path);
            *self.listen_cfg.write() = ListenConfig::Uds(path.clone());
            tracing::info!(
                path = %path.display(),
                "gRPC hub listen address configured for UDS"
            );
            return Ok(true);
        }

        // Not a platform-specific address, fall back to TCP
        Ok(false)
    }

    /// Validate that all service names are unique across all gears.
    fn validate_unique_services(gears: &[GearInstallers]) -> anyhow::Result<()> {
        let mut seen = HashSet::new();
        for gear in gears {
            for installer in &gear.installers {
                if !seen.insert(installer.service_name) {
                    anyhow::bail!(
                        "Duplicate gRPC service detected: {}",
                        installer.service_name
                    );
                }
            }
        }
        Ok(())
    }

    /// Build routes from gear installers. Returns None if no services registered.
    fn build_routes_from_gears(gears: &[GearInstallers]) -> Option<tonic::service::Routes> {
        let mut routes_builder = RoutesBuilder::default();
        let mut has_services = false;
        for gear in gears {
            for installer in &gear.installers {
                (installer.register)(&mut routes_builder);
                has_services = true;
            }
        }
        if has_services {
            Some(routes_builder.routes())
        } else {
            None
        }
    }

    /// Prepare Unix Domain Socket path by removing existing socket file if present.
    #[cfg(unix)]
    fn prepare_uds_socket_path(path: &std::path::Path) {
        use std::io;

        if !path.exists() {
            return;
        }

        match std::fs::remove_file(path) {
            Ok(()) => {
                tracing::debug!(
                    path = %path.display(),
                    "removed existing UDS socket file before bind"
                );
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to remove existing UDS socket file before bind"
                );
            }
        }
    }

    /// Deregister gears from Directory on shutdown.
    async fn deregister_gears(&self, gears: &[GearInstallers]) -> anyhow::Result<()> {
        let Some(directory) = self.resolve_directory_client() else {
            return Ok(());
        };

        let instance_id = self.instance_id.get().ok_or_else(|| {
            anyhow::anyhow!(
                "GrpcHub instance_id not set: SystemGear::pre_init must run before Directory deregistration"
            )
        })?;

        for gear_data in gears {
            if let Err(e) = directory
                .deregister_instance(&gear_data.gear_name, instance_id)
                .await
            {
                tracing::warn!(
                    gear =  %gear_data.gear_name,
                    error = %e,
                    "Failed to deregister gear from Directory"
                );
            }
        }

        Ok(())
    }

    /// Run the tonic server with the provided installers.
    ///
    /// # Errors
    /// Returns an error if server startup or execution fails.
    pub async fn run_with_installers(
        &self,
        data: GrpcInstallerData,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        Self::validate_unique_services(&data.gears)?;

        let Some(routes) = Self::build_routes_from_gears(&data.gears) else {
            ready.notify();
            cancel.cancelled().await;
            return Ok(());
        };

        let listen_cfg = self.listen_cfg.read().clone();
        let serve_result = match listen_cfg {
            ListenConfig::Tcp(addr) => {
                self.serve_tcp(addr, routes, &data.gears, cancel, ready)
                    .await
            }
            #[cfg(unix)]
            ListenConfig::Uds(path) => {
                self.serve_uds(path, routes, &data.gears, cancel, ready)
                    .await
            }
            #[cfg(windows)]
            ListenConfig::NamedPipe(ref pipe_name) => {
                self.serve_named_pipe(pipe_name.clone(), routes, &data.gears, cancel, ready)
                    .await
            }
        };

        self.deregister_gears(&data.gears).await?;
        serve_result
    }

    /// Serve gRPC over TCP with Directory registration.
    async fn serve_tcp(
        &self,
        addr: SocketAddr,
        routes: tonic::service::Routes,
        gears: &[GearInstallers],
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        let bound_addr = listener.local_addr()?;
        tracing::info!(%bound_addr, transport = "tcp", "gRPC hub listening");

        self.set_bound_endpoint(format!("http://{bound_addr}"));

        if let Some(endpoint) = self.tcp_directory_endpoint(bound_addr) {
            self.register_gears(gears, &endpoint).await?;
        } else {
            tracing::warn!(
                %bound_addr,
                "listen_addr is unspecified and no advertise_addr configured; skipping Directory registration"
            );
        }

        ready.notify();

        let incoming = TcpListenerStream::new(listener);
        Server::builder()
            .layer(self.effective_auth_layer()?)
            .add_routes(routes)
            .serve_with_incoming_shutdown(incoming, async move {
                cancel.cancelled().await;
            })
            .await?;
        Ok(())
    }

    /// Serve gRPC over Unix Domain Socket with Directory registration.
    #[cfg(unix)]
    async fn serve_uds(
        &self,
        path: std::path::PathBuf,
        routes: tonic::service::Routes,
        gears: &[GearInstallers],
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        use tokio::net::UnixListener;
        use tokio_stream::wrappers::UnixListenerStream;

        Self::prepare_uds_socket_path(&path);

        tracing::info!(
            path = %path.display(),
            transport = "uds",
            "gRPC hub listening"
        );

        let uds = UnixListener::bind(&path)
            .with_context(|| format!("failed to bind UDS listener at '{}'", path.display()))?;

        let endpoint = format!("unix://{}", path.display());
        self.set_bound_endpoint(endpoint.clone());
        self.register_gears(gears, &endpoint).await?;
        ready.notify();

        let incoming = UnixListenerStream::new(uds);
        Server::builder()
            .layer(self.effective_auth_layer()?)
            .add_routes(routes)
            .serve_with_incoming_shutdown(incoming, async move {
                cancel.cancelled().await;
            })
            .await?;
        Ok(())
    }

    /// Serve gRPC over Windows named pipe with Directory registration.
    #[cfg(windows)]
    async fn serve_named_pipe(
        &self,
        pipe_name: String,
        routes: tonic::service::Routes,
        gears: &[GearInstallers],
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        tracing::info!(name = %pipe_name, transport = "named_pipe", "gRPC hub listening");

        let endpoint = format!("pipe://{pipe_name}");
        self.set_bound_endpoint(endpoint.clone());
        self.register_gears(gears, &endpoint).await?;
        ready.notify();

        let incoming = create_named_pipe_incoming(pipe_name, cancel.clone());
        Server::builder()
            .layer(self.effective_auth_layer()?)
            .add_routes(routes)
            .serve_with_incoming_shutdown(incoming, async move {
                cancel.cancelled().await;
            })
            .await?;
        Ok(())
    }

    async fn register_gears(&self, gears: &[GearInstallers], endpoint: &str) -> anyhow::Result<()> {
        let Some(directory) = self.resolve_directory_client() else {
            tracing::info!("DirectoryClient not available; skipping Directory registration");
            return Ok(());
        };

        let instance_id = self.instance_id.get().ok_or_else(|| {
            anyhow::anyhow!(
                "GrpcHub instance_id not set: SystemGear::pre_init must run before Directory registration"
            )
        })?;

        {
            for gear_data in gears {
                let service_names: Vec<String> = gear_data
                    .installers
                    .iter()
                    .map(|i| i.service_name.to_owned())
                    .collect();

                let info = cf_system_sdks::directory::RegisterInstanceInfo::new(
                    gear_data.gear_name.clone(),
                    instance_id.clone(),
                )
                .with_grpc_services(
                    service_names
                        .iter()
                        .map(|n| {
                            (
                                n.clone(),
                                cf_system_sdks::directory::ServiceEndpoint::new(endpoint),
                            )
                        })
                        .collect(),
                )
                .with_version(env!("CARGO_PKG_VERSION"));

                directory.register_instance(info).await?;
                tracing::info!(
                    gear =  %gear_data.gear_name,
                    endpoint = %endpoint,
                    "Registered gear in Directory"
                );
            }
        }

        Ok(())
    }

    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let store = self
            .installer_store
            .get()
            .ok_or_else(|| anyhow::anyhow!("GrpcInstallerStore not wired into GrpcHub"))?;
        let data = store.take();

        let data = data.ok_or_else(|| anyhow::anyhow!("GrpcInstallerStore is empty"))?;

        self.run_with_installers(data, cancel, ready).await
    }
}

#[async_trait]
impl SystemCapability for GrpcHub {
    fn pre_init(&self, sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        self.installer_store
            .set(Arc::clone(&sys.grpc_installers))
            .map_err(|_| {
                anyhow::anyhow!("GrpcInstallerStore already set (pre_init called twice?)")
            })?;

        self.instance_id
            .set(sys.instance_id().to_string())
            .map_err(|_| anyhow::anyhow!("instance_id already set (pre_init called twice?)"))?;
        Ok(())
    }
}

impl toolkit::contracts::GrpcHubCapability for GrpcHub {
    fn bound_endpoint(&self) -> Option<String> {
        self.get_bound_endpoint()
    }
}

#[async_trait]
impl Gear for GrpcHub {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Load typed configuration
        let cfg: GrpcHubConfig = ctx.config_or_default()?;

        tracing::debug!(?cfg, "Loaded gRPC hub configuration");

        // Parse listen_addr into appropriate transport type
        self.apply_listen_config(&cfg.listen_addr)?;

        // Fail loud at init on an out-of-bound cache TTL rather than silently
        // running with a dangerously wide token-revocation window.
        if cfg.internal_auth_cache_ttl_secs > MAX_INTERNAL_AUTH_CACHE_TTL_SECS {
            anyhow::bail!(
                "grpc-hub internal_auth_cache_ttl_secs ({}) exceeds the maximum of {}s",
                cfg.internal_auth_cache_ttl_secs,
                MAX_INTERNAL_AUTH_CACHE_TTL_SECS
            );
        }

        // Fail loud on an exempt-method entry that could never match a path.
        if let Some(exempt_methods) = &cfg.internal_auth_exempt_methods {
            for entry in exempt_methods {
                validate_exempt_method(entry)?;
            }
        }

        // Build the inbound platform-plane middleware; an unset `internal_auth`
        // yields an explicit `InternalAuthGrpcLayer::disabled()` (Profile 1 /
        // in-process).
        let authenticator = build_internal_authenticator(
            cfg.internal_auth.as_ref(),
            cfg.internal_auth_cache_ttl_secs,
        )
        .await?;
        match (&authenticator, &cfg.internal_auth) {
            (Some(_), Some(internal_auth)) => {
                let provider = if internal_auth.is_kube() {
                    "kube"
                } else {
                    "shared_secret"
                };
                tracing::info!(
                    provider,
                    enforcement = ?cfg.internal_auth_enforcement,
                    cache_ttl_secs = cfg.internal_auth_cache_ttl_secs,
                    exempt_prefixes = ?cfg
                        .internal_auth_exempt_methods
                        .as_deref()
                        .unwrap_or(&[]),
                    "inbound gRPC platform-plane enforcement enabled"
                );
            }
            _ => {
                tracing::warn!(
                    "grpc-hub internal_auth is unset; every inbound gRPC RPC on this hub is \
                     UNAUTHENTICATED (expected only for Profile 1 / in-process deployments)"
                );
            }
        }
        let auth_layer = assemble_auth_layer(
            authenticator,
            cfg.internal_auth_enforcement,
            cfg.internal_auth_exempt_methods.clone(),
        );
        self.auth_layer
            .set(auth_layer)
            .map_err(|_| anyhow::anyhow!("auth_layer already set (init called twice?)"))?;

        if let Some(advertise_addr) = cfg.advertise_addr {
            let parsed = parse_advertise_addr(&advertise_addr)?;

            self.advertise_addr
                .set(parsed)
                .map_err(|_| anyhow::anyhow!("advertise_addr already set (init called twice?)"))?;
        }

        // Store ClientHub reference for lazy DirectoryClient resolution during serve phase.
        self.client_hub
            .set(ctx.client_hub())
            .map_err(|_| anyhow::anyhow!("ClientHub already set (init called twice?)"))?;

        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use http::{Request, Response};
    use std::{
        convert::Infallible,
        future,
        sync::Arc,
        task::{Context as TaskContext, Poll},
    };
    use tokio::time::{Duration, sleep};
    use tokio_util::sync::CancellationToken;
    use tonic::{body::Body, server::NamedService};
    use toolkit::contracts::Gear;
    use toolkit::lifecycle::ReadySignal;
    use toolkit::runtime::{GearInstallers, GrpcInstallerData, GrpcInstallerStore};
    use toolkit::{client_hub::ClientHub, config::ConfigProvider, context::GearCtx};
    use tower::Service;
    use uuid::Uuid;

    const SERVICE_A: &str = "grpc_hub.test.ServiceA";
    const SERVICE_B: &str = "grpc_hub.test.ServiceB";

    #[derive(Clone)]
    struct ServiceAImpl;

    #[derive(Clone)]
    struct ServiceBImpl;

    impl NamedService for ServiceAImpl {
        const NAME: &'static str = SERVICE_A;
    }

    impl NamedService for ServiceBImpl {
        const NAME: &'static str = SERVICE_B;
    }

    impl Service<Request<Body>> for ServiceAImpl {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            future::ready(Ok(Response::new(Body::empty())))
        }
    }

    impl Service<Request<Body>> for ServiceBImpl {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            future::ready(Ok(Response::new(Body::empty())))
        }
    }

    fn installer_a() -> toolkit::contracts::RegisterGrpcServiceFn {
        toolkit::contracts::RegisterGrpcServiceFn {
            service_name: SERVICE_A,
            register: Box::new(|routes| {
                routes.add_service(ServiceAImpl);
            }),
        }
    }

    fn installer_b() -> toolkit::contracts::RegisterGrpcServiceFn {
        toolkit::contracts::RegisterGrpcServiceFn {
            service_name: SERVICE_B,
            register: Box::new(|routes| {
                routes.add_service(ServiceBImpl);
            }),
        }
    }

    #[tokio::test]
    async fn build_internal_authenticator_passes_through_when_unconfigured() {
        let auth = build_internal_authenticator(None, 30).await.unwrap();
        assert!(auth.is_none());
    }

    #[tokio::test]
    async fn build_internal_authenticator_builds_shared_secret() {
        let cfg = InternalAuthConfig::SharedSecret {
            secret: "test-secret".to_owned(),
            peer_name: "test-peer".to_owned(),
        };
        let auth = build_internal_authenticator(Some(&cfg), 30).await.unwrap();
        assert!(auth.is_some());
    }

    #[tokio::test]
    #[cfg(not(feature = "k8s-auth"))]
    async fn build_internal_authenticator_rejects_kube_without_feature() {
        let cfg = InternalAuthConfig::Kube {
            audiences: vec!["toolkit-internal".to_owned()],
            token_path: None,
        };
        let err = build_internal_authenticator(Some(&cfg), 30)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("k8s-auth"),
            "error should mention missing k8s-auth feature: {err}"
        );
    }

    #[test]
    fn validate_exempt_method_accepts_a_well_formed_prefix() {
        assert!(validate_exempt_method("/pkg.Svc/Method").is_ok());
    }

    #[test]
    fn assemble_auth_layer_applies_custom_exempt_prefixes() {
        // With a custom exempt list, the built-in health/reflection default
        // must no longer apply.
        let layer = assemble_auth_layer(
            None,
            InternalAuthEnforcement::Required,
            Some(vec!["/my.Svc/".to_owned()]),
        );
        let rendered = format!("{layer:?}");
        assert!(rendered.contains("my.Svc"));
        assert!(!rendered.contains("grpc.health"));
    }

    #[test]
    fn effective_auth_layer_errors_when_init_never_ran() {
        // `init` unconditionally populates `auth_layer`, so finding it unset
        // here means `init` never ran — a startup-ordering bug that must
        // refuse to serve rather than silently pass every RPC through.
        let hub = GrpcHub::default();
        let err = hub.effective_auth_layer().unwrap_err();
        assert!(
            err.to_string().contains("auth_layer not initialized"),
            "expected an initialization error, got {err}"
        );
    }

    #[test]
    fn test_advertise_addr_parse_and_resolve_substitutes_ephemeral_port() {
        // `:0` means "use the bound port" at serve time.
        assert_eq!(
            parse_advertise_addr("myhost:0").unwrap(),
            ("myhost".into(), None)
        );
        assert_eq!(
            parse_advertise_addr("127.0.0.1:0").unwrap(),
            ("127.0.0.1".into(), None)
        );
        assert_eq!(
            parse_advertise_addr("[::1]:0").unwrap(),
            ("[::1]".into(), None)
        );

        // A literal non-zero port is advertised as-is.
        assert_eq!(
            parse_advertise_addr("myhost:50051").unwrap(),
            ("myhost".into(), Some(50051))
        );

        // No `:` — the bound port is appended at serve time.
        assert_eq!(
            parse_advertise_addr("myhost").unwrap(),
            ("myhost".into(), None)
        );
    }

    #[test]
    fn test_advertise_addr_parse_rejects_unparsable_port() {
        assert!(parse_advertise_addr("myhost:abc").is_err());
    }

    #[tokio::test]
    async fn test_run_with_installers_rejects_duplicates() {
        let hub = GrpcHub::default();
        hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers: vec![installer_a(), installer_a()],
            }],
        };
        let cancel = CancellationToken::new();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let ready = ReadySignal::from_sender(tx);

        let result = hub.run_with_installers(data, cancel, ready).await;

        assert!(result.is_err(), "duplicate services should error");
    }

    #[tokio::test]
    async fn test_run_with_installers_starts_server() {
        let hub = Arc::new(GrpcHub::default());
        hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());
        // `serve_tcp` requires an `auth_layer`; this test only exercises
        // listener/registration plumbing, so wire the disabled layer directly.
        hub.auth_layer
            .set(InternalAuthGrpcLayer::disabled())
            .unwrap();
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers: vec![installer_a(), installer_b()],
            }],
        };
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ready = ReadySignal::from_sender(tx);

        let hub_task = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await })
        };

        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("ready signal should fire")
            .expect("ready channel should complete");

        hub_task
            .await
            .expect("task should join successfully")
            .expect("server should exit cleanly");
    }

    #[tokio::test]
    async fn test_serve_with_system_context() {
        let hub = Arc::new(GrpcHub::default());
        hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());

        // Wire system context with installers
        let installer_store = Arc::new(GrpcInstallerStore::new());
        installer_store
            .set(GrpcInstallerData {
                gears: vec![GearInstallers {
                    gear_name: "test".to_owned(),
                    installers: vec![installer_a()],
                }],
            })
            .expect("store should accept installers");

        let gear_manager = Arc::new(toolkit::runtime::GearManager::new());
        let sys_ctx = toolkit::runtime::SystemContext::new(
            Uuid::new_v4(),
            gear_manager,
            Arc::clone(&installer_store),
        );

        hub.pre_init(&sys_ctx)
            .expect("pre_init should set installer_store and instance_id");
        // `serve` requires `auth_layer` to be set (normally by `init`).
        hub.auth_layer
            .set(InternalAuthGrpcLayer::disabled())
            .unwrap();

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ready = ReadySignal::from_sender(tx);

        let serve_task = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.serve(cancel, ready).await })
        };

        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("ready signal should fire")
            .expect("ready signal should complete");

        serve_task
            .await
            .expect("task should join")
            .expect("serve should complete without error");

        // After serve completes, installer_store should be empty (consumed)
        assert!(
            installer_store.is_empty(),
            "installers should be consumed after serve completes"
        );
    }

    #[test]
    fn omitted_cache_ttl_inherits_struct_default() {
        let cfg: GrpcHubConfig = serde_json::from_value(serde_json::json!({
            "internal_auth": { "provider": "shared_secret", "secret": "x" }
        }))
        .expect("partial config should deserialize via container default");
        assert_eq!(
            cfg.internal_auth_cache_ttl_secs, DEFAULT_INTERNAL_AUTH_CACHE_TTL_SECS,
            "omitted TTL must inherit the struct Default, not u64::default()"
        );
    }

    #[tokio::test]
    async fn test_init_parses_listen_addr() {
        #[derive(Default)]
        struct ConfigProviderWithAddr;
        impl ConfigProvider for ConfigProviderWithAddr {
            fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
                if gear_name == "grpc-hub" {
                    use std::sync::OnceLock;
                    static CONFIG: OnceLock<serde_json::Value> = OnceLock::new();
                    Some(CONFIG.get_or_init(|| {
                        serde_json::json!({
                            "config": {
                                "listen_addr": "127.0.0.1:10"
                            }
                        })
                    }))
                } else {
                    None
                }
            }
        }

        let hub = GrpcHub::default();
        let cancel = CancellationToken::new();

        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(ConfigProviderWithAddr),
            Arc::new(ClientHub::default()),
            cancel,
        );

        hub.init(&ctx).await.expect("init should succeed");

        assert_eq!(
            hub.listen_addr_tcp().expect("should be TCP"),
            "127.0.0.1:10".parse().unwrap()
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_init_parses_uds_addr() {
        #[derive(Default)]
        struct ConfigProviderWithUds;
        impl ConfigProvider for ConfigProviderWithUds {
            fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
                if gear_name == "grpc-hub" {
                    use std::sync::OnceLock;
                    static CONFIG: OnceLock<serde_json::Value> = OnceLock::new();
                    Some(CONFIG.get_or_init(|| {
                        serde_json::json!({
                            "config": {
                                "listen_addr": "uds:///tmp/test_grpc.sock"
                            }
                        })
                    }))
                } else {
                    None
                }
            }
        }

        let hub = GrpcHub::default();
        let cancel = CancellationToken::new();

        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(ConfigProviderWithUds),
            Arc::new(ClientHub::default()),
            cancel,
        );

        hub.init(&ctx).await.expect("init should succeed");

        // Verify that listen_addr_tcp returns None for UDS config
        assert!(
            hub.listen_addr_tcp().is_none(),
            "Expected UDS config, not TCP"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_init_parses_uds_listen_addr_and_serves() {
        use tempfile::TempDir;

        // Custom ConfigProvider returning uds:// path
        struct ConfigProviderWithUds {
            config_value: serde_json::Value,
        }
        impl ConfigProvider for ConfigProviderWithUds {
            fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
                if gear_name == "grpc-hub" {
                    Some(&self.config_value)
                } else {
                    None
                }
            }
        }

        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let socket_path = temp_dir.path().join("test_grpc_hub.sock");
        let socket_path_str = format!("uds://{}", socket_path.display());

        let hub = Arc::new(GrpcHub::default());
        let cancel = CancellationToken::new();

        let config_provider = ConfigProviderWithUds {
            config_value: serde_json::json!({
                "config": {
                    "listen_addr": socket_path_str
                }
            }),
        };

        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(config_provider),
            Arc::new(ClientHub::default()),
            cancel.clone(),
        );

        hub.init(&ctx).await.expect("init should succeed");

        let installers = vec![installer_a()];
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers,
            }],
        };
        let cancel_clone = cancel.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ready = ReadySignal::from_sender(tx);

        let hub_task = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await })
        };

        tokio::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            cancel_clone.cancel();
        });

        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("ready signal should fire")
            .expect("ready channel should complete");

        // Verify socket file was created
        assert!(socket_path.exists(), "Unix socket file should be created");

        hub_task
            .await
            .expect("task should join successfully")
            .expect("server should exit cleanly");
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn test_named_pipe_listen_and_shutdown() {
        // Custom ConfigProvider returning named pipe address
        struct ConfigProviderWithNamedPipe;
        impl ConfigProvider for ConfigProviderWithNamedPipe {
            fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
                if gear_name == "grpc-hub" {
                    use std::sync::OnceLock;
                    static CONFIG: OnceLock<serde_json::Value> = OnceLock::new();
                    Some(CONFIG.get_or_init(|| {
                        serde_json::json!({
                            "config": {
                                "listen_addr": r"pipe://\\.\pipe\test_grpc_hub"
                            }
                        })
                    }))
                } else {
                    None
                }
            }
        }

        let hub = Arc::new(GrpcHub::default());
        let cancel = CancellationToken::new();

        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(ConfigProviderWithNamedPipe),
            Arc::new(ClientHub::default()),
            cancel.clone(),
        );

        hub.init(&ctx).await.expect("init should succeed");

        // Verify that listen_addr_tcp returns None for named pipe config
        assert!(
            hub.listen_addr_tcp().is_none(),
            "Expected named pipe config, not TCP"
        );

        let installers = vec![installer_a()];
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers,
            }],
        };
        let cancel_clone = cancel.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ready = ReadySignal::from_sender(tx);

        let hub_task = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await })
        };

        // Give the server a moment to start, then cancel
        tokio::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            cancel_clone.cancel();
        });

        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("ready signal should fire")
            .expect("ready channel should complete");

        hub_task
            .await
            .expect("task should join successfully")
            .expect("server should exit cleanly");
    }

    #[tokio::test]
    async fn test_run_with_no_installers_exits_gracefully() {
        let hub = GrpcHub::default();
        hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());
        let data = GrpcInstallerData { gears: vec![] };
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ready = ReadySignal::from_sender(tx);

        let hub_task =
            tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await });

        // Schedule cancellation
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        // Should receive ready signal immediately
        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("ready signal should fire")
            .expect("ready channel should complete");

        // Task should complete successfully
        hub_task
            .await
            .expect("task should join successfully")
            .expect("should exit cleanly with no services");
    }

    #[tokio::test]
    async fn test_resolve_directory_client_lazy_after_init() {
        use toolkit::{
            DirectoryClient as DirectoryClientTrait, RegisterInstanceInfo, ServiceEndpoint,
            ServiceInstanceInfo,
        };

        struct MockDirectoryClient;

        #[async_trait]
        impl DirectoryClientTrait for MockDirectoryClient {
            async fn resolve_grpc_service(
                &self,
                _service_name: &str,
            ) -> anyhow::Result<ServiceEndpoint> {
                Ok(ServiceEndpoint::new("mock://endpoint"))
            }
            async fn resolve_rest_service(
                &self,
                _gear_name: &str,
            ) -> anyhow::Result<ServiceEndpoint> {
                Ok(ServiceEndpoint::new("mock://rest"))
            }
            async fn get_openapi_spec(&self, _gear_name: &str) -> anyhow::Result<String> {
                Ok(String::new())
            }
            async fn list_instances(
                &self,
                _gear: &str,
            ) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
                Ok(vec![])
            }
            async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
                Ok(vec![])
            }
            async fn register_instance(&self, _info: RegisterInstanceInfo) -> anyhow::Result<()> {
                Ok(())
            }
            async fn deregister_instance(
                &self,
                _gear: &str,
                _instance_id: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn send_heartbeat(&self, _gear: &str, _instance_id: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }

        struct EmptyConfigProvider;
        impl ConfigProvider for EmptyConfigProvider {
            fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
                None
            }
        }

        let client_hub = Arc::new(ClientHub::default());
        let hub = GrpcHub::default();
        let cancel = CancellationToken::new();

        // Create context with an empty ClientHub (no DirectoryClient yet)
        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(EmptyConfigProvider),
            Arc::clone(&client_hub),
            cancel,
        );

        hub.init(&ctx).await.expect("init should succeed");

        // DirectoryClient is NOT registered yet — should return None
        assert!(
            hub.resolve_directory_client().is_none(),
            "should be None before DirectoryClient is registered"
        );

        // Simulate gear_orchestrator registering DirectoryClient after grpc-hub init
        let mock_dir: Arc<dyn DirectoryClientTrait> = Arc::new(MockDirectoryClient);
        client_hub.register::<dyn DirectoryClientTrait>(mock_dir);

        // Now lazy resolution should find it
        assert!(
            hub.resolve_directory_client().is_some(),
            "should resolve DirectoryClient registered after init()"
        );
    }

    fn config_provider_with(config: serde_json::Value) -> impl ConfigProvider {
        struct Provider(serde_json::Value);
        impl ConfigProvider for Provider {
            fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
                (gear_name == "grpc-hub").then_some(&self.0)
            }
        }
        let mut wrapped = serde_json::Map::new();
        wrapped.insert("config".to_owned(), config);
        Provider(serde_json::Value::Object(wrapped))
    }

    #[tokio::test]
    async fn init_rejects_cache_ttl_over_max() {
        let hub = GrpcHub::default();
        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(config_provider_with(serde_json::json!({
                "internal_auth_cache_ttl_secs": MAX_INTERNAL_AUTH_CACHE_TTL_SECS + 1
            }))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        );
        let err = hub.init(&ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("exceeds the maximum"),
            "expected a maximum-TTL error, got {err}"
        );
    }

    #[tokio::test]
    async fn init_accepts_cache_ttl_at_max() {
        let hub = GrpcHub::default();
        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(config_provider_with(serde_json::json!({
                "internal_auth_cache_ttl_secs": MAX_INTERNAL_AUTH_CACHE_TTL_SECS
            }))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        );
        hub.init(&ctx)
            .await
            .expect("TTL exactly at the max must be accepted");
    }

    #[tokio::test]
    async fn init_rejects_invalid_exempt_method_entries() {
        for bad_entry in ["", "no-leading-slash"] {
            let hub = GrpcHub::default();
            let ctx = GearCtx::new(
                "grpc-hub",
                Uuid::new_v4(),
                Arc::new(config_provider_with(serde_json::json!({
                    "internal_auth_exempt_methods": [bad_entry]
                }))),
                Arc::new(ClientHub::default()),
                CancellationToken::new(),
            );
            let err = hub.init(&ctx).await.unwrap_err();
            assert!(
                err.to_string().contains("invalid"),
                "expected {bad_entry:?} to be rejected, got {err}"
            );
        }
    }

    /// `cpt-cf-adr-platform-plane-auth` acceptance over the real `init()` +
    /// `serve_tcp` path: a `shared_secret` `internal_auth` config gets
    /// installed on the server that actually gets served, and an inbound RPC
    /// is enforced end to end over a real TCP listener.
    #[tokio::test]
    async fn shared_secret_config_enforces_internal_token_over_real_tcp_listener() {
        use secrecy::SecretString;
        use tonic::Request;
        use tonic::client::Grpc;
        use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
        use tonic::service::interceptor::InterceptedService;
        use tonic::transport::Channel;
        use toolkit_transport_grpc::InternalAuthInterceptor;

        const SECRET: &str = "dev-internal-token";

        // A codec that passes message bytes through untouched.
        // `ServiceAImpl` ignores request/response body content entirely, so
        // no protobuf message type is needed to drive a real gRPC round trip
        // through the layered server.
        #[derive(Clone, Default)]
        struct RawBytesCodec;

        impl Encoder for RawBytesCodec {
            type Item = Vec<u8>;
            type Error = tonic::Status;
            fn encode(
                &mut self,
                item: Self::Item,
                dst: &mut EncodeBuf<'_>,
            ) -> Result<(), Self::Error> {
                use bytes::BufMut;
                dst.put_slice(&item);
                Ok(())
            }
        }

        impl Decoder for RawBytesCodec {
            type Item = Vec<u8>;
            type Error = tonic::Status;
            fn decode(
                &mut self,
                src: &mut DecodeBuf<'_>,
            ) -> Result<Option<Self::Item>, Self::Error> {
                use bytes::Buf;
                let mut buf = vec![0u8; src.remaining()];
                src.copy_to_slice(&mut buf);
                Ok(Some(buf))
            }
        }

        impl Codec for RawBytesCodec {
            type Encode = Vec<u8>;
            type Decode = Vec<u8>;
            type Encoder = RawBytesCodec;
            type Decoder = RawBytesCodec;
            fn encoder(&mut self) -> Self::Encoder {
                RawBytesCodec
            }
            fn decoder(&mut self) -> Self::Decoder {
                RawBytesCodec
            }
        }

        let hub = Arc::new(GrpcHub::default());
        let cancel = CancellationToken::new();
        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(config_provider_with(serde_json::json!({
                "listen_addr": "127.0.0.1:0",
                "internal_auth": { "provider": "shared_secret", "secret": SECRET },
            }))),
            Arc::new(ClientHub::default()),
            cancel.clone(),
        );
        hub.init(&ctx).await.expect("init should succeed");

        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers: vec![installer_a()],
            }],
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ready = ReadySignal::from_sender(tx);
        let hub_task = {
            let hub = Arc::clone(&hub);
            let cancel = cancel.clone();
            tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await })
        };

        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("ready signal should fire")
            .expect("ready channel should complete");

        let uri = hub
            .get_bound_endpoint()
            .expect("hub should have bound and recorded its TCP endpoint");
        let path = http::uri::PathAndQuery::from_static("/grpc_hub.test.ServiceA/Method");

        // (1) No credential -> Unauthenticated, over the real served listener.
        let channel = Channel::from_shared(uri.clone())
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut anon = Grpc::new(channel);
        anon.ready().await.unwrap();
        let err = anon
            .unary(Request::new(Vec::new()), path.clone(), RawBytesCodec)
            .await
            .expect_err("a call without an internal token must be rejected");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);

        // (2) The matching shared secret -> accepted.
        let channel = Channel::from_shared(uri).unwrap().connect().await.unwrap();
        let intercepted = InterceptedService::new(
            channel,
            InternalAuthInterceptor::from_token(SecretString::from(SECRET)),
        );
        let mut authed = Grpc::new(intercepted);
        authed.ready().await.unwrap();
        // `ServiceAImpl` returns a body with no gRPC-framed message at all
        // (it exists only to prove routing/auth, not to speak a real
        // protocol), so tonic's client reports "missing response message"
        // even on a successful call. What this test asserts is that the
        // request got *past* the auth layer to reach the service at all —
        // i.e. it must never be rejected as unauthenticated.
        if let Err(status) = authed
            .unary(Request::new(Vec::new()), path, RawBytesCodec)
            .await
        {
            assert_ne!(
                status.code(),
                tonic::Code::Unauthenticated,
                "a call with a valid internal token must not be rejected as unauthenticated, \
                 got {status:?}"
            );
        }

        cancel.cancel();
        hub_task
            .await
            .expect("task should join")
            .expect("server should exit cleanly");
    }
}
