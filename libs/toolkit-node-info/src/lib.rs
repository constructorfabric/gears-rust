#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! Node Information Library
//!
//! This library provides system information collection for the current node
//! where the code is executed. It collects:
//! - System information (OS, CPU, memory, GPU, battery, host)
//! - System capabilities (hardware and OS capabilities)
//!
//! This is a standalone library that can be used by any gear to collect
//! information about the current execution environment.

/// Stable hardware identifier detection.
mod hardware_uuid;
/// System capability collection.
mod syscap_collector;
/// Host system information collection backed by `sysinfo`.
mod sysinfo_collector;

// Platform-specific GPU collectors
/// GPU information collection on Linux.
#[cfg(target_os = "linux")]
mod gpu_collector_linux;
/// GPU information collection on macOS.
#[cfg(target_os = "macos")]
mod gpu_collector_macos;
/// GPU information collection on Windows.
#[cfg(target_os = "windows")]
mod gpu_collector_windows;

pub mod error;
pub mod model;

/// Internal collector that aggregates node information.
mod collector;

pub use collector::NodeInfoCollector;
pub use error::NodeInfoError;
pub use hardware_uuid::get_hardware_uuid;
pub use model::*;
