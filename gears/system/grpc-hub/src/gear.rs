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
use tokio_util::sync::CancellationToken;
use tonic::{
    service::RoutesBuilder,
    transport::{Server, server::TcpIncoming},
};

use toolkit_security::{DynInternalAuthenticator, InternalAuthConfig};
use toolkit_transport_grpc::{InternalAuthEnforcement, InternalAuthGrpcLayer};

#[cfg(windows)]
use toolkit_transport_grpc::create_named_pipe_incoming;

/// Loopback by default: a hub reachable over the network must be configured
/// explicitly, and then needs platform enforcement.
const DEFAULT_LISTEN_ADDR: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 50051));

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
    /// Defaults to `127.0.0.1:50051` if not specified; set `0.0.0.0:50051` (with
    /// `internal_auth`) for workers on other hosts or pods.
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

    /// Development only: start on a non-loopback TCP address without platform
    /// enforcement. Off by default: such a hub refuses to start, because every
    /// caller that can reach the port could register in the directory or use
    /// Cluster.
    pub allow_unauthenticated_tcp: bool,

    /// Refuse to serve a UDS listener whose existing directory is not owned by
    /// this process's user or is open to group or others, instead of warning.
    /// Only a `0700` directory owned by the service keeps other local users off
    /// the platform plane.
    /// Off by default for development; a production deployment sets it.
    pub strict_socket_dir: bool,
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
            allow_unauthenticated_tcp: false,
            strict_socket_dir: false,
        }
    }
}

