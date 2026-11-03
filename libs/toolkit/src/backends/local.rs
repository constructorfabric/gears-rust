//! Local process backend implementation

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::log_forwarder::{StreamKind, spawn_stream_forwarder};
use super::{BackendKind, GearRuntimeBackend, InstanceHandle, OopGearConfig};

/// Grace period before force-killing processes on shutdown
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// Grace period for individual instance stop
const INSTANCE_STOP_GRACE_PERIOD: Duration = Duration::from_secs(2);

/// Timeout for waiting on forwarder tasks during shutdown
const FORWARDER_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

/// Send graceful termination signal to a child process.
///
/// # Returns
/// - `true` if signal was successfully sent
/// - `false` if:
///   - Process has no PID (already exited)
///   - PID cannot be converted to i32 (extremely rare: PID > 2,147,483,647)
///   - Signal delivery fails
///
/// On Unix: Sends SIGTERM which the process can handle for cleanup.
/// On Windows: Returns false since there's no reliable graceful termination
/// method for console applications.
#[cfg(unix)]
fn send_terminate_signal(child: &Child) -> bool {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let Some(pid) = child.id() else {
        return false;
    };

    let Ok(pid_i32) = i32::try_from(pid) else {
        tracing::warn!(
            pid = pid,
            "Failed to convert PID to i32, cannot send SIGTERM (PID exceeds i32::MAX: {})",
            i32::MAX
        );
        return false;
    };

    kill(Pid::from_raw(pid_i32), Signal::SIGTERM).is_ok()
}

/// Send graceful termination signal to a child process.
///
/// # Returns
/// - `false` always on Windows (no reliable graceful termination available)
///
/// On Windows there's no reliable SIGTERM equivalent for console applications.
/// We return false to indicate that graceful termination is not available,
/// and the caller should proceed with force kill.
#[cfg(windows)]
fn send_terminate_signal(_child: &Child) -> bool {
    false
}

/// Stop a child process with graceful termination and timeout.
///
/// 1. Sends SIGTERM (Unix) via `send_terminate_signal`
/// 2. Waits for process exit within `grace` period
/// 3. On timeout: force kills the process
async fn stop_child_with_grace(
    child: &mut Child,
    handle: &InstanceHandle,
    grace: Duration,
    context: &str,
) {
    let pid = child.id();
    let sent = send_terminate_signal(child);

    // Log with gear context if termination signal failed
    if !sent && pid.is_some() {
        tracing::debug!(
            gear =  %handle.gear,
            instance_id = %handle.instance_id,
            pid = ?pid,
            "{context}: graceful termination not available, will force kill"
        );
    }

    tracing::debug!(
        gear =  %handle.gear,
        instance_id = %handle.instance_id,
        pid = ?pid,
        graceful = sent,
        "{context}: sent termination signal"
    );

    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => {
            tracing::debug!(
                gear =  %handle.gear,
                instance_id = %handle.instance_id,
                status = ?status,
                "{context}: process exited gracefully"
            );
        }
        Ok(Err(e)) => {
            tracing::warn!(
                gear =  %handle.gear,
                instance_id = %handle.instance_id,
                error = %e,
                "{context}: failed to wait for process"
            );
        }
        Err(_) => {
            tracing::debug!(
                gear =  %handle.gear,
                instance_id = %handle.instance_id,
                "{context}: grace period expired, force killing"
            );
            if let Err(e) = child.kill().await {
                tracing::warn!(
                    gear =  %handle.gear,
                    instance_id = %handle.instance_id,
                    error = %e,
                    "{context}: failed to force kill"
                );
            }
        }
    }
}

/// Wait for a log forwarder task to finish with timeout.
async fn wait_forwarder(
    handle: Option<JoinHandle<()>>,
    gear: &str,
    instance_id: uuid::Uuid,
    stream: &str,
) {
    let Some(h) = handle else { return };
    match tokio::time::timeout(FORWARDER_DRAIN_TIMEOUT, h).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            if e.is_panic() {
                tracing::warn!(gear, %instance_id, stream, error = %e, "log forwarder task panicked");
            } else {
                tracing::warn!(gear, %instance_id, stream, error = %e, "log forwarder task cancelled");
            }
        }
        Err(_) => {
            tracing::warn!(
                gear,
                %instance_id,
                stream,
                timeout_ms = FORWARDER_DRAIN_TIMEOUT.as_millis(),
                "log forwarder did not finish within drain timeout",
            );
        }
    }
}

