//! gRPC Hub Module definition
//!
//! Contains the `GrpcHub` module struct and its trait implementations.

use anyhow::Context;
use async_trait::async_trait;
use modkit::{
    DirectoryClient,
    client_hub::ClientHub,
    context::ModuleCtx,
    contracts::{Module, SystemCapability},
    lifecycle::ReadySignal,
    runtime::{GrpcInstallerData, GrpcInstallerStore, ModuleInstallers},
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

#[cfg(windows)]
use modkit_transport_grpc::create_named_pipe_incoming;

const DEFAULT_LISTEN_ADDR: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 50051));

/// Configuration for the gRPC Hub module.
///
/// Supports multiple transport types via `listen_addr`:
/// - TCP: `"127.0.0.1:50051"` or `"0.0.0.0:0"` for ephemeral port
/// - Unix Domain Socket (Unix only): `"uds:///path/to/socket.sock"`
/// - Named Pipe (Windows only): `"pipe://\\.\pipe\my_pipe"` or `"npipe://\\.\pipe\my_pipe"`
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GrpcHubConfig {
    /// Listen address for the gRPC server.
    /// Defaults to `0.0.0.0:50051` if not specified.
    pub listen_addr: String,
}

impl Default for GrpcHubConfig {
    fn default() -> Self {
        Self {
            listen_addr: DEFAULT_LISTEN_ADDR.to_string(),
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

/// The gRPC Hub module.
/// This module is responsible for hosting the gRPC server and managing the gRPC services.
#[modkit::module(
    name = "grpc-hub",
    capabilities = [stateful, system, grpc_hub],
    lifecycle(entry = "serve", await_ready)
)]
pub struct GrpcHub {
    pub(crate) listen_cfg: RwLock<ListenConfig>,
    pub(crate) installer_store: OnceLock<Arc<GrpcInstallerStore>>,
    pub(crate) client_hub: OnceLock<Arc<ClientHub>>,
    pub(crate) instance_id: OnceLock<String>,
    pub(crate) bound_endpoint: RwLock<Option<String>>,
}

impl Default for GrpcHub {
    fn default() -> Self {
        Self {
            listen_cfg: RwLock::new(ListenConfig::Tcp(DEFAULT_LISTEN_ADDR)),
            installer_store: OnceLock::new(),
            client_hub: OnceLock::new(),
            instance_id: OnceLock::new(),
            bound_endpoint: RwLock::new(None),
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

    /// Validate that all service names are unique across all modules.
    fn validate_unique_services(modules: &[ModuleInstallers]) -> anyhow::Result<()> {
        let mut seen = HashSet::new();
        for module in modules {
            for installer in &module.installers {
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

    /// Build routes from module installers. Returns None if no services registered.
    fn build_routes_from_modules(modules: &[ModuleInstallers]) -> Option<tonic::service::Routes> {
        let mut routes_builder = RoutesBuilder::default();
        let mut has_services = false;
        for module in modules {
            for installer in &module.installers {
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

    /// Deregister modules from Directory on shutdown.
    async fn deregister_modules(&self, modules: &[ModuleInstallers]) -> anyhow::Result<()> {
        let Some(directory) = self.resolve_directory_client() else {
            return Ok(());
        };

        let instance_id = self.instance_id.get().ok_or_else(|| {
            anyhow::anyhow!(
                "GrpcHub instance_id not set: SystemModule::pre_init must run before Directory deregistration"
            )
        })?;

        for module_data in modules {
            if let Err(e) = directory
                .deregister_instance(&module_data.module_name, instance_id)
                .await
            {
                tracing::warn!(
                    module = %module_data.module_name,
                    error = %e,
                    "Failed to deregister module from Directory"
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
        Self::validate_unique_services(&data.modules)?;

        let Some(routes) = Self::build_routes_from_modules(&data.modules) else {
            ready.notify();
            cancel.cancelled().await;
            return Ok(());
        };

        let listen_cfg = self.listen_cfg.read().clone();
        let serve_result = match listen_cfg {
            ListenConfig::Tcp(addr) => {
                self.serve_tcp(addr, routes, &data.modules, cancel, ready)
                    .await
            }
            #[cfg(unix)]
            ListenConfig::Uds(path) => {
                self.serve_uds(path, routes, &data.modules, cancel, ready)
                    .await
            }
            #[cfg(windows)]
            ListenConfig::NamedPipe(ref pipe_name) => {
                self.serve_named_pipe(pipe_name.clone(), routes, &data.modules, cancel, ready)
                    .await
            }
        };

        self.deregister_modules(&data.modules).await?;
        serve_result
    }

    /// Serve gRPC over TCP with Directory registration.
    async fn serve_tcp(
        &self,
        addr: SocketAddr,
        routes: tonic::service::Routes,
        modules: &[ModuleInstallers],
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        let bound_addr = listener.local_addr()?;
        let endpoint = format!("http://{bound_addr}");
        tracing::info!(%bound_addr, transport = "tcp", "gRPC hub listening");

        self.set_bound_endpoint(endpoint.clone());
        self.register_modules(modules, &endpoint).await?;
        ready.notify();

        let incoming = TcpListenerStream::new(listener);
        Server::builder()
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
        modules: &[ModuleInstallers],
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
        self.register_modules(modules, &endpoint).await?;
        ready.notify();

        let incoming = UnixListenerStream::new(uds);
        Server::builder()
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
        modules: &[ModuleInstallers],
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        tracing::info!(name = %pipe_name, transport = "named_pipe", "gRPC hub listening");

        let endpoint = format!("pipe://{pipe_name}");
        self.set_bound_endpoint(endpoint.clone());
        self.register_modules(modules, &endpoint).await?;
        ready.notify();

        let incoming = create_named_pipe_incoming(pipe_name, cancel.clone());
        Server::builder()
            .add_routes(routes)
            .serve_with_incoming_shutdown(incoming, async move {
                cancel.cancelled().await;
            })
            .await?;
        Ok(())
    }

    async fn register_modules(
        &self,
        modules: &[ModuleInstallers],
        endpoint: &str,
    ) -> anyhow::Result<()> {
        let Some(directory) = self.resolve_directory_client() else {
            tracing::info!("DirectoryClient not available; skipping Directory registration");
            return Ok(());
        };

        let instance_id = self.instance_id.get().ok_or_else(|| {
            anyhow::anyhow!(
                "GrpcHub instance_id not set: SystemModule::pre_init must run before Directory registration"
            )
        })?;

        {
            for module_data in modules {
                let service_names: Vec<String> = module_data
                    .installers
                    .iter()
                    .map(|i| i.service_name.to_owned())
                    .collect();

                let info = cf_system_sdks::directory::RegisterInstanceInfo {
                    module: module_data.module_name.clone(),
                    instance_id: instance_id.clone(),
                    grpc_services: service_names
                        .iter()
                        .map(|n| {
                            (
                                n.clone(),
                                cf_system_sdks::directory::ServiceEndpoint::new(endpoint),
                            )
                        })
                        .collect(),
                    version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                };

                directory.register_instance(info).await?;
                tracing::info!(
                    module = %module_data.module_name,
                    endpoint = %endpoint,
                    "Registered module in Directory"
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
    fn pre_init(&self, sys: &modkit::runtime::SystemContext) -> anyhow::Result<()> {
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

impl modkit::contracts::GrpcHubCapability for GrpcHub {
    fn bound_endpoint(&self) -> Option<String> {
        self.get_bound_endpoint()
    }
}

#[async_trait]
impl Module for GrpcHub {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        // Load typed configuration
        let cfg: GrpcHubConfig = ctx.config()?;
        tracing::debug!(listen_addr = %cfg.listen_addr, "Loaded gRPC hub configuration");

        // Parse listen_addr into appropriate transport type
        self.apply_listen_config(&cfg.listen_addr)?;

        // Store ClientHub reference for lazy DirectoryClient resolution during serve phase.
        self.client_hub
            .set(ctx.client_hub())
            .map_err(|_| anyhow::anyhow!("ClientHub already set (init called twice?)"))?;

        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "module_tests.rs"]
mod tests;
