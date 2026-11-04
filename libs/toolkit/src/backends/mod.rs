//! Backend abstraction for out-of-process gear management
//!
//! This gear provides traits and types for spawning and managing `OoP` gear instances.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;
use uuid::Uuid;

/// The kind of backend used to spawn and manage gear instances
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    LocalProcess,
    K8s,
    Static,
    Mock,
}

/// Configuration for an out-of-process gear
pub struct OopGearConfig {
    pub name: String,
    pub binary: Option<PathBuf>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_directory: Option<String>,
    pub backend: BackendKind,
    pub version: Option<String>,
}

impl OopGearConfig {
    pub fn new(name: impl Into<String>, backend: BackendKind) -> Self {
        Self {
            name: name.into(),
            binary: None,
            args: Vec::new(),
            env: HashMap::new(),
            working_directory: None,
            backend,
            version: None,
        }
    }
}

/// A handle to a running gear instance
#[derive(Clone)]
pub struct InstanceHandle {
    pub gear: String,
    pub instance_id: Uuid,
    pub backend: BackendKind,
    pub pid: Option<u32>,
    pub created_at: Instant,
}

impl std::fmt::Debug for InstanceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceHandle")
            .field("gear", &self.gear)
            .field("instance_id", &self.instance_id)
            .field("backend", &self.backend)
            .field("pid", &self.pid)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Trait for backends that can spawn and manage gear instances
#[async_trait]
pub trait GearRuntimeBackend: Send + Sync {
    async fn spawn_instance(&self, cfg: &OopGearConfig) -> Result<InstanceHandle>;
    async fn stop_instance(&self, handle: &InstanceHandle) -> Result<()>;
    async fn list_instances(&self, gear: &str) -> Result<Vec<InstanceHandle>>;
}

/// Configuration passed to `OopBackend::spawn`
pub struct OopSpawnConfig {
    pub gear_name: String,
    pub binary: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_directory: Option<String>,
}

/// A type-erased backend for spawning `OoP` gears.
///
/// This trait is used by `HostRuntime` to spawn `OoP` gears after the start phase.
#[async_trait]
pub trait OopBackend: Send + Sync {
    /// Spawn an `OoP` gear instance.
    async fn spawn(&self, config: OopSpawnConfig) -> Result<()>;

    /// Stop every spawned instance and wait for it to end: the runtime calls
    /// it when startup fails after a spawn, and at shutdown. The runtime waits
    /// at most its shutdown deadline, then stops its in-process gears anyway.
    async fn shutdown_all(&self);
}

pub mod local;
pub mod log_forwarder;

pub use local::LocalProcessBackend;

/// Adapter that implements `OopBackend` trait for `LocalProcessBackend`.
///
/// This allows `LocalProcessBackend` to be used by `HostRuntime` for spawning `OoP` gears.
#[async_trait]
impl OopBackend for LocalProcessBackend {
    async fn spawn(&self, config: OopSpawnConfig) -> Result<()> {
        let mut oop_config = OopGearConfig::new(&config.gear_name, BackendKind::LocalProcess);
        oop_config.binary = Some(config.binary);
        oop_config.args = config.args;
        oop_config.env = config.env;
        oop_config.working_directory = config.working_directory;

        self.spawn_instance(&oop_config).await?;
        Ok(())
    }

    async fn shutdown_all(&self) {
        // Stops and joins every instance still tracked, now: a failed startup
        // ends its children without cancelling the host's (possibly borrowed)
        // token. The cancellation task stops whatever is left on shutdown.
        self.stop_all().await;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_oop_gear_config_builder() {
        let mut cfg = OopGearConfig::new("my_gear", BackendKind::LocalProcess);
        cfg.binary = Some(PathBuf::from("/usr/bin/myapp"));
        cfg.args = vec!["--port".to_owned(), "8080".to_owned()];
        cfg.env.insert("LOG_LEVEL".to_owned(), "debug".to_owned());
        cfg.version = Some("1.0.0".to_owned());

        assert_eq!(cfg.name, "my_gear");
        assert_eq!(cfg.backend, BackendKind::LocalProcess);
        assert_eq!(cfg.binary, Some(PathBuf::from("/usr/bin/myapp")));
        assert_eq!(cfg.args.len(), 2);
        assert_eq!(cfg.env.len(), 1);
        assert_eq!(cfg.version, Some("1.0.0".to_owned()));
    }

    #[test]
    fn test_backend_kind_equality() {
        assert_eq!(BackendKind::LocalProcess, BackendKind::LocalProcess);
        assert_ne!(BackendKind::LocalProcess, BackendKind::K8s);
        assert_ne!(BackendKind::K8s, BackendKind::Static);
        assert_ne!(BackendKind::Static, BackendKind::Mock);
    }

    #[test]
    fn test_instance_handle_debug() {
        let instance_id = Uuid::new_v4();
        let handle = InstanceHandle {
            gear: "test_gear".to_owned(),
            instance_id,
            backend: BackendKind::LocalProcess,
            pid: Some(12345),
            created_at: Instant::now(),
        };

        let debug_str = format!("{handle:?}");
        assert!(debug_str.contains("test_gear"));
        assert!(debug_str.contains(&instance_id.to_string()));
        assert!(debug_str.contains("LocalProcess"));
        assert!(debug_str.contains("12345"));
    }
}