/// Configuration for the listen address
#[derive(Clone)]
pub(crate) enum ListenConfig {
    /// `listen_addr: none`: services are registered in-process and nothing is
    /// bound (an embedded binary whose callers all use local clients).
    Disabled,
    /// Listen on a TCP socket address.
    Tcp(SocketAddr),
    /// Listen on a Unix domain socket at this path.
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

/// A UDS path from `listen_addr`, with a leading `~/` resolved against `home`.
#[cfg(unix)]
fn expand_home(
    uds_path: &str,
    home: Option<std::ffi::OsString>,
) -> anyhow::Result<std::path::PathBuf> {
    match uds_path.strip_prefix("~/") {
        Some(rest) => Ok(std::path::PathBuf::from(home.context("uds://~/ needs HOME")?).join(rest)),
        None => Ok(std::path::PathBuf::from(uds_path)),
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

    // Shared-secret (and any future dependency-light provider) builds here. An
    // unusable secret fails the build rather than falling through to the kube
    // branch, which would report the wrong problem.
    match cfg.build_authenticator()? {
        toolkit_security::BuiltAuthenticator::Built(auth) => return Ok(Some(auth)),
        toolkit_security::BuiltAuthenticator::RequiresExternalBackend => {}
    }

    #[cfg(feature = "k8s-auth")]
    {
        if cfg.is_kube() {
            let audiences = cfg.kube_audiences().unwrap_or_default().to_vec();
            // `0` disables caching; any positive TTL wraps the validator in the
            // short-lived positive cache.
            let cache_ttl =
                (cache_ttl_secs > 0).then(|| std::time::Duration::from_secs(cache_ttl_secs));
            let auth = toolkit_k8s_auth::build_cached_k8s_authenticator(audiences, cache_ttl, None)
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
    /// Address the gRPC server listens on; replaceable until the server starts.
    pub(crate) listen_cfg: RwLock<ListenConfig>,
    /// Host and optional port advertised to peers, set once during init.
    pub(crate) advertise_addr: OnceLock<(String, Option<u16>)>,
    /// Store of gRPC service installers collected from other gears, set once during init.
    pub(crate) installer_store: OnceLock<Arc<GrpcInstallerStore>>,
    /// Client hub used to resolve dependencies, set once during init.
    pub(crate) client_hub: OnceLock<Arc<ClientHub>>,
    /// Unique identifier of this hub instance, set once during init.
    pub(crate) instance_id: OnceLock<String>,
    /// Endpoint the server actually bound to, published once `serve` is listening.
    pub(crate) bound_endpoint: RwLock<Option<String>>,
    /// Platform-plane middleware applied to every served gRPC RPC; a
    /// pass-through layer when `internal_auth` is unset.
    pub(crate) auth_layer: OnceLock<InternalAuthGrpcLayer>,
    /// [`GrpcHubConfig::strict_socket_dir`], read at `init`.
    pub(crate) strict_socket_dir: std::sync::atomic::AtomicBool,
    /// Set before `ready` when the hub will never bind (`listen_addr: none`,
    /// or no services), so nobody waits for an endpoint.
    pub(crate) serves_no_endpoint: std::sync::atomic::AtomicBool,
    /// Bound on the first registration pass, which runs before `ready`.
    pub(crate) first_pass_timeout: std::time::Duration,
    /// Bound on one registration call; the remote directory client has its
    /// own, longer one.
    pub(crate) attempt_timeout: std::time::Duration,
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
            strict_socket_dir: std::sync::atomic::AtomicBool::new(false),
            serves_no_endpoint: std::sync::atomic::AtomicBool::new(false),
            first_pass_timeout: std::time::Duration::from_secs(5),
            attempt_timeout: std::time::Duration::from_secs(5),
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
            ListenConfig::Disabled => None,
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
        if listen_addr.eq_ignore_ascii_case("none") {
            *self.listen_cfg.write() = ListenConfig::Disabled;
            tracing::info!("gRPC hub listener disabled (listen_addr: none)");
            return Ok(());
        }

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
            // `~/` is the service user's home, as `server.home_dir` reads it:
            // a generated single-node configuration puts the socket in the
            // product's home rather than at a path fixed for every machine.
            let path = expand_home(uds_path, std::env::var_os("HOME"))?;
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

    /// How long a probe of an existing socket waits for an answer.
    #[cfg(unix)]
    const STALE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

    /// Prepare the Unix Domain Socket path: remove a stale socket left by a
    /// previous run, and refuse anything else (a regular file, a symlink, or a
    /// socket another process is still serving on). A socket directory that
    /// another user owns, or that is open to group or others, is refused when
    /// `strict` and warned about otherwise: only a `0700` directory owned by
    /// the service keeps other local users off the platform plane.
    #[cfg(unix)]
    async fn prepare_uds_socket_path(path: &std::path::Path, strict: bool) -> anyhow::Result<()> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        // A missing directory is created owner-only, so the socket is private
        // from the start. One that exists is the operator's; a group- or
        // world-accessible one is reported below. A directory that cannot be
        // inspected is an error: it would otherwise skip the check.
        let dir = Self::socket_dir(path);
        match std::fs::metadata(dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)
                    .with_context(|| format!("create UDS directory '{}'", dir.display()))?;
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("inspect UDS directory '{}'", dir.display()));
            }
            Ok(_) => {}
        }
        let meta = std::fs::metadata(dir)
            .with_context(|| format!("inspect UDS directory '{}'", dir.display()))?;
        Self::check_socket_dir(
            dir,
            std::os::unix::fs::MetadataExt::uid(&meta),
            meta.permissions().mode(),
            rustix::process::geteuid().as_raw(),
            strict,
        )?;

        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(e).with_context(|| format!("inspect UDS path '{}'", path.display()));
            }
        };
        if !meta.file_type().is_socket() {
            anyhow::bail!(
                "refusing to replace '{}': it exists and is not a socket",
                path.display()
            );
        }
        // Ownership first: another user's socket stays whatever a probe says.
        Self::check_socket_owner(
            path,
            std::os::unix::fs::MetadataExt::uid(&meta),
            rustix::process::geteuid().as_raw(),
        )?;
        // A listener with a full backlog never answers; bound the probe so it
        // cannot hold startup.
        let probe = tokio::time::timeout(
            Self::STALE_PROBE_TIMEOUT,
            tokio::net::UnixStream::connect(path),
        )
        .await
        .ok()
        .map(|connected| connected.map(drop));
        Self::stale_socket_verdict(path, probe)?;
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale UDS socket '{}'", path.display()))?;
        tracing::debug!(path = %path.display(), "removed stale UDS socket before bind");
        Ok(())
    }

    /// Bind the listener owner-only from the start. `bind` creates the socket
    /// with the process umask, and restricting the final path afterwards
    /// leaves a window in which another local user can connect, and follows a
    /// symlink planted there. So the socket is bound in a fresh `0700`
    /// directory beside `path`, restricted there where nobody else can reach
    /// it, and renamed into place. The staging name is short: a socket path is
    /// limited to about 104 bytes on macOS.
    #[cfg(unix)]
    fn bind_owner_only(path: &std::path::Path) -> anyhow::Result<tokio::net::UnixListener> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let staging = Self::socket_dir(path).join(format!(".b{}", std::process::id()));
        if let Ok(meta) = std::fs::symlink_metadata(&staging)
            && meta.is_dir()
            && std::os::unix::fs::MetadataExt::uid(&meta) == rustix::process::geteuid().as_raw()
        {
            // Left by a run of this service that died with the same pid.
            std::fs::remove_dir_all(&staging)
                .with_context(|| format!("remove leftover UDS staging '{}'", staging.display()))?;
        }
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .with_context(|| format!("create UDS staging '{}'", staging.display()))?;
        let staged = staging.join("s");
        let bound = tokio::net::UnixListener::bind(&staged)
            .with_context(|| format!("failed to bind UDS listener at '{}'", path.display()))
            .and_then(|listener| {
                std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("restrict UDS socket '{}'", path.display()))?;
                std::fs::rename(&staged, path)
                    .with_context(|| format!("move UDS socket into '{}'", path.display()))?;
                Ok(listener)
            });
        if let Err(e) = std::fs::remove_dir_all(&staging) {
            tracing::warn!(
                staging = %staging.display(),
                error = %e,
                "could not remove the UDS staging directory"
            );
        }
        bound
    }

    /// The directory holding the socket: a bare file name (`uds://hub.sock`)
    /// lives in the working directory, which is checked like any other.
    #[cfg(unix)]
    fn socket_dir(path: &std::path::Path) -> &std::path::Path {
        match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            Some(_) => std::path::Path::new("."),
            None => std::path::Path::new("/"),
        }
    }

    /// Whether the socket's directory keeps other local users out: owned by this
    /// process's user and closed to group and others. A failing directory is an
    /// error when `strict`, a warning otherwise.
    #[cfg(unix)]
    fn check_socket_dir(
        dir: &std::path::Path,
        owner: u32,
        mode: u32,
        euid: u32,
        strict: bool,
    ) -> anyhow::Result<()> {
        let problem = if owner != euid {
            format!("is owned by uid {owner}, not this service's uid {euid}")
        } else if mode & 0o077 != 0 {
            format!("has mode {:o}, open to group or others", mode & 0o777)
        } else {
            return Ok(());
        };
        if strict {
            anyhow::bail!(
                "refusing to serve on a UDS in '{}': the directory {problem}; use a 0700 directory \
                 owned by the service (strict_socket_dir)",
                dir.display()
            );
        }
        tracing::warn!(
            dir = %dir.display(),
            "gRPC hub UDS directory {problem}; use a 0700 directory owned by the service in \
             production"
        );
        Ok(())
    }

    /// Only a socket this service owns may be replaced.
    #[cfg(unix)]
    fn check_socket_owner(path: &std::path::Path, owner: u32, euid: u32) -> anyhow::Result<()> {
        if owner != euid {
            anyhow::bail!(
                "refusing to replace '{}': it is owned by uid {owner}, not this service (uid {euid})",
                path.display()
            );
        }
        Ok(())
    }

    /// Whether an existing socket of ours may be unlinked, from the probe's
    /// outcome (`None`: no answer in time). Only a refused connection proves
    /// that nobody is listening. A socket someone serves on, one that does not
    /// answer, and any other connection error (permission, I/O) leave it in
    /// place.
    #[cfg(unix)]
    fn stale_socket_verdict(
        path: &std::path::Path,
        probe: Option<std::io::Result<()>>,
    ) -> anyhow::Result<()> {
        match probe {
            None => anyhow::bail!(
                "refusing to replace '{}': it does not answer within {}s",
                path.display(),
                Self::STALE_PROBE_TIMEOUT.as_secs()
            ),
            Some(Ok(())) => anyhow::bail!(
                "refusing to replace '{}': another process is serving on it",
                path.display()
            ),
            Some(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => Ok(()),
            Some(Err(e)) => Err(anyhow::Error::new(e).context(format!(
                "refusing to replace '{}': cannot tell whether it is stale",
                path.display()
            ))),
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
            self.serves_no_endpoint
                .store(true, std::sync::atomic::Ordering::Release);
            ready.notify();
            cancel.cancelled().await;
            return Ok(());
        };

        let listen_cfg = self.listen_cfg.read().clone();
        let serve_result = match listen_cfg {
            ListenConfig::Disabled => {
                // In-process callers use local clients; nothing to bind or
                // register an endpoint for.
                self.serves_no_endpoint
                    .store(true, std::sync::atomic::Ordering::Release);
                ready.notify();
                cancel.cancelled().await;
                return Ok(());
            }
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
        let incoming = TcpIncoming::bind(addr)?.with_nodelay(Some(true));
        let bound_addr = incoming.local_addr()?;
        tracing::info!(%bound_addr, transport = "tcp", "gRPC hub listening");

        self.set_bound_endpoint(format!("http://{bound_addr}"));

        let endpoint = self.tcp_directory_endpoint(bound_addr);
        if endpoint.is_none() {
            tracing::warn!(
                %bound_addr,
                "listen_addr is unspecified and no advertise_addr configured; skipping Directory registration"
            );
        }

        let retry = self.register_gears_first(gears, endpoint, &cancel).await;
        ready.notify();

        let shutdown = cancel.clone();
        let server = Server::builder()
            .layer(self.effective_auth_layer()?)
            .add_routes(routes)
            .serve_with_incoming_shutdown(incoming, async move {
                shutdown.cancelled().await;
            });
        self.serve_beside_registration(server, retry, &cancel).await
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
        use tokio_stream::wrappers::UnixListenerStream;

        Self::prepare_uds_socket_path(
            &path,
            self.strict_socket_dir
                .load(std::sync::atomic::Ordering::Relaxed),
        )
        .await?;

        tracing::info!(
            path = %path.display(),
            transport = "uds",
            "gRPC hub listening"
        );

        let uds = Self::bind_owner_only(&path)?;

        let endpoint = format!("unix://{}", path.display());
        self.set_bound_endpoint(endpoint.clone());
        let retry = self
            .register_gears_first(gears, Some(endpoint), &cancel)
            .await;
        ready.notify();

        let incoming = UnixListenerStream::new(uds);
        let shutdown = cancel.clone();
        let server = Server::builder()
            .layer(self.effective_auth_layer()?)
            .add_routes(routes)
            .serve_with_incoming_shutdown(incoming, async move {
                shutdown.cancelled().await;
            });
        self.serve_beside_registration(server, retry, &cancel).await
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
        let retry = self
            .register_gears_first(gears, Some(endpoint), &cancel)
            .await;
        ready.notify();

        let incoming = create_named_pipe_incoming(pipe_name, cancel.clone());
        let shutdown = cancel.clone();
        let server = Server::builder()
            .layer(self.effective_auth_layer()?)
            .add_routes(routes)
            .serve_with_incoming_shutdown(incoming, async move {
                shutdown.cancelled().await;
            });
        self.serve_beside_registration(server, retry, &cancel).await
    }

    /// Run `server` with the registration retries beside it. A server that
    /// ends -- an accept error, or shutdown -- ends the retries with it and
    /// reports at once; retries that finish leave the server running.
    async fn serve_beside_registration<E>(
        &self,
        server: impl std::future::Future<Output = Result<(), E>>,
        retry: Option<Retry<'_>>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let retries = self.register_gears_until_done(retry, cancel);
        tokio::pin!(server, retries);
        // cancel-safe: dropping `retries` when the server ends abandons at most
        // one registration call in flight, as cancellation does.
        let outcome = tokio::select! {
            outcome = &mut server => outcome,
            () = &mut retries => server.await,
        };
        outcome.context("gRPC hub server ended")
    }

    /// First registration pass, before `ready`: the host's REST registration,
    /// which runs after the start phase, then lands on a record that already
    /// holds the gRPC services. Bounded, so a hung directory does not hold
    /// `ready`. Returns what is left to retry: the gears the directory has not
    /// accepted yet.
    async fn register_gears_first<'a>(
        &self,
        gears: &'a [GearInstallers],
        endpoint: Option<String>,
        cancel: &CancellationToken,
    ) -> Option<Retry<'a>> {
        let endpoint = endpoint?;
        let mut pending: Vec<&GearInstallers> = gears.iter().collect();
        // cancel-safe: a pass cut short leaves in `pending` exactly the gears
        // whose registration was not confirmed.
        let pass = tokio::time::timeout(
            self.first_pass_timeout,
            self.register_pending(&mut pending, &endpoint),
        );
        tokio::select! {
            () = cancel.cancelled() => return None,
            _ = pass => {}
        }
        if pending.is_empty() {
            return None;
        }
        tracing::warn!(
            pending = ?Retry::names(&pending),
            first_pass_timeout_ms = self.first_pass_timeout.as_millis(),
            "Directory registration of the hub's gRPC services is incomplete after the first \
             pass; serving, and retrying"
        );
        Some(Retry { endpoint, pending })
    }

    /// Retry the pending gears beside the running server, with jittered
    /// backoff, until each is accepted or permanently rejected, or `cancel`
    /// fires. Cancellation also ends a call in flight, so a slow directory
    /// neither holds shutdown open nor sees later gears registered after it.
    ///
    /// A failed registration must not end the serve task: a directory that
    /// comes up after this process -- Flight Control starting after the
    /// cluster unit on a cold start -- used to close the port for good while
    /// the process stayed ready.
    async fn register_gears_until_done(
        &self,
        retry: Option<Retry<'_>>,
        cancel: &CancellationToken,
    ) {
        let Some(Retry {
            endpoint,
            mut pending,
        }) = retry
        else {
            return;
        };
        let mut backoff = INITIAL_BACKOFF;
        while !pending.is_empty() {
            // cancel-safe: between passes nothing is in flight; a pass cut
            // short leaves the unconfirmed gears in `pending`.
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(jittered(backoff)) => {}
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = self.register_pending(&mut pending, &endpoint) => {}
            }
            if !pending.is_empty() {
                tracing::warn!(
                    pending = ?Retry::names(&pending),
                    backoff_ms = backoff.as_millis(),
                    "Directory registration of the hub's gRPC services failed; serving, and retrying"
                );
            }
            backoff = next_backoff(backoff);
        }
    }

    /// One registration pass over `pending`: each accepted gear, and each one
    /// the directory refuses for good, leaves the list; a gear that failed
    /// transiently stays, and the pass goes on to the next one. Each call is
    /// bounded by `attempt_timeout`.
    ///
    /// The record carries only the gRPC half: the directory keeps the REST
    /// endpoint and `OpenAPI` spec the host registered for the same instance
    /// (`GearInstance::with_metadata_of`), so nothing is read back first.
    async fn register_pending(&self, pending: &mut Vec<&GearInstallers>, endpoint: &str) {
        let Some(directory) = self.resolve_directory_client() else {
            tracing::info!("DirectoryClient not available; skipping Directory registration");
            pending.clear();
            return;
        };
        let Some(instance_id) = self.instance_id.get() else {
            tracing::error!(
                "GrpcHub instance_id not set: SystemGear::pre_init must run before Directory \
                 registration; not registering"
            );
            pending.clear();
            return;
        };

        let mut next = 0;
        while let Some(gear_data) = pending.get(next) {
            if self
                .register_one(directory.as_ref(), gear_data, instance_id, endpoint)
                .await
            {
                pending.remove(next);
            } else {
                next += 1;
            }
        }
    }

    /// Register one gear's gRPC services, bounded by `attempt_timeout`.
    /// Returns whether the gear is settled: accepted, or refused for good.
    async fn register_one(
        &self,
        directory: &dyn DirectoryClient,
        gear_data: &GearInstallers,
        instance_id: &str,
        endpoint: &str,
    ) -> bool {
        let info = Self::registration_info(gear_data, instance_id, endpoint);
        let outcome = tokio::time::timeout(self.attempt_timeout, directory.register_instance(info))
            .await
            .ok();
        let gear = gear_data.gear_name.as_str();
        match outcome {
            None => {
                tracing::warn!(
                    gear,
                    timeout_ms = self.attempt_timeout.as_millis(),
                    "Directory registration did not answer"
                );
                false
            }
            Some(Ok(())) => {
                tracing::info!(gear, endpoint, "Registered gear in Directory");
                true
            }
            Some(Err(error)) => Self::settle_refusal(gear, &error),
        }
    }

    /// The gRPC half of one gear's record.
    fn registration_info(
        gear_data: &GearInstallers,
        instance_id: &str,
        endpoint: &str,
    ) -> cf_system_sdks::directory::RegisterInstanceInfo {
        let services = gear_data
            .installers
            .iter()
            .map(|i| {
                (
                    i.service_name.to_owned(),
                    cf_system_sdks::directory::ServiceEndpoint::new(endpoint),
                )
            })
            .collect();
        cf_system_sdks::directory::RegisterInstanceInfo::new(&gear_data.gear_name, instance_id)
            .with_grpc_services(services)
            .with_version(env!("CARGO_PKG_VERSION"))
    }

    /// Whether a refused registration is settled: a permanent refusal is,
    /// and is not retried.
    fn settle_refusal(gear: &str, error: &anyhow::Error) -> bool {
        if is_permanent_rejection(error) {
            tracing::error!(
                gear,
                error = %error,
                "Directory refused the gear's gRPC services for good; not retrying"
            );
            return true;
        }
        tracing::warn!(gear, error = %error, "Directory registration failed");
        false
    }

    /// Runs the gRPC server until `cancel` fires, signalling `ready` once the listener is bound.
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

