//! Engine construction and lifecycle.
//!
//! An embedding gear owns one [`Engine`] per database. The lifecycle has three
//! phases, each consuming the previous one:
//!
//! 1. [`Engine::builder`] … [`EngineBuilder::build`] validates settings. No I/O.
//!    The returned engine already hands out [`ExecutionClient`], [`WorkflowRegistry`]
//!    and [`ExecutionInspector`]; they stay valid for the life of the process.
//! 2. [`Engine::prepare`] checks handler bindings and, with a queue, connects to
//!    it and starts delivery. Call it once all gears are initialized. On failure the
//!    engine is handed back in the [`PrepareError`], so the call can be retried.
//! 3. [`Runner::run`] serves until the cancellation token fires, then drains
//!    within [`Engine::shutdown_timeout`].
use std::collections::BTreeMap;
use std::fmt;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use tokio_util::sync::CancellationToken;

use crate::authorization::{Authority, Authorizer};
use crate::catalog::registrar::Registrar;
use crate::catalog::registry::Registry;
use crate::client::service::Service;
use crate::client::{ExecutionClient, ExecutionInspector, WorkflowRegistry};
use crate::config::Config;
use crate::store::JournalStore;
use control::control_loop;
use executor::Executor;
use runtime::{Runtime, RuntimeError};

/// Shared periodic control loop and error triage.
pub mod control;
/// Claims runs and executes their activity handlers.
pub mod executor;
/// Activity retry scheduling.
pub mod retry;
/// Queue connection, delivery and worker supervision.
pub mod runtime;
/// Shutdown budget shared by the draining components.
pub mod shutdown;

/// What this process does with durable work.
///
/// Every mode observes runs and manages the catalog. Operations that schedule work
/// (start, retry, resume, and abort of a saga) need delivery, so they require
/// [`Self::Dispatcher`] or [`Self::Worker`] in the calling process.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ExecutionMode {
    /// Observation, cancellation and catalog reconciliation only. Nothing is
    /// published or executed; scheduling operations fail with
    /// [`ErrorKind::FailedPrecondition`](crate::ErrorKind::FailedPrecondition) and
    /// code [`DELIVERY_DISABLED`](crate::codes::DELIVERY_DISABLED). The default.
    ControlPlane,
    /// Admits and schedules runs and publishes due work to the queue, without
    /// executing activities. Until [`Engine::prepare`] succeeds, scheduling
    /// operations fail with [`ErrorKind::Unavailable`](crate::ErrorKind::Unavailable).
    Dispatcher(QueueConfig),
    /// Like [`Self::Dispatcher`], and also executes activities with the
    /// registered handlers.
    Worker(QueueConfig),
}

/// Connection and routing of the PostgreSQL work queue.
///
/// Queues live in the shared `apalis` schema of the database named by the DSN, so
/// every queue name must be unique among all engines that use that database. Name
/// queues after the embedding gear, for example `orders-durable`.
#[derive(Clone)]
pub struct QueueConfig {
    dsn: SecretString,
    default_queue: String,
    queues: BTreeMap<String, usize>,
    routes: BTreeMap<String, String>,
}

impl QueueConfig {
    /// Concurrency of the default queue unless [`Self::concurrency`] changes it.
    pub const DEFAULT_CONCURRENCY: NonZeroUsize = NonZeroUsize::new(2).unwrap();

    /// Queue at the PostgreSQL `dsn` whose default queue is `queue`, used for
    /// definitions without a [route](Self::route).
    #[must_use]
    pub fn new(dsn: SecretString, queue: impl Into<String>) -> Self {
        let queue = queue.into();
        Self {
            dsn,
            queues: [(queue.clone(), Self::DEFAULT_CONCURRENCY.get())].into(),
            default_queue: queue,
            routes: BTreeMap::new(),
        }
    }

    /// Workers of the default queue. Default [`Self::DEFAULT_CONCURRENCY`].
    #[must_use]
    pub fn concurrency(mut self, concurrency: NonZeroUsize) -> Self {
        self.queues
            .insert(self.default_queue.clone(), concurrency.get());
        self
    }

    /// Declare an additional queue with `concurrency` workers.
    #[must_use]
    pub fn queue(mut self, name: impl Into<String>, concurrency: NonZeroUsize) -> Self {
        self.queues.insert(name.into(), concurrency.get());
        self
    }