/// Internal representation of a local process instance
struct LocalInstance {
    handle: InstanceHandle,
    child: Child,
    /// Task handle for stdout log forwarder
    stdout_forwarder: Option<JoinHandle<()>>,
    /// Task handle for stderr log forwarder
    stderr_forwarder: Option<JoinHandle<()>>,
}

/// Map key type for instances - uses Uuid directly
type InstanceMap = HashMap<Uuid, LocalInstance>;

/// Backend that spawns gears as local child processes and manages their lifecycle.
///
/// When the cancellation token is triggered, the backend will:
/// 1. Send termination signal to all processes (SIGTERM on Unix, `TerminateProcess` on Windows)
/// 2. Wait up to 5 seconds for graceful shutdown
/// 3. Force kill any remaining processes
pub struct LocalProcessBackend {
    instances: Arc<RwLock<InstanceMap>>,
    cancel: CancellationToken,
    /// Shared shutdown task: cancellation and `stop_all` await it;
    /// dropping the backend triggers it. Dropping a waiter does not stop it.
    shutdown: Arc<Shutdown>,
    /// Cancelled on drop: the watcher then stops the children, although the
    /// caller's token, which the backend only borrows, stays live.
    dropped: CancellationToken,
}

/// See [`LocalProcessBackend::shutdown`].
struct Shutdown {
    instances: Arc<RwLock<InstanceMap>>,
    /// Set under the instances lock when shutdown takes them; refuses racing spawns.
    stopping: std::sync::atomic::AtomicBool,
    started: std::sync::atomic::AtomicBool,
    done: tokio::sync::watch::Sender<bool>,
}

impl Shutdown {
    /// Start the run once, on a task of its own, and wait until it ended.
    async fn run(self: &Arc<Self>) {
        self.run_with(|shutdown| {
            Box::pin(async move { LocalProcessBackend::shutdown_all_instances(&shutdown).await })
        })
        .await;
    }

    /// [`run`](Self::run) with `body` as the run: started once, and ended for
    /// every waiter however it ends, a panic included.
    async fn run_with(
        self: &Arc<Self>,
        body: impl FnOnce(Arc<Self>) -> futures_util::future::BoxFuture<'static, ()>,
    ) {
        let mut done = self.done.subscribe();
        if !self.started.swap(true, std::sync::atomic::Ordering::AcqRel) {
            let ended = RunEnded(Arc::clone(self));
            let run = body(Arc::clone(self));
            tokio::spawn(async move {
                let _ended = ended;
                run.await;
            });
        }
        drop(done.wait_for(|done| *done).await);
    }
}

/// Wake shutdown waiters on completion or panic.
struct RunEnded(Arc<Shutdown>);

impl Drop for RunEnded {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(
                "LocalProcessBackend: the shutdown run panicked; children it had not stopped may still run"
            );
        }
        self.0.done.send_replace(true);
    }
}

impl Drop for LocalProcessBackend {
    fn drop(&mut self) {
        self.dropped.cancel();
    }
}

