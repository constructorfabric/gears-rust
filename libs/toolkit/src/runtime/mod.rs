mod gear_manager;
mod grpc_installers;
mod host_runtime;
#[cfg(feature = "bootstrap")]
mod oop_registration;
#[cfg(feature = "bootstrap")]
mod oop_serve;
mod readiness;
mod runner;
mod system_context;

/// Shutdown signal handling utilities
pub mod shutdown;

#[cfg(test)]
mod tests;

pub use gear_manager::{
    Endpoint, GearInstance, GearManager, GrpcServiceNameConflict, InstanceState,
};
pub use grpc_installers::{GearInstallers, GrpcInstallerData, GrpcInstallerStore, LateBatch};
pub use host_runtime::{
    DEFAULT_SHUTDOWN_DEADLINE, DbOptions, HostRuntime, TOOLKIT_DIRECTORY_ENDPOINT_ENV,
    TOOLKIT_MODULE_CONFIG_ENV,
};
// `DynBearerAuthenticator` / `DynInternalAuthenticator` used to be defined
// here; they now live in `toolkit_security`, so consumers import them from
// there rather than through this re-export.
#[cfg(feature = "bootstrap")]
pub use oop_serve::OopServeOptions;
/// Stays unresolved until the application group's migrations, GTS admission,
/// initialization and startup complete. The system group serves while it waits.
pub const PREPARATION_GATE: &str = "application/preparation";

pub use readiness::{
    DEFAULT_HEALTHCHECK_TIMEOUT, DependencyChecker, ReadinessHealthcheck, ReadinessLifecycle,
    ReadinessReport, ReadinessState,
};
#[cfg(feature = "bootstrap")]
pub use runner::run_oop_serving;
pub use runner::{
    ClientRegistration, OopGearSpawnConfig, OopSpawnOptions, RunOptions, ShutdownOptions, run,
};
pub use system_context::SystemContext;