    /// Route the exact versioned `definition` name to a declared `queue`.
    #[must_use]
    pub fn route(mut self, definition: impl Into<String>, queue: impl Into<String>) -> Self {
        self.routes.insert(definition.into(), queue.into());
        self
    }
}

impl fmt::Debug for QueueConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueueConfig")
            .field("dsn", &"[REDACTED]")
            .field("default_queue", &self.default_queue)
            .field("queues", &self.queues)
            .field("routes", &self.routes)
            .finish()
    }
}

/// Invalid engine settings, reported by [`EngineBuilder::build`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid durable execution setting `{setting}`: {reason}")]
pub struct ConfigError {
    setting: &'static str,
    reason: &'static str,
}

impl ConfigError {
    /// Name of the builder setting, for example `heartbeat` or `queues`.
    #[must_use]
    pub fn setting(&self) -> &'static str {
        self.setting
    }

    /// The violated rule.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

/// Category of a [`LifecycleError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LifecycleErrorKind {
    /// A registered definition lacks handlers for some of its activities.
    IncompleteBindings,
    /// The queue database cannot be reached or prepared.
    QueueUnavailable,
    /// Journal storage or delivery failed; for the control plane after
    /// [`EngineBuilder::control_plane_failure_limit`] consecutive failures.
    Storage,
    /// Work did not drain within [`EngineBuilder::shutdown_timeout`]. Unfinished
    /// claims are recovered after their lease expires.
    ShutdownTimedOut,
    /// A queue worker stopped unexpectedly.
    Worker,
}

/// Failure of [`Engine::prepare`] or [`Runner::run`]. The engine cannot continue;
/// return it from the gear lifecycle so the host stops or restarts the process.
///
/// When a component fails and the drain then fails too, the first failure is
/// returned and the later one is logged.
#[derive(Debug, thiserror::Error)]
#[error("durable execution {}", self.describe())]
pub struct LifecycleError {
    kind: LifecycleErrorKind,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl LifecycleError {
    /// The failure category.
    #[must_use]
    pub fn kind(&self) -> LifecycleErrorKind {
        self.kind
    }

    fn describe(&self) -> &'static str {
        match self.kind {
            LifecycleErrorKind::IncompleteBindings => "handlers are incomplete",
            LifecycleErrorKind::QueueUnavailable => "queue is unavailable",
            LifecycleErrorKind::Storage => "storage failed",
            LifecycleErrorKind::ShutdownTimedOut => "shutdown timed out",
            LifecycleErrorKind::Worker => "worker stopped",
        }
    }
}

impl From<RuntimeError> for LifecycleError {
    fn from(error: RuntimeError) -> Self {
        let (kind, source) = match error {
            RuntimeError::Bindings(source) => {
                (LifecycleErrorKind::IncompleteBindings, Some(source.into()))
            }
            RuntimeError::Queue(source) => (LifecycleErrorKind::QueueUnavailable, Some(source)),
            RuntimeError::Storage(source) => (LifecycleErrorKind::Storage, Some(source)),
            RuntimeError::Worker(source) => (LifecycleErrorKind::Worker, Some(source)),
            RuntimeError::ShutdownTimedOut => (LifecycleErrorKind::ShutdownTimedOut, None),
        };
        Self { kind, source }
    }
}

/// Failure of [`Engine::prepare`] that hands the engine back, so a transient
/// failure such as [`LifecycleErrorKind::QueueUnavailable`] can be retried without
/// rebuilding the engine and registering the workflows again.
#[must_use = "the engine is returned inside the error"]
#[derive(thiserror::Error)]
#[error("{error}")]
pub struct PrepareError {
    #[source]
    error: LifecycleError,
    engine: Box<Engine>,
}

impl PrepareError {
    /// The failure category.
    #[must_use]
    pub fn kind(&self) -> LifecycleErrorKind {
        self.error.kind()
    }

    /// The failure and the unchanged engine.
    #[must_use]
    pub fn into_parts(self) -> (LifecycleError, Engine) {
        (self.error, *self.engine)
    }
}

impl fmt::Debug for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrepareError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// Settings for an [`Engine`]. Every setting has a safe default; values are
/// validated by [`Self::build`].
#[must_use]
pub struct EngineBuilder {
    db: toolkit_db::Db,
    authorizer: Arc<dyn Authorizer>,
    mode: ExecutionMode,
    lease: Duration,
    heartbeat: Duration,
    dispatch_interval: Duration,
    shutdown_timeout: Duration,
    control_plane_failure_limit: NonZeroU32,
    authorization_timeout: Duration,
}