impl LocalProcessBackend {
    /// Create a new `LocalProcessBackend` with the given cancellation token.
    ///
    /// When the token is cancelled, or the backend is dropped, all spawned
    /// processes will be gracefully stopped.
    #[must_use]
    pub fn new(cancel: CancellationToken) -> Self {
        let instances = Arc::new(RwLock::new(HashMap::new()));
        let backend = Self {
            instances: Arc::clone(&instances),
            cancel: cancel.clone(),
            shutdown: Arc::new(Shutdown {
                instances,
                stopping: std::sync::atomic::AtomicBool::new(false),
                started: std::sync::atomic::AtomicBool::new(false),
                done: tokio::sync::watch::Sender::new(false),
            }),
            dropped: CancellationToken::new(),
        };

        // Spawn background task to handle shutdown
        let shutdown = Arc::clone(&backend.shutdown);
        let dropped = backend.dropped.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = cancel.cancelled() => {
                    tracing::info!("LocalProcessBackend: shutdown signal received, stopping all processes");
                }
                () = dropped.cancelled() => {
                    tracing::info!("LocalProcessBackend: dropped, stopping all processes");
                }
            }
            shutdown.run().await;
        });

        backend
    }

    /// Gracefully stop every tracked instance, with the shutdown grace
    /// period, and wait until each ended — also when the cancellation
    /// watcher began that shutdown first. Later spawns are refused.
    pub async fn stop_all(&self) {
        self.shutdown.run().await;
    }

    /// Gracefully stop all tracked instances with timeout.
    async fn shutdown_all_instances(shutdown: &Shutdown) {
        let mut all_instances: Vec<LocalInstance> = {
            let mut guard = shutdown.instances.write();
            shutdown
                .stopping
                .store(true, std::sync::atomic::Ordering::Release);
            guard.drain().map(|(_, inst)| inst).collect()
        };

        if all_instances.is_empty() {
            return;
        }

        tracing::info!(count = all_instances.len(), "Stopping OoP gear processes");

        // Stop all processes with grace period, concurrently: the run takes one
        // grace period, not one per child.
        futures_util::future::join_all(all_instances.iter_mut().map(|inst| {
            stop_child_with_grace(
                &mut inst.child,
                &inst.handle,
                SHUTDOWN_GRACE_PERIOD,
                "shutdown",
            )
        }))
        .await;

        // Wait for forwarders to drain
        for inst in all_instances {
            wait_forwarder(
                inst.stdout_forwarder,
                &inst.handle.gear,
                inst.handle.instance_id,
                "stdout",
            )
            .await;
            wait_forwarder(
                inst.stderr_forwarder,
                &inst.handle.gear,
                inst.handle.instance_id,
                "stderr",
            )
            .await;
        }

        tracing::info!("All OoP gear processes stopped");
    }
}

#[async_trait]
impl GearRuntimeBackend for LocalProcessBackend {
    async fn spawn_instance(&self, cfg: &OopGearConfig) -> Result<InstanceHandle> {
        // Verify backend kind
        if cfg.backend != BackendKind::LocalProcess {
            bail!(
                "LocalProcessBackend can only spawn LocalProcess instances, got {:?}",
                cfg.backend
            );
        }

        // Ensure binary is set
        let binary = cfg
            .binary
            .as_ref()
            .context("executable_path must be set for LocalProcess backend")?;

        // Generate unique instance ID using UUID v7
        let instance_id = Uuid::now_v7();

        // Build command
        let mut cmd = Command::new(binary);
        cmd.args(&cfg.args);
        cmd.envs(&cfg.env);

        // A child whose handle is dropped unstopped is killed, never orphaned.
        cmd.kill_on_drop(true);

        // Pipe stdout/stderr for log forwarding
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Set working directory if specified
        if let Some(ref working_dir) = cfg.working_directory {
            let path = Path::new(working_dir);
            if path.exists() && path.is_dir() {
                cmd.current_dir(path);
            } else {
                tracing::warn!(
                    gear =  %cfg.name,
                    working_dir = %working_dir,
                    "Working directory does not exist or is not a directory, using current dir"
                );
            }
        }

        // Spawn the process
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn process: {}", binary.display()))?;

        // Get PID
        let pid = child.id();

        // Spawn log forwarder tasks for stdout/stderr with cancellation support
        let gear_name = cfg.name.clone();
        let cancel = self.cancel.clone();
        let stdout_forwarder = child.stdout.take().map(|stdout| {
            spawn_stream_forwarder(
                stdout,
                gear_name.clone(),
                instance_id,
                cancel.clone(),
                StreamKind::Stdout,
            )
        });
        let stderr_forwarder = child.stderr.take().map(|stderr| {
            spawn_stream_forwarder(
                stderr,
                gear_name.clone(),
                instance_id,
                cancel.clone(),
                StreamKind::Stderr,
            )
        });

        tracing::info!(
            gear =  %cfg.name,
            instance_id = %instance_id,
            pid = ?pid,
            "Spawned OoP gear with log forwarding"
        );

        // Create handle
        let handle = InstanceHandle {
            gear: cfg.name.clone(),
            instance_id,
            backend: BackendKind::LocalProcess,
            pid,
            created_at: std::time::Instant::now(),
        };

        // Store in instances map, unless the backend is shutting down.
        {
            let mut instances = self.instances.write();
            if self
                .shutdown
                .stopping
                .load(std::sync::atomic::Ordering::Acquire)
            {
                drop(instances);
                if let Err(e) = child.start_kill() {
                    tracing::warn!(gear = %cfg.name, error = %e, "failed to kill a process spawned during shutdown");
                }
                drop(child.wait().await);
                bail!(
                    "LocalProcessBackend is shutting down; {} was not started",
                    cfg.name
                );
            }
            instances.insert(
                instance_id,
                LocalInstance {
                    handle: handle.clone(),
                    child,
                    stdout_forwarder,
                    stderr_forwarder,
                },
            );
        }

        Ok(handle)
    }

