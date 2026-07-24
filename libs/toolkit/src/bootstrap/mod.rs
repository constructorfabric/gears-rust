//! Unified bootstrap library for Gears Toolkit gears
//!
//! This crate provides bootstrap functionality for both host (in-process) and
//! `OoP` (out-of-process) Toolkit gears.
//!
//! ## Gears
//!
//! - [`config`]: Configuration types and utilities
//! - [`host`]: Host/in-process bootstrap - logging, signals, and paths
//! - [`oop`]: Out-of-process gear bootstrap - lifecycle management with `DirectoryService`
//!   (requires the `oop` feature)
//!
//! ## Backends
//!
//! Backend types for spawning `OoP` gears have been moved to `toolkit::backends`.

pub mod config;
pub mod crypto;
pub mod host;

pub mod oop;

// Re-export commonly used config types at crate root for convenience
pub use config::{
    AppConfig, CliArgs, ConsoleFormat, GearConfig, GearRuntime, LoggingConfig, RenderedGearConfig,
    RuntimeKind, Section, ServerConfig, TOOLKIT_MODULE_CONFIG_ENV, VendorConfig, VendorConfigError,
    dump_effective_gears_config_json, dump_effective_gears_config_yaml, list_gear_names,
    render_effective_gears_config,
};

// Re-export host types for convenience
pub use oop::{OopRunOptions, run_oop_with_options};

mod run;
pub use run::{run_migrate, run_server};

pub use crypto::{CryptoProviderError, init_crypto_provider, sha256};