impl EngineBuilder {
    /// Default [`Self::lease`].
    pub const DEFAULT_LEASE: Duration = Duration::from_secs(120);
    /// Default [`Self::heartbeat`].
    pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(20);
    /// Default [`Self::dispatch_interval`].
    pub const DEFAULT_DISPATCH_INTERVAL: Duration = Duration::from_secs(5);
    /// Default [`Self::shutdown_timeout`].
    pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
    /// Default [`Self::control_plane_failure_limit`].
    pub const DEFAULT_CONTROL_PLANE_FAILURE_LIMIT: NonZeroU32 = NonZeroU32::new(5).unwrap();
    /// Default [`Self::authorization_timeout`].
    pub const DEFAULT_AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(5);

    /// What this process does with durable work. Default [`ExecutionMode::ControlPlane`].
    pub fn mode(mut self, mode: ExecutionMode) -> Self {
        self.mode = mode;
        self
    }

    /// How long a claimed activity stays owned without a heartbeat. Whole seconds,
    /// at most one day, more than three heartbeats. Default 120 seconds.
    pub fn lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// Interval between lease renewals of a running activity. Whole seconds,
    /// less than a third of the lease. Default 20 seconds.
    pub fn heartbeat(mut self, heartbeat: Duration) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    /// Interval of catalog reconciliation, recovery and dispatch. Whole seconds
    /// within one day. Default 5 seconds.
    pub fn dispatch_interval(mut self, interval: Duration) -> Self {
        self.dispatch_interval = interval;
        self
    }

    /// Total time to drain workers and delivery after cancellation. Whole seconds
    /// within one day. Default 30 seconds.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Consecutive storage failures of the control plane before [`Runner::run`]
    /// fails with [`LifecycleErrorKind::Storage`]. At most 128. Default 5.
    /// Authorization failures pause background work and do not count.
    pub fn control_plane_failure_limit(mut self, limit: NonZeroU32) -> Self {
        self.control_plane_failure_limit = limit;
        self
    }

    /// Upper bound of each [`Authorizer`] call; a timeout counts as unavailable
    /// authorization. Positive. Default 5 seconds.
    pub fn authorization_timeout(mut self, timeout: Duration) -> Self {
        self.authorization_timeout = timeout;
        self
    }

    /// Validate the settings and build the engine. Performs no I/O.
    ///
    /// # Errors
    /// [`ConfigError`] naming the first invalid setting.
    pub fn build(self) -> Result<Engine, ConfigError> {
        let invalid = |setting, reason| ConfigError { setting, reason };
        let whole_secs = |setting, value: Duration| {
            if value.subsec_nanos() == 0 {
                Ok(value.as_secs())
            } else {
                Err(invalid(setting, "must be a whole number of seconds"))
            }
        };
        if self.authorization_timeout.is_zero() {
            return Err(invalid("authorization_timeout", "must be positive"));
        }
        let (execute_activities, queue) = match self.mode {
            ExecutionMode::ControlPlane => (false, None),
            ExecutionMode::Dispatcher(queue) => (false, Some(queue)),
            ExecutionMode::Worker(queue) => (true, Some(queue)),
        };
        let (queue_dsn, default_queue, queues, definition_queues) = match queue {
            Some(QueueConfig {
                dsn,
                default_queue,
                queues,
                routes,
            }) => (Some(dsn), default_queue, queues, routes),
            // Without delivery no queue is used; a placeholder keeps validation uniform.
            None => (
                None,
                "control-plane".to_owned(),
                [("control-plane".to_owned(), 1)].into(),
                BTreeMap::new(),
            ),
        };
        let config = Config {
            execute_activities,
            default_queue,
            queues,
            definition_queues,
            lease_secs: whole_secs("lease", self.lease)?,
            heartbeat_secs: whole_secs("heartbeat", self.heartbeat)?,
            dispatch_interval_secs: whole_secs("dispatch_interval", self.dispatch_interval)?,
            control_plane_failure_limit: self.control_plane_failure_limit.get(),
            shutdown_timeout_secs: whole_secs("shutdown_timeout", self.shutdown_timeout)?,
        };
        config
            .validate()
            .map_err(|(setting, reason)| invalid(setting, reason))?;

        let authority = Authority::new(self.authorizer, self.authorization_timeout);
        let db = Arc::new(toolkit_db::DBProvider::new(self.db));
        let mut store = JournalStore::new(db).with_authorization(Arc::new(authority.clone()));
        if queue_dsn.is_some() {
            store = store.with_delivery();
        }
        let registry = Arc::new(Registry::default());
        Ok(Engine {
            service: Arc::new(Service {
                store: store.clone(),
                authority,
            }),
            registrar: Arc::new(Registrar {
                store: store.clone(),
                registry: registry.clone(),
            }),
            executor: Arc::new(Executor {
                store,
                registry,
                config,
            }),
            queue_dsn,
            readiness: Readiness::default(),
        })
    }
}