    async fn stop_instance(&self, handle: &InstanceHandle) -> Result<()> {
        let local = {
            let mut instances = self.instances.write();
            instances.remove(&handle.instance_id)
        };

        if let Some(mut local) = local {
            stop_child_with_grace(
                &mut local.child,
                &local.handle,
                INSTANCE_STOP_GRACE_PERIOD,
                "stop_instance",
            )
            .await;

            // we do not await forwarders here, they'll stop on their own via CancellationToken and pipe close;
            // shutdown_all_instances handles draining for global shutdown
        } else {
            tracing::debug!(
                gear =  %handle.gear,
                instance_id = %handle.instance_id,
                "stop_instance called for unknown instance, ignoring"
            );
        }

        Ok(())
    }

    async fn list_instances(&self, gear: &str) -> Result<Vec<InstanceHandle>> {
        let instances = self.instances.read();

        let result = instances
            .values()
            .filter(|inst| inst.handle.gear == gear)
            .map(|inst| inst.handle.clone())
            .collect();

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    fn test_backend() -> LocalProcessBackend {
        LocalProcessBackend::new(CancellationToken::new())
    }

    #[tokio::test]
    async fn test_spawn_instance_requires_binary() {
        let backend = test_backend();
        let cfg = OopGearConfig::new("test_gear", BackendKind::LocalProcess);

        let result = backend.spawn_instance(&cfg).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("executable_path must be set")
        );
    }