/// The registration still to retry: the gears the directory has not accepted.
struct Retry<'a> {
    endpoint: String,
    pending: Vec<&'a GearInstallers>,
}

impl Retry<'_> {
    fn names<'g>(pending: &[&'g GearInstallers]) -> Vec<&'g str> {
        pending.iter().map(|g| g.gear_name.as_str()).collect()
    }
}

/// A refusal that retrying the same registration cannot change: a malformed
/// request, a peer not allowed to register the gear, or a service name pinned
/// to another gear. Anything else -- an unreachable directory, a name another
/// gear currently advertises -- may clear.
fn is_permanent_rejection(error: &anyhow::Error) -> bool {
    use cf_system_sdks::directory::{
        DirectoryInvalidArgument, DirectoryPermissionDenied, DirectoryServiceNameConflict,
    };
    error.is::<DirectoryInvalidArgument>()
        || error.is::<DirectoryPermissionDenied>()
        || error
            .downcast_ref::<DirectoryServiceNameConflict>()
            .is_some_and(|conflict| !conflict.recoverable)
}

/// First delay between registration passes.
const INITIAL_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);
/// Longest delay between registration passes.
const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

/// The delay after `current`: doubled, up to [`MAX_BACKOFF`].
fn next_backoff(current: std::time::Duration) -> std::time::Duration {
    current.saturating_mul(2).min(MAX_BACKOFF)
}