impl fmt::Debug for EngineBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineBuilder")
            .field("mode", &self.mode)
            .field("lease", &self.lease)
            .field("heartbeat", &self.heartbeat)
            .field("dispatch_interval", &self.dispatch_interval)
            .field("shutdown_timeout", &self.shutdown_timeout)
            .field(
                "control_plane_failure_limit",
                &self.control_plane_failure_limit,
            )
            .field("authorization_timeout", &self.authorization_timeout)
            .finish_non_exhaustive()
    }
}

/// A configured durable-execution engine that has not started serving.
///
/// See the [crate documentation](crate) for the lifecycle.
pub struct Engine {
    executor: Arc<Executor>,
    service: Arc<Service>,
    registrar: Arc<Registrar>,
    queue_dsn: Option<SecretString>,
    readiness: Readiness,
}

impl Engine {
    /// Start configuring an engine on `db`, authorizing through `authorizer`.
    ///
    /// The database must contain the schema from [`crate::migrations`].
    pub fn builder(db: toolkit_db::Db, authorizer: Arc<dyn Authorizer>) -> EngineBuilder {
        EngineBuilder {
            db,
            authorizer,
            mode: ExecutionMode::ControlPlane,
            lease: EngineBuilder::DEFAULT_LEASE,
            heartbeat: EngineBuilder::DEFAULT_HEARTBEAT,
            dispatch_interval: EngineBuilder::DEFAULT_DISPATCH_INTERVAL,
            shutdown_timeout: EngineBuilder::DEFAULT_SHUTDOWN_TIMEOUT,
            control_plane_failure_limit: EngineBuilder::DEFAULT_CONTROL_PLANE_FAILURE_LIMIT,
            authorization_timeout: EngineBuilder::DEFAULT_AUTHORIZATION_TIMEOUT,
        }
    }

    /// Client for starting, observing and controlling runs.
    #[must_use]
    pub fn client(&self) -> ExecutionClient {
        ExecutionClient::new(self.service.clone())
    }

    /// Trusted workflow catalog management.
    #[must_use]
    pub fn registry(&self) -> WorkflowRegistry {
        WorkflowRegistry::new(self.registrar.clone())
    }

    /// Administrative progress reads.
    #[must_use]
    pub fn inspector(&self) -> ExecutionInspector {
        ExecutionInspector::new(self.service.clone())
    }

    /// Serving state for the gear's health check.
    #[must_use]
    pub fn readiness(&self) -> Readiness {
        self.readiness.clone()
    }

    /// Configured drain budget; use it as the gear's stop timeout.
    #[must_use]
    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.executor.config.shutdown_timeout_secs)
    }

    /// Check bindings and, with a queue, connect to it, prepare its schema and
    /// start delivery. Call after the workflows of this process are registered;
    /// later registrations bind late and are still delivered.
    ///
    /// # Errors
    /// A [`PrepareError`] carrying the engine:
    /// [`LifecycleErrorKind::IncompleteBindings`] for a definition with missing
    /// handlers, [`LifecycleErrorKind::QueueUnavailable`] or
    /// [`LifecycleErrorKind::Storage`] when the queue or delivery cannot start.
    pub async fn prepare(self) -> Result<Runner, PrepareError> {
        let prepared = match &self.queue_dsn {
            Some(dsn) => Runtime::prepare(self.executor.clone(), dsn.expose_secret())
                .await
                .map(Some),
            None => self
                .executor
                .registry
                .validate_bindings()
                .map(|()| None)
                .map_err(RuntimeError::Bindings),
        };
        match prepared {
            Ok(runtime) => Ok(Runner {
                executor: self.executor,
                runtime,
                readiness: self.readiness,
            }),
            Err(error) => Err(PrepareError {
                error: error.into(),
                engine: Box::new(self),
            }),
        }
    }
}