    #[tokio::test]
    async fn test_spawn_instance_requires_correct_backend() {
        let backend = test_backend();
        let mut cfg = OopGearConfig::new("test_gear", BackendKind::K8s);
        cfg.binary = Some(PathBuf::from("/bin/echo"));

        let result = backend.spawn_instance(&cfg).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("can only spawn LocalProcess")
        );
    }

    #[tokio::test]
    async fn test_spawn_list_stop_lifecycle() {
        let backend = test_backend();

        // Create config with a valid binary that exists on most systems
        let mut cfg = OopGearConfig::new("test_gear", BackendKind::LocalProcess);

        // Use a simple command that exists cross-platform
        #[cfg(windows)]
        let binary = PathBuf::from("C:\\Windows\\System32\\cmd.exe");
        #[cfg(not(windows))]
        let binary = PathBuf::from("/bin/sleep");

        cfg.binary = Some(binary);
        cfg.args = vec!["10".to_owned()]; // sleep for 10 seconds

        // Spawn instance
        let handle = backend
            .spawn_instance(&cfg)
            .await
            .expect("should spawn instance");

        assert_eq!(handle.gear, "test_gear");
        assert!(!handle.instance_id.is_nil());
        assert_eq!(handle.backend, BackendKind::LocalProcess);

        // List instances
        let instances = backend
            .list_instances("test_gear")
            .await
            .expect("should list instances");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].gear, "test_gear");
        assert_eq!(instances[0].instance_id, handle.instance_id);

        // Stop instance
        backend
            .stop_instance(&handle)
            .await
            .expect("should stop instance");

        // Verify it's removed
        let instances = backend
            .list_instances("test_gear")
            .await
            .expect("should list instances");
        assert_eq!(instances.len(), 0);
    }

    #[tokio::test]
    async fn test_list_instances_filters_by_gear() {
        let backend = test_backend();

        #[cfg(windows)]
        let binary = PathBuf::from("C:\\Windows\\System32\\cmd.exe");
        #[cfg(not(windows))]
        let binary = PathBuf::from("/bin/sleep");

        // Spawn instance for gear_a
        let mut cfg_a = OopGearConfig::new("gear_a", BackendKind::LocalProcess);
        cfg_a.binary = Some(binary.clone());
        cfg_a.args = vec!["10".to_owned()];

        let handle_a = backend
            .spawn_instance(&cfg_a)
            .await
            .expect("should spawn gear_a");

        // Spawn instance for gear_b
        let mut cfg_b = OopGearConfig::new("gear_b", BackendKind::LocalProcess);
        cfg_b.binary = Some(binary);
        cfg_b.args = vec!["10".to_owned()];

        let handle_b = backend
            .spawn_instance(&cfg_b)
            .await
            .expect("should spawn gear_b");

        // List gear_a instances
        let instances_a = backend
            .list_instances("gear_a")
            .await
            .expect("should list gear_a");
        assert_eq!(instances_a.len(), 1);
        assert_eq!(instances_a[0].gear, "gear_a");

        // List gear_b instances
        let instances_b = backend
            .list_instances("gear_b")
            .await
            .expect("should list gear_b");
        assert_eq!(instances_b.len(), 1);
        assert_eq!(instances_b[0].gear, "gear_b");

        // Clean up
        backend.stop_instance(&handle_a).await.ok();
        backend.stop_instance(&handle_b).await.ok();
    }

    #[tokio::test]
    async fn test_stop_nonexistent_instance() {
        let backend = test_backend();
        let handle = InstanceHandle {
            gear: "test_gear".to_owned(),
            instance_id: Uuid::new_v4(),
            backend: BackendKind::LocalProcess,
            pid: None,
            created_at: Instant::now(),
        };

        // Should not error even if instance doesn't exist
        let result = backend.stop_instance(&handle).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_instances_empty() {
        let backend = test_backend();
        let instances = backend
            .list_instances("nonexistent_gear")
            .await
            .expect("should list instances");
        assert_eq!(instances.len(), 0);
    }

    mod send_terminate_signal_tests {
        #[cfg(unix)]
        use {super::send_terminate_signal, std::time::Duration};

        #[cfg(unix)]
        #[tokio::test]
        async fn test_send_terminate_signal_to_valid_process() {
            // Spawn a long-running process
            let mut cmd = tokio::process::Command::new("sleep");
            cmd.args(["30"]);

            let mut child = cmd.spawn().expect("should spawn test process");

            // Send termination signal
            let result = send_terminate_signal(&child);

            // Should return true indicating signal was sent
            assert!(result, "Should successfully send SIGTERM to valid process");

            // Wait briefly for graceful shutdown
            tokio::time::timeout(Duration::from_secs(1), child.wait())
                .await
                .expect("process should exit within timeout")
                .expect("wait should succeed");
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn test_send_terminate_signal_to_exited_process() {
            // Spawn a process that exits immediately using sh -c 'exit 0'
            // This works on all Unix systems (Linux, macOS, BSD)
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.args(["-c", "exit 0"]);
            let mut child = cmd.spawn().expect("should spawn test process");

            // Wait for it to exit
            tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .expect("process should exit within timeout")
                .expect("wait should succeed");

            // Try to send termination signal to exited process
            let result = send_terminate_signal(&child);

            // Should return false because PID is no longer available
            assert!(!result, "Should return false for already-exited process");
        }

        #[cfg(unix)]
        #[test]
        fn test_pid_conversion_edge_case_documentation() {
            // This test documents the edge case behavior for PIDs > i32::MAX
            // In practice, this is extremely rare as it would require:
            // 1. System uptime of weeks/months without reboot
            // 2. PID counter to wrap around multiple times
            // 3. Specific kernel configuration

            // The maximum value a u32 PID can have
            let max_u32_pid: u32 = u32::MAX;

            // This would fail to convert to i32
            let result = i32::try_from(max_u32_pid);
            assert!(result.is_err(), "u32::MAX should not fit in i32");

            // Our code handles this by logging a warning and returning false
            // preventing the dangerous unwrap_or(0) that would signal PID 0
        }

        #[cfg(unix)]
        #[test]
        fn test_pid_conversion_normal_range() {
            // Test that normal PIDs convert successfully
            let normal_pid: u32 = 12345;
            let result = i32::try_from(normal_pid);
            assert!(result.is_ok(), "Normal PID should convert to i32");
            assert_eq!(result.unwrap(), 12345);
        }
    }

    /// A process ignoring SIGTERM, for the grace-period paths, and the marker
    /// file it creates once it ignores it: see [`ignoring_sigterm`].
    #[cfg(unix)]
    fn stubborn() -> (OopGearConfig, PathBuf) {
        let marker = std::env::temp_dir().join(format!("stubborn-{}", Uuid::new_v4()));
        let mut cfg = OopGearConfig::new("stubborn", BackendKind::LocalProcess);
        cfg.binary = Some(PathBuf::from("/bin/sh"));
        // `exec`: the tracked process is the one ignoring SIGTERM, with no
        // untracked grandchild left behind by a kill. An ignored signal stays
        // ignored across `exec`.
        cfg.args = vec![
            "-c".to_owned(),
            r#"trap '' TERM; : > "$1"; exec sleep 60"#.to_owned(),
            "stubborn".to_owned(),
            marker.display().to_string(),
        ];
        (cfg, marker)
    }

    /// Wait until the stubborn child ignores SIGTERM, then remove its marker.
    #[cfg(unix)]
    async fn ignoring_sigterm(marker: &std::path::Path) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stubborn child never installed its SIGTERM trap"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::remove_file(marker).ok();
    }

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }

    /// `stop_all` racing the cancellation watcher returns only once the child
    /// ended, though the watcher took the instances first.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_all_waits_for_a_shutdown_the_watcher_began() {
        let cancel = CancellationToken::new();
        let backend = LocalProcessBackend::new(cancel.clone());
        let (stubborn_cfg, marker) = stubborn();
        let handle = backend.spawn_instance(&stubborn_cfg).await.expect("spawn");
        let pid = handle.pid.expect("a pid");
        ignoring_sigterm(&marker).await;

        cancel.cancel();
        while backend.instances.read().contains_key(&handle.instance_id) {
            tokio::task::yield_now().await;
        }
        backend.stop_all().await;

        assert!(!alive(pid), "the child ended before stop_all returned");
        assert!(
            backend.spawn_instance(&stubborn().0).await.is_err(),
            "no spawn after shutdown"
        );
    }

    /// Dropping the backend stops its children though the caller's token,
    /// which it only borrows, stays live.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_backend_stops_its_children() {
        let cancel = CancellationToken::new();
        let backend = LocalProcessBackend::new(cancel.clone());
        let mut cfg = OopGearConfig::new("sleeper", BackendKind::LocalProcess);
        cfg.binary = Some(PathBuf::from("/bin/sleep"));
        cfg.args = vec!["60".to_owned()];
        let pid = backend
            .spawn_instance(&cfg)
            .await
            .expect("spawn")
            .pid
            .expect("a pid");

        drop(backend);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while alive(pid) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the child outlived the backend"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!cancel.is_cancelled());
    }

    /// A `stop_all` abandoned mid-run — its future dropped while the stubborn
    /// child sits in its grace period — and then the backend dropped: the run
    /// belongs to its own task, so the child still ends.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_abandoned_stop_all_still_stops_the_child() {
        let cancel = CancellationToken::new();
        let backend = LocalProcessBackend::new(cancel.clone());
        let (stubborn_cfg, marker) = stubborn();
        let handle = backend.spawn_instance(&stubborn_cfg).await.expect("spawn");
        let pid = handle.pid.expect("a pid");
        ignoring_sigterm(&marker).await;

        let stopping = tokio::time::timeout(Duration::from_millis(200), backend.stop_all()).await;
        assert!(
            stopping.is_err(),
            "abandoned while the child ignores SIGTERM"
        );
        assert!(
            backend.instances.read().is_empty(),
            "the run took the instances"
        );
        drop(backend);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while alive(pid) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the child outlived the abandoned stop"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!cancel.is_cancelled());
    }
    /// A shutdown run that panics still ends every wait for it.
    #[tokio::test]
    async fn a_panicking_shutdown_run_releases_its_waiters() {
        let shutdown = Arc::new(Shutdown {
            instances: Arc::new(RwLock::new(HashMap::new())),
            stopping: std::sync::atomic::AtomicBool::new(false),
            started: std::sync::atomic::AtomicBool::new(false),
            done: tokio::sync::watch::Sender::new(false),
        });

        let first = tokio::time::timeout(
            Duration::from_secs(5),
            shutdown.run_with(|_| Box::pin(async { panic!("the run failed") })),
        )
        .await;
        let second = tokio::time::timeout(Duration::from_secs(5), shutdown.run()).await;

        assert!(first.is_ok(), "the waiter of the panicking run returns");
        assert!(second.is_ok(), "a later waiter returns too");
    }
}