/// `delay` scaled by a random factor in `[0.8, 1.2)`, so hubs that failed
/// together do not retry in lockstep.
fn jittered(delay: std::time::Duration) -> std::time::Duration {
    use std::hash::{BuildHasher, Hasher};
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let permille = 800 + u32::try_from(random % 400).unwrap_or(0);
    delay * permille / 1000
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

    fn serves_no_endpoint(&self) -> bool {
        self.serves_no_endpoint
            .load(std::sync::atomic::Ordering::Acquire)
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
        // A hub reachable over the network must authenticate its callers.
        // `Permissive` lets a token-less caller through, so it counts as no
        // enforcement here.
        let enforced = authenticator.is_some()
            && cfg.internal_auth_enforcement == InternalAuthEnforcement::Required;
        if let ListenConfig::Tcp(addr) = &*self.listen_cfg.read()
            && !addr.ip().is_loopback()
            && !enforced
        {
            if cfg.allow_unauthenticated_tcp {
                tracing::warn!(
                    %addr,
                    "grpc-hub serves non-loopback TCP without required platform enforcement \
                     (allow_unauthenticated_tcp; development only)"
                );
            } else {
                anyhow::bail!(
                    "grpc-hub listen_addr {addr} is reachable over the network without required \
                     platform enforcement: configure internal_auth with enforcement `required`, \
                     bind loopback / UDS / none, or set allow_unauthenticated_tcp for development"
                );
            }
        }
        self.strict_socket_dir
            .store(cfg.strict_socket_dir, std::sync::atomic::Ordering::Relaxed);
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
        collections::HashMap,
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

    /// `ServiceA` that counts the requests its own instance handled: a test's
    /// evidence that cannot be satisfied by another test's call.
    #[derive(Clone, Default)]
    struct CountingServiceA(Arc<std::sync::atomic::AtomicUsize>);

    impl NamedService for CountingServiceA {
        const NAME: &'static str = SERVICE_A;
    }

    impl Service<Request<Body>> for CountingServiceA {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            future::ready(Ok(Response::new(Body::empty())))
        }
    }

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
            secret: secrecy::SecretString::from("test-secret"),
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

    /// How the scripted directory answers a gear's registration.
    #[derive(Clone, Copy)]
    enum Rejection {
        /// An unreachable directory: worth retrying.
        Transient,
        /// A peer not allowed to register the gear: never worth retrying.
        Permanent,
    }

    /// A directory whose answers the test controls: it refuses until opened,
    /// can hang every call or refuse one gear, keeps what the hub sent, and
    /// stores records as the real one does -- per `(gear, instance_id)`, an
    /// absent REST endpoint or empty gRPC set keeping the stored one.
    #[derive(Default)]
    struct ScriptedDirectory {
        open: std::sync::atomic::AtomicBool,
        hang: std::sync::atomic::AtomicBool,
        attempts: std::sync::atomic::AtomicUsize,
        list_calls: std::sync::atomic::AtomicUsize,
        rejections: parking_lot::Mutex<HashMap<String, Rejection>>,
        sent: parking_lot::Mutex<Vec<toolkit::RegisterInstanceInfo>>,
        records: parking_lot::Mutex<Vec<toolkit::ServiceInstanceInfo>>,
    }

    impl ScriptedDirectory {
        fn opened() -> Arc<Self> {
            let directory = Self::default();
            directory
                .open
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Arc::new(directory)
        }

        fn attempts_for(&self, gear: &str) -> usize {
            self.sent.lock().iter().filter(|i| i.gear == gear).count()
        }

        fn record_of(&self, gear: &str) -> Option<toolkit::ServiceInstanceInfo> {
            self.records.lock().iter().find(|r| r.gear == gear).cloned()
        }
    }

    #[async_trait]
    impl toolkit::DirectoryClient for ScriptedDirectory {
        async fn resolve_grpc_service(&self, _s: &str) -> anyhow::Result<toolkit::ServiceEndpoint> {
            anyhow::bail!("unused")
        }
        async fn resolve_rest_service(&self, _g: &str) -> anyhow::Result<toolkit::ServiceEndpoint> {
            anyhow::bail!("unused")
        }
        async fn get_openapi_spec(&self, _g: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn list_instances(
            &self,
            gear: &str,
        ) -> anyhow::Result<Vec<toolkit::ServiceInstanceInfo>> {
            self.list_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self
                .records
                .lock()
                .iter()
                .filter(|r| r.gear == gear)
                .cloned()
                .collect())
        }
        async fn list_all_instances(&self) -> anyhow::Result<Vec<toolkit::ServiceInstanceInfo>> {
            Ok(self.records.lock().clone())
        }
        async fn register_instance(
            &self,
            info: toolkit::RegisterInstanceInfo,
        ) -> anyhow::Result<()> {
            use std::sync::atomic::Ordering;
            self.attempts.fetch_add(1, Ordering::SeqCst);
            self.sent.lock().push(info.clone());
            if self.hang.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let rejection = self.rejections.lock().get(&info.gear).copied();
            match rejection {
                Some(Rejection::Permanent) => {
                    return Err(cf_system_sdks::directory::DirectoryPermissionDenied::new(
                        "peer not authorized for this gear",
                    )
                    .into());
                }
                Some(Rejection::Transient) => anyhow::bail!("gRPC Unavailable: tcp connect error"),
                None => {}
            }
            if !self.open.load(Ordering::SeqCst) {
                anyhow::bail!("gRPC Unavailable: tcp connect error");
            }
            let mut records = self.records.lock();
            let stored = records
                .iter()
                .position(|r| r.gear == info.gear && r.instance_id == info.instance_id)
                .map(|at| records.remove(at));
            let rest = info
                .rest_endpoint
                .or_else(|| stored.as_ref().and_then(|r| r.rest_endpoint.clone()));
            let grpc = if info.grpc_services.is_empty() {
                stored.map(|r| r.grpc_services).unwrap_or_default()
            } else {
                info.grpc_services
            };
            records.push(
                toolkit::ServiceInstanceInfo::new(&info.gear, &info.instance_id)
                    .with_rest_endpoint(rest)
                    .with_grpc_services(grpc),
            );
            Ok(())
        }
        async fn deregister_instance(&self, _g: &str, _i: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_heartbeat(&self, _g: &str, _i: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A hub initialised on loopback with `directory` in its client hub.
    async fn hub_with_directory(
        directory: &Arc<ScriptedDirectory>,
        mut hub: GrpcHub,
    ) -> Arc<GrpcHub> {
        let client_hub = Arc::new(ClientHub::default());
        client_hub.register::<dyn toolkit::DirectoryClient>(
            Arc::clone(directory) as Arc<dyn toolkit::DirectoryClient>
        );
        // Short bounds keep the tests that exercise them fast.
        hub.first_pass_timeout = Duration::from_millis(500);
        hub.attempt_timeout = Duration::from_millis(500);
        let hub = Arc::new(hub);
        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(config_provider_with(
                serde_json::json!({ "listen_addr": "127.0.0.1:0", "advertise_addr": "127.0.0.1" }),
            )),
            Arc::clone(&client_hub),
            CancellationToken::new(),
        );
        hub.init(&ctx).await.expect("init");
        hub.instance_id.set(Uuid::new_v4().to_string()).unwrap();
        hub
    }

    /// A hub with `directory` and `gears` installed, serving on loopback;
    /// returns it, its task, the cancel token and the ready receiver.
    async fn spawn_hub_with_directory(
        directory: &Arc<ScriptedDirectory>,
        gears: Vec<GearInstallers>,
    ) -> (
        Arc<GrpcHub>,
        tokio::task::JoinHandle<anyhow::Result<()>>,
        CancellationToken,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let hub = hub_with_directory(directory, GrpcHub::default()).await;
        let cancel = CancellationToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = {
            let hub = Arc::clone(&hub);
            let cancel = cancel.clone();
            tokio::spawn(async move {
                hub.run_with_installers(
                    GrpcInstallerData { gears },
                    cancel,
                    ReadySignal::from_sender(tx),
                )
                .await
            })
        };
        (hub, task, cancel, rx)
    }

    fn service_a_gear(name: &str, calls: &Arc<std::sync::atomic::AtomicUsize>) -> GearInstallers {
        GearInstallers {
            gear_name: name.to_owned(),
            installers: vec![toolkit::contracts::RegisterGrpcServiceFn {
                service_name: SERVICE_A,
                register: Box::new({
                    let calls = Arc::clone(calls);
                    move |routes| {
                        routes.add_service(CountingServiceA(Arc::clone(&calls)));
                    }
                }),
            }],
        }
    }

    fn service_b_gear(name: &str) -> GearInstallers {
        GearInstallers {
            gear_name: name.to_owned(),
            installers: vec![installer_b()],
        }
    }

    /// Wait until `done` holds, failing the test after five seconds instead
    /// of hanging it.
    async fn eventually(what: &str, done: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    async fn await_ready(rx: tokio::sync::oneshot::Receiver<()>) {
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("ready")
            .expect("ready channel");
    }

    /// With the directory up, the hub's record is in place when it signals
    /// ready, so the host's REST registration that follows lands on it.
    #[tokio::test]
    async fn an_available_directory_holds_the_record_before_ready() {
        let directory = ScriptedDirectory::opened();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_hub, task, cancel, rx) =
            spawn_hub_with_directory(&directory, vec![service_a_gear("test", &calls)]).await;
        await_ready(rx).await;
        let records = directory.records.lock().clone();
        assert_eq!(records.len(), 1, "registered before ready: {records:?}");
        assert_eq!(records[0].grpc_services.len(), 1);
        cancel.cancel();
        task.await.unwrap().expect("clean shutdown");
    }

    /// The hub writes only its gRPC half and reads nothing back: a failed or
    /// stale read can no longer erase the REST endpoint the host registered,
    /// whichever registration lands first.
    #[tokio::test]
    async fn the_hub_registers_only_its_grpc_half() {
        let directory = ScriptedDirectory::opened();
        let (hub, task, cancel, rx) =
            spawn_hub_with_directory(&directory, vec![service_b_gear("test")]).await;
        await_ready(rx).await;
        let sent = directory.sent.lock().clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].rest_endpoint.is_none(), "no REST endpoint sent");
        assert!(sent[0].openapi_spec.is_none(), "no OpenAPI spec sent");
        assert!(!sent[0].grpc_services.is_empty());
        assert_eq!(
            directory
                .list_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "nothing is read back before registering"
        );
        // The host's REST registration, after the hub's: both halves stay.
        let instance_id = hub.instance_id.get().unwrap().clone();
        toolkit::DirectoryClient::register_instance(
            &*directory,
            toolkit::RegisterInstanceInfo::new("test", &instance_id)
                .with_rest_endpoint(toolkit::ServiceEndpoint::new("http://127.0.0.1:8080")),
        )
        .await
        .unwrap();
        let record = directory.record_of("test").unwrap();
        assert!(record.rest_endpoint.is_some() && !record.grpc_services.is_empty());
        cancel.cancel();
        task.await.unwrap().expect("clean shutdown");
    }

    /// The directory is not up yet (Flight Control starting after this
    /// unit). The hub serves anyway and registers once the directory accepts;
    /// it used to fail the first attempt, end the serve task and leave the
    /// port closed. The late record keeps the REST endpoint the host
    /// registered for the same instance meanwhile.
    #[tokio::test]
    async fn a_late_directory_neither_stops_the_hub_nor_loses_its_registration() {
        use std::sync::atomic::Ordering;

        let directory = Arc::new(ScriptedDirectory::default());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (hub, task, cancel, rx) =
            spawn_hub_with_directory(&directory, vec![service_a_gear("test", &calls)]).await;
        await_ready(rx).await;
        let bound = hub
            .get_bound_endpoint()
            .expect("bound")
            .trim_start_matches("http://")
            .to_owned();
        // A completed RPC, not just an accepted connection, while the
        // directory accepts nothing.
        let path = http::uri::PathAndQuery::from_static("/grpc_hub.test.ServiceA/Method");
        let channel = tonic::transport::Channel::from_shared(format!("http://{bound}"))
            .unwrap()
            .connect()
            .await
            .expect("connects");
        let mut client = tonic::client::Grpc::new(channel);
        client.ready().await.unwrap();
        // The handler itself must run: this instance counts its calls (it
        // answers with an empty body, which tonic reports as an error -- the
        // count, not the status, is the evidence).
        let _status = client
            .unary(tonic::Request::new(Vec::new()), path, RawBytesCodec)
            .await;
        assert!(
            calls.load(Ordering::SeqCst) > 0,
            "the hub's service handled the RPC"
        );
        assert!(
            directory.records.lock().is_empty(),
            "served before any registration was accepted"
        );

        // The directory comes up and the host registers this instance's REST
        // endpoint before the hub's retry lands.
        let instance_id = hub.instance_id.get().unwrap().clone();
        directory.records.lock().push(
            toolkit::ServiceInstanceInfo::new("test", &instance_id)
                .with_rest_endpoint(Some(toolkit::ServiceEndpoint::new("http://127.0.0.1:8080"))),
        );
        directory.open.store(true, Ordering::SeqCst);

        eventually("the retried registration", || {
            directory
                .record_of("test")
                .is_some_and(|r| !r.grpc_services.is_empty())
        })
        .await;
        let record = directory.record_of("test").unwrap();
        assert_eq!(
            record.rest_endpoint.map(|e| e.uri),
            Some("http://127.0.0.1:8080".to_owned()),
            "the REST endpoint survives the hub's registration"
        );
        assert!(!task.is_finished(), "the hub is still serving");
        tokio::net::TcpStream::connect(&bound)
            .await
            .expect("still serving");

        cancel.cancel();
        task.await.unwrap().expect("clean shutdown");
    }

    /// A directory that does not answer the first pass holds `ready` only as
    /// long as `first_pass_timeout`; the hub then serves and keeps retrying.
    #[tokio::test]
    async fn a_hung_first_pass_releases_ready_after_its_timeout() {
        let directory = Arc::new(ScriptedDirectory::default());
        directory
            .hang
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let started = tokio::time::Instant::now();
        let (hub, task, cancel, rx) =
            spawn_hub_with_directory(&directory, vec![service_b_gear("test")]).await;
        await_ready(rx).await;
        assert!(
            started.elapsed() >= hub.first_pass_timeout,
            "ready waited for the first pass"
        );
        assert!(hub.get_bound_endpoint().is_some(), "serving");
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown")
            .unwrap()
            .expect("clean shutdown");
    }

    /// One gear's failure does not hold the others: a transient one is
    /// retried, a permanent one is dropped after one attempt.
    #[tokio::test]
    async fn each_gear_is_retried_on_its_own_and_permanent_refusals_are_dropped() {
        use std::sync::atomic::Ordering;

        let directory = ScriptedDirectory::opened();
        directory
            .rejections
            .lock()
            .insert("flaky".to_owned(), Rejection::Transient);
        directory
            .rejections
            .lock()
            .insert("pinned".to_owned(), Rejection::Permanent);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // Service names are unique per hub, so the refused gear brings none.
        let gears = vec![
            service_b_gear("flaky"),
            GearInstallers {
                gear_name: "pinned".to_owned(),
                installers: Vec::new(),
            },
            service_a_gear("good", &calls),
        ];
        let (_hub, task, cancel, rx) = spawn_hub_with_directory(&directory, gears).await;
        await_ready(rx).await;
        assert!(
            directory.record_of("good").is_some(),
            "a gear after a failing one registers in the same pass"
        );

        eventually("a retry of the flaky gear", || {
            directory.attempts_for("flaky") >= 3
        })
        .await;
        assert_eq!(
            directory.attempts_for("pinned"),
            1,
            "a permanent refusal is not retried"
        );
        directory.rejections.lock().remove("flaky");
        eventually("the flaky gear's registration", || {
            directory.record_of("flaky").is_some()
        })
        .await;
        let settled = directory.attempts.load(Ordering::SeqCst);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(
            directory.attempts.load(Ordering::SeqCst),
            settled,
            "nothing is retried once every gear is settled"
        );
        cancel.cancel();
        task.await.unwrap().expect("clean shutdown");
    }

    /// A server that ends while registration is still being retried reports
    /// at once instead of waiting for the directory.
    #[tokio::test]
    async fn a_dead_server_ends_the_registration_retries() {
        let directory = Arc::new(ScriptedDirectory::default());
        let hub = hub_with_directory(&directory, GrpcHub::default()).await;
        let gear = service_b_gear("test");
        let retry = Retry {
            endpoint: "http://127.0.0.1:1".to_owned(),
            pending: vec![&gear],
        };
        let cancel = CancellationToken::new();
        let server = async { Err::<(), _>(std::io::Error::other("accept failed")) };
        let err = tokio::time::timeout(
            Duration::from_secs(1),
            hub.serve_beside_registration(server, Some(retry), &cancel),
        )
        .await
        .expect("the server's end is not held by the retries")
        .unwrap_err();
        assert!(format!("{err:#}").contains("accept failed"), "{err:#}");
    }

    #[test]
    fn backoff_doubles_up_to_its_cap_and_jitter_stays_within_a_fifth() {
        assert_eq!(next_backoff(INITIAL_BACKOFF), INITIAL_BACKOFF * 2);
        assert_eq!(next_backoff(MAX_BACKOFF), MAX_BACKOFF);
        assert_eq!(
            next_backoff(MAX_BACKOFF / 2 + Duration::from_millis(1)),
            MAX_BACKOFF
        );
        let base = Duration::from_secs(10);
        for _ in 0..200 {
            let delay = jittered(base);
            assert!(delay >= base * 4 / 5 && delay < base * 6 / 5, "{delay:?}");
        }
    }

    #[test]
    fn permanent_rejections_are_told_apart_from_transient_ones() {
        use cf_system_sdks::directory::{
            DirectoryInvalidArgument, DirectoryPermissionDenied, DirectoryServiceNameConflict,
        };
        let conflict = |recoverable| {
            anyhow::Error::from(DirectoryServiceNameConflict {
                service_name: "pkg.Svc".to_owned(),
                owner: "other".to_owned(),
                recoverable,
            })
        };
        assert!(is_permanent_rejection(
            &DirectoryInvalidArgument::new("bad").into()
        ));
        assert!(is_permanent_rejection(
            &DirectoryPermissionDenied::new("no").into()
        ));
        assert!(is_permanent_rejection(&conflict(false)));
        assert!(!is_permanent_rejection(&conflict(true)));
        assert!(!is_permanent_rejection(&anyhow::anyhow!(
            "gRPC Unavailable"
        )));
    }

    /// Cancellation ends a registration in flight -- the first attempt and a
    /// retry alike -- so a hung directory holds neither shutdown nor later
    /// gears' registration.
    #[tokio::test]
    async fn cancellation_ends_a_registration_in_flight() {
        use std::sync::atomic::Ordering;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // The first attempt hangs, before ready.
        let directory = Arc::new(ScriptedDirectory::default());
        directory.hang.store(true, Ordering::SeqCst);
        let (_hub, task, cancel, _rx) =
            spawn_hub_with_directory(&directory, vec![service_a_gear("one", &calls)]).await;
        eventually("the first attempt", || {
            directory.attempts.load(Ordering::SeqCst) > 0
        })
        .await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown is not held by the first attempt")
            .unwrap()
            .expect("clean shutdown");

        // A retry hangs, beside the running server.
        let directory = Arc::new(ScriptedDirectory::default());
        let gears = vec![service_a_gear("one", &calls), service_b_gear("two")];
        let (hub, task, cancel, rx) = spawn_hub_with_directory(&directory, gears).await;
        await_ready(rx).await;
        let first_pass = directory.attempts.load(Ordering::SeqCst);
        assert_eq!(first_pass, 2, "the first pass tried both gears");
        directory.hang.store(true, Ordering::SeqCst);
        eventually("a hung retry", || {
            directory.attempts.load(Ordering::SeqCst) > first_pass
        })
        .await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown is not held by a retry")
            .unwrap()
            .expect("clean shutdown");
        assert_eq!(
            directory.attempts.load(Ordering::SeqCst),
            first_pass + 1,
            "no gear registers after cancellation"
        );
        drop(hub);
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

        let client_hub = Arc::new(ClientHub::default());
        let hub = GrpcHub::default();
        let cancel = CancellationToken::new();

        // Create context with an empty ClientHub (no DirectoryClient yet)
        // The default network address needs internal_auth; loopback does not.
        let ctx = GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(config_provider_with(
                serde_json::json!({ "listen_addr": "127.0.0.1:0" }),
            )),
            Arc::clone(&client_hub),
            cancel,
        );

        hub.init(&ctx).await.expect("init should succeed");

        // DirectoryClient is NOT registered yet — should return None
        assert!(
            hub.resolve_directory_client().is_none(),
            "should be None before DirectoryClient is registered"
        );

        // Simulate service_discovery registering DirectoryClient after grpc-hub init
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
                "listen_addr": "127.0.0.1:0",
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
    // A codec that passes message bytes through untouched.
    // `ServiceAImpl` ignores request/response body content entirely, so
    // no protobuf message type is needed to drive a real gRPC round trip
    // through the layered server.
    #[derive(Clone, Default)]
    struct RawBytesCodec;

    impl tonic::codec::Encoder for RawBytesCodec {
        type Item = Vec<u8>;
        type Error = tonic::Status;
        fn encode(
            &mut self,
            item: Self::Item,
            dst: &mut tonic::codec::EncodeBuf<'_>,
        ) -> Result<(), Self::Error> {
            use bytes::BufMut;
            dst.put_slice(&item);
            Ok(())
        }
    }

    impl tonic::codec::Decoder for RawBytesCodec {
        type Item = Vec<u8>;
        type Error = tonic::Status;
        fn decode(
            &mut self,
            src: &mut tonic::codec::DecodeBuf<'_>,
        ) -> Result<Option<Self::Item>, Self::Error> {
            use bytes::Buf;
            let mut buf = vec![0u8; src.remaining()];
            src.copy_to_slice(&mut buf);
            Ok(Some(buf))
        }
    }

    impl tonic::codec::Codec for RawBytesCodec {
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

    /// `serve_tcp` path: a `shared_secret` `internal_auth` config gets
    /// installed on the server that actually gets served, and an inbound RPC
    /// is enforced end to end over a real TCP listener.
    #[tokio::test]
    async fn shared_secret_config_enforces_internal_token_over_real_tcp_listener() {
        use secrecy::SecretString;
        use tonic::Request;
        use tonic::client::Grpc;
        use tonic::service::interceptor::InterceptedService;
        use tonic::transport::Channel;
        use toolkit_transport_grpc::InternalAuthInterceptor;

        const SECRET: &str = "dev-internal-token";

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

    fn ctx_with(config: serde_json::Value) -> GearCtx {
        GearCtx::new(
            "grpc-hub",
            Uuid::new_v4(),
            Arc::new(config_provider_with(config)),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        )
    }

    // A hub reachable over the network without platform enforcement
    // refuses to start; loopback and the development flag are allowed.
    #[tokio::test]
    async fn non_loopback_tcp_without_internal_auth_refuses_to_start() {
        let err = GrpcHub::default()
            .init(&ctx_with(
                serde_json::json!({ "listen_addr": "0.0.0.0:50051" }),
            ))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("without required platform enforcement"),
            "{err}"
        );

        GrpcHub::default()
            .init(&ctx_with(
                serde_json::json!({ "listen_addr": "127.0.0.1:50051" }),
            ))
            .await
            .expect("loopback is allowed");
        GrpcHub::default()
            .init(&ctx_with(serde_json::json!({
                "listen_addr": "0.0.0.0:50051",
                "allow_unauthenticated_tcp": true
            })))
            .await
            .expect("development opt-out is allowed");
        GrpcHub::default()
            .init(&ctx_with(serde_json::json!({
                "listen_addr": "0.0.0.0:50051",
                "internal_auth": { "provider": "shared_secret", "secret": "s", "peer_name": "p" }
            })))
            .await
            .expect("an enforced hub is allowed");
        // Permissive lets token-less callers through: not enforcement.
        let err = GrpcHub::default()
            .init(&ctx_with(serde_json::json!({
                "listen_addr": "0.0.0.0:50051",
                "internal_auth": { "provider": "shared_secret", "secret": "s", "peer_name": "p" },
                "internal_auth_enforcement": "permissive"
            })))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("without required platform enforcement"),
            "{err}"
        );
    }

    // `listen_addr: none` registers services in-process and binds nothing.
    #[tokio::test]
    async fn listen_none_binds_nothing() {
        let hub = Arc::new(GrpcHub::default());
        hub.init(&ctx_with(serde_json::json!({ "listen_addr": "none" })))
            .await
            .expect("init");
        assert!(hub.listen_addr_tcp().is_none());

        let cancel = CancellationToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers: vec![installer_a()],
            }],
        };
        let task = {
            let hub = hub.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                hub.run_with_installers(data, cancel, ReadySignal::from_sender(tx))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("ready")
            .expect("ready channel");
        assert!(hub.get_bound_endpoint().is_none(), "no endpoint is bound");
        assert!(
            toolkit::contracts::GrpcHubCapability::serves_no_endpoint(&*hub),
            "nobody needs to wait for an endpoint"
        );
        cancel.cancel();
        task.await.expect("join").expect("clean exit");
    }

    // The UDS hub never replaces a file that is not a stale socket, and
    // creates its socket owner-only.
    #[cfg(unix)]
    #[tokio::test]
    async fn uds_socket_is_owner_only_and_only_stale_sockets_are_replaced() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let regular = dir.path().join("not-a-socket");
        std::fs::write(&regular, b"x").unwrap();
        let err = GrpcHub::prepare_uds_socket_path(&regular, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a socket"), "{err}");
        assert!(regular.exists(), "a regular file is left alone");

        // A symlink is not followed, whatever it points at.
        let link = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&regular, &link).unwrap();
        let err = GrpcHub::prepare_uds_socket_path(&link, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a socket"), "{err}");
        assert!(
            regular.exists() && link.exists(),
            "the link and its target stay"
        );

        let live = dir.path().join("live.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
        let err = GrpcHub::prepare_uds_socket_path(&live, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("another process"), "{err}");

        let stale = dir.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        GrpcHub::prepare_uds_socket_path(&stale, false)
            .await
            .expect("stale socket is removed");
        assert!(!stale.exists());

        // Only our own socket may go, and only a refused connection proves it
        // stale: a socket that answers, one that does not answer in time, and
        // any other error stay.
        let p = std::path::Path::new("/run/hub.sock");
        assert!(GrpcHub::check_socket_owner(p, 1000, 1000).is_ok());
        let foreign = GrpcHub::check_socket_owner(p, 0, 1000).unwrap_err();
        assert!(foreign.to_string().contains("owned by uid 0"), "{foreign}");
        let refused = Some(Err(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused,
        )));
        assert!(GrpcHub::stale_socket_verdict(p, refused).is_ok());
        let denied = GrpcHub::stale_socket_verdict(
            p,
            Some(Err(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
        )
        .unwrap_err();
        assert!(format!("{denied:#}").contains("cannot tell"), "{denied:#}");
        assert!(
            denied
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .is_some(),
            "the I/O error is kept as the source"
        );
        assert!(GrpcHub::stale_socket_verdict(p, Some(Ok(()))).is_err());
        let silent = GrpcHub::stale_socket_verdict(p, None).unwrap_err();
        assert!(silent.to_string().contains("does not answer"), "{silent}");

        // A directory open to others, or another user's, is refused when
        // strict and only warned about otherwise.
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = GrpcHub::prepare_uds_socket_path(&open.join("hub.sock"), true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mode 755"), "{err}");
        GrpcHub::prepare_uds_socket_path(&open.join("hub.sock"), false)
            .await
            .expect("only a warning without strict_socket_dir");
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        GrpcHub::prepare_uds_socket_path(&private.join("hub.sock"), true)
            .await
            .expect("a 0700 directory of our own passes");
        let foreign =
            GrpcHub::check_socket_dir(std::path::Path::new("/run/hub"), 0, 0o700, 1000, true)
                .unwrap_err();
        assert!(foreign.to_string().contains("owned by uid 0"), "{foreign}");
        // A bare file name is checked against the working directory, not
        // skipped.
        assert_eq!(
            GrpcHub::socket_dir(std::path::Path::new("hub.sock")),
            std::path::Path::new(".")
        );
        assert_eq!(GrpcHub::socket_dir(&open.join("hub.sock")), open.as_path());
        // A missing directory is created 0700, so strict serving starts there.
        let fresh = dir.path().join("fresh/run");
        GrpcHub::prepare_uds_socket_path(&fresh.join("hub.sock"), true)
            .await
            .expect("a directory the hub creates is private");
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );

        // Serving creates the socket 0600.
        let path = dir.path().join("hub.sock");
        let hub = Arc::new(GrpcHub::default());
        hub.init(&ctx_with(serde_json::json!({
            "listen_addr": format!("uds://{}", path.display())
        })))
        .await
        .expect("init");
        let cancel = CancellationToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers: vec![installer_a()],
            }],
        };
        let task = {
            let hub = hub.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                hub.run_with_installers(data, cancel, ReadySignal::from_sender(tx))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("ready")
            .expect("ready channel");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            !dir.path()
                .join(format!(".b{}", std::process::id()))
                .exists(),
            "the staging directory is gone"
        );
        tokio::net::UnixStream::connect(&path)
            .await
            .expect("the moved socket accepts connections");
        cancel.cancel();
        task.await.expect("join").expect("clean exit");

        // strict_socket_dir from the config: serving in an open directory
        // fails before anything is bound.
        let hub = Arc::new(GrpcHub::default());
        hub.init(&ctx_with(serde_json::json!({
            "listen_addr": format!("uds://{}", open.join("strict.sock").display()),
            "strict_socket_dir": true,
        })))
        .await
        .expect("init");
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers: vec![installer_a()],
            }],
        };
        let err = tokio::time::timeout(
            Duration::from_secs(2),
            hub.run_with_installers(data, CancellationToken::new(), ReadySignal::from_sender(tx)),
        )
        .await
        .expect("fails at once")
        .unwrap_err();
        assert!(err.to_string().contains("mode 755"), "{err}");
        assert!(!open.join("strict.sock").exists(), "nothing was bound");

        // A socket whose directory does not exist yet gets one, owner-only.
        let nested = dir.path().join("run/private/hub.sock");
        let hub = Arc::new(GrpcHub::default());
        hub.init(&ctx_with(serde_json::json!({
            "listen_addr": format!("uds://{}", nested.display())
        })))
        .await
        .expect("init");
        let cancel = CancellationToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let data = GrpcInstallerData {
            gears: vec![GearInstallers {
                gear_name: "test".to_owned(),
                installers: vec![installer_a()],
            }],
        };
        let task = {
            let hub = hub.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                hub.run_with_installers(data, cancel, ReadySignal::from_sender(tx))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("ready")
            .expect("ready channel");
        let dir_mode = std::fs::metadata(nested.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "the socket's directory is private");
        cancel.cancel();
        task.await.expect("join").expect("clean exit");
    }

    #[cfg(unix)]
    #[test]
    fn a_leading_tilde_is_the_service_users_home() {
        let home = Some(std::ffi::OsString::from("/home/svc"));
        assert_eq!(
            expand_home("~/run/hub.sock", home).unwrap(),
            std::path::Path::new("/home/svc/run/hub.sock")
        );
        assert_eq!(
            expand_home("/run/hub.sock", None).unwrap(),
            std::path::Path::new("/run/hub.sock"),
            "an absolute path needs no HOME"
        );
        let err = expand_home("~/hub.sock", None).unwrap_err();
        assert!(err.to_string().contains("needs HOME"), "{err}");
    }
}