#[cfg(test)]
impl Engine {
    /// Replace the background authorization of the executor; tests use it to
    /// simulate storage and authorization faults.
    pub(crate) fn with_worker_authorization(
        mut self,
        authorization: Arc<dyn crate::authorization::worker::WorkerAuthorization>,
    ) -> Self {
        let executor = Arc::get_mut(&mut self.executor).expect("engine executor is not shared");
        executor.store = executor.store.clone().with_authorization(authorization);
        self
    }
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("queue", &self.queue_dsn.as_ref().map(|_| "[REDACTED]"))
            .field("ready", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

/// A prepared engine. Serve it with [`Self::run`].
#[must_use = "call Runner::run to serve the engine"]
pub struct Runner {
    executor: Arc<Executor>,
    runtime: Option<Runtime>,
    readiness: Readiness,
}

impl Runner {
    /// Serve until `cancel` fires, then drain within the shutdown timeout.
    ///
    /// Spawns the queue workers, the dispatcher and delivery as tasks of the
    /// current runtime and owns them: they stop before this future completes,
    /// and dropping the future aborts them. [`Readiness::is_ready`] is `true`
    /// while it serves and turns `false` as soon as `cancel` fires or a component
    /// fails. Authorization failures pause background work instead of failing it.
    ///
    /// # Errors
    /// A [`LifecycleError`] when storage keeps failing, a worker stops or the
    /// drain exceeds the shutdown timeout. The failure that stopped the engine is
    /// returned; a later drain failure is logged.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), LifecycleError> {
        let local_stop = cancel.child_token();
        let _serving = ReadyGuard::serve(&self.readiness, local_stop.clone());
        let executor = &self.executor;
        // The runtime owns catalog reconciliation when delivery is enabled;
        // the metadata-only controller supplies it otherwise.
        let runtime_controls_catalog = self.runtime.is_some();
        let controller = async {
            if runtime_controls_catalog {
                local_stop.cancelled().await;
                return Ok(());
            }
            control_loop(
                &executor.config,
                tokio::time::MissedTickBehavior::Delay,
                &local_stop,
                || executor.reconcile_definitions(),
            )
            .await
            .map_err(|error| LifecycleError::from(RuntimeError::Storage(error.into())))
        };
        let execution = async {
            if let Some(runtime) = self.runtime {
                runtime
                    .run(local_stop.clone())
                    .await
                    .map_err(LifecycleError::from)
            } else {
                local_stop.cancelled().await;
                Ok(())
            }
        };
        tokio::pin!(controller, execution);
        let (control_done, first) = tokio::select! {
            result = &mut controller => (true, result),
            result = &mut execution => (false, result),
        };
        local_stop.cancel();
        let later = if control_done {
            execution.await
        } else {
            controller.await
        };
        match (first, later) {
            (Err(first), Err(later)) => {
                tracing::warn!(error = %later, "durable execution drain also failed");
                Err(first)
            }
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

impl fmt::Debug for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runner")
            .field("delivery", &self.runtime.is_some())
            .field("ready", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

/// Whether [`Runner::run`] is serving. Cheap to clone; all clones observe the
/// same engine.
#[derive(Clone, Default)]
pub struct Readiness(Arc<ReadinessState>);

#[derive(Default)]
struct ReadinessState {
    serving: AtomicBool,
    stop: OnceLock<CancellationToken>,
}

impl Readiness {
    /// `true` while the engine serves; `false` before, after, and from the moment
    /// shutdown starts, including while draining.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.0.serving.load(Ordering::Acquire)
            && self.0.stop.get().is_none_or(|stop| !stop.is_cancelled())
    }
}

impl fmt::Debug for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Readiness").field(&self.is_ready()).finish()
    }
}

/// Marks the engine as serving until `stop` fires, and clears it on every exit
/// path of [`Runner::run`].
struct ReadyGuard(Arc<ReadinessState>);

impl ReadyGuard {
    fn serve(readiness: &Readiness, stop: CancellationToken) -> Self {
        // A runner is consumed by `run`, so the token is installed once per engine.
        readiness.0.stop.get_or_init(|| stop);
        readiness.0.serving.store(true, Ordering::Release);
        Self(readiness.0.clone())
    }
}

impl Drop for ReadyGuard {
    fn drop(&mut self) {
        self.0.serving.store(false, Ordering::Release);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/engine_tests.rs"]
mod tests;
