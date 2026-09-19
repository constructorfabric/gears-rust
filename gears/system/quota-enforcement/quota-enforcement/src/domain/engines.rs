//! Static engine registration and bounded immutable artifact storage.
//! Active pointers never enter the cache. Compilation is the caller's work,
//! outside database transactions and outside this module's short mutex sections.
use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;

use parking_lot::Mutex;
use quota_enforcement_sdk::engine::{EngineError, EvaluationFailure};
use quota_enforcement_sdk::{PolicyId, QuotaResolutionEngineV1, ValidatedConfig};

use super::error::DomainError;
use toolkit_macros::domain_model;

/// Duplicate static registration is a bootstrap failure, never an overwrite.
#[domain_model]
#[derive(Debug, thiserror::Error)]
#[error("duplicate engine registration: {engine_id}")]
pub struct DuplicateEngine {
    /// Bounded engine identifier returned by the linked implementation.
    pub engine_id: &'static str,
}

/// Immutable registry assembled before serving traffic.
// @cpt-dod:cpt-cf-quota-enforcement-dod-engine-registry:p1
#[domain_model]
pub struct EngineRegistry {
    engines: BTreeMap<&'static str, Arc<dyn QuotaResolutionEngineV1>>,
}

impl EngineRegistry {
    /// Register the deployment's statically linked implementations exactly once.
    ///
    /// # Errors
    /// Returns a duplicate-ID error rather than replacing an existing engine.
    pub fn new(
        engines: impl IntoIterator<Item = Arc<dyn QuotaResolutionEngineV1>>,
    ) -> Result<Self, DuplicateEngine> {
        let mut registered = BTreeMap::new();
        for engine in engines {
            let id = engine.id();
            if registered.insert(id, engine).is_some() {
                return Err(DuplicateEngine { engine_id: id });
            }
        }
        Ok(Self {
            engines: registered,
        })
    }

    /// Resolve without allocating or accepting runtime registrations.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn QuotaResolutionEngineV1>> {
        self.engines.get(id)
    }

    /// Registered IDs in deterministic order, suitable for `UNKNOWN_ENGINE` detail.
    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.engines.keys().copied()
    }
}

/// Serialises preparation of one version, and counts who still wants it.
///
/// The count is explicit rather than inferred from reference counts: a caller
/// waiting for the lock is holding a reference through a pending future, so
/// reference counts cannot distinguish "someone is still waiting" from "a
/// pending future happens to exist", and a waiter cancelled before it acquires
/// the lock would leave the entry behind.
struct GateEntry {
    /// Shared so the waiter's guard owns the lock outright and can be moved
    /// into the compilation job.
    lock: Arc<tokio::sync::Mutex<()>>,
    users: std::sync::atomic::AtomicUsize,
}

/// Async, because the lock is held across the compile and must not park a
/// runtime worker.
type CompileGate = Arc<GateEntry>;

/// Ownership of one version's compile gate, and of removing it afterwards.
///
/// Claiming is separate from acquiring: the claim is registered before the
/// wait begins, so every exit removes the entry when it was the last claim,
/// whether the caller compiled, found the artifact already published, or was
/// cancelled while still queued behind someone else.
struct GateGuard {
    cache: Arc<PolicyArtifactCache>,
    key: (PolicyId, u32),
    gate: CompileGate,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl GateGuard {
    /// Register a claim on this version's gate. Synchronous on purpose: it
    /// completes before the first await, so there is no window in which a
    /// caller wants the gate without being counted.
    fn claim(cache: &Arc<PolicyArtifactCache>, key: (PolicyId, u32)) -> Self {
        let gate = cache.gate(&key);
        Self {
            cache: Arc::clone(cache),
            key,
            gate,
            guard: None,
        }
    }

    /// Claim the gate, then wait for it.
    async fn enter(cache: &Arc<PolicyArtifactCache>, key: (PolicyId, u32)) -> Self {
        let mut claim = Self::claim(cache, key);
        claim.guard = Some(Arc::clone(&claim.gate.lock).lock_owned().await);
        claim
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        // The lock first, so the next claimant finds the gate free.
        drop(self.guard.take());
        self.cache.release_gate(&self.key, &self.gate);
    }
}

#[domain_model]
#[derive(Default)]
struct Artifacts {
    entries: BTreeMap<(PolicyId, u32), Arc<dyn ValidatedConfig>>,
    insertion_order: VecDeque<(PolicyId, u32)>,
}

/// FIFO-bounded immutable cache; an invocation pins its artifact with an Arc.
///
/// It also owns the gate that keeps duplicate work out of itself. Preparation
/// is process-wide, so the per-version locks and the compilation permits live
/// with the entries they guard rather than with the short-lived driver that
/// takes them.
#[domain_model]
pub struct PolicyArtifactCache {
    capacity: NonZeroUsize,
    artifacts: Mutex<Artifacts>,
    /// One lock per `(policy_id, version)` being compiled. Held across the
    /// compile, so it is an async lock: no runtime worker is parked on it.
    compiling: Mutex<BTreeMap<(PolicyId, u32), CompileGate>>,
    /// How many compilations may run at once, whatever they are compiling.
    permits: Arc<tokio::sync::Semaphore>,
}

impl PolicyArtifactCache {
    /// Bound resident entries independently of persisted schema storage, and
    /// bound how many artifacts may be compiled at once.
    #[must_use]
    pub fn new(capacity: NonZeroUsize, preparation_concurrency: NonZeroUsize) -> Self {
        Self {
            capacity,
            artifacts: Mutex::new(Artifacts::default()),
            compiling: Mutex::new(BTreeMap::new()),
            permits: Arc::new(tokio::sync::Semaphore::new(preparation_concurrency.get())),
        }
    }

    /// Claim the gate that serialises preparation of one version. Claimants
    /// take the lock, re-check the cache, and compile only if they are still
    /// the first. The claim is counted here, under the map lock.
    fn gate(&self, key: &(PolicyId, u32)) -> CompileGate {
        let mut compiling = self.compiling.lock();
        let gate = Arc::clone(compiling.entry(key.clone()).or_insert_with(|| {
            CompileGate::new(GateEntry {
                lock: Arc::new(tokio::sync::Mutex::new(())),
                users: std::sync::atomic::AtomicUsize::new(0),
            })
        }));
        gate.users.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        gate
    }

    /// Give up a claim, and drop the gate with the last one, so the map cannot
    /// grow with every version a long-lived process ever compiles. Counting and
    /// removal happen under the same lock that hands gates out, so a claim
    /// arriving now either finds this entry or creates the next one, never a
    /// gate about to be removed.
    fn release_gate(&self, key: &(PolicyId, u32), gate: &CompileGate) {
        let mut compiling = self.compiling.lock();
        let last = gate.users.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1;
        if last
            && compiling
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, gate))
        {
            compiling.remove(key);
        }
    }

    /// Pin an available version. A miss requires preparation outside the transaction.
    #[must_use]
    pub fn get(&self, id: &PolicyId, version: u32) -> Option<Arc<dyn ValidatedConfig>> {
        self.artifacts
            .lock()
            .entries
            .get(&(id.clone(), version))
            .cloned()
    }

    /// Publish an already prepared immutable artifact after commit, or after
    /// rebuilding a persisted version. Duplicate publication retains the first.
    // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-cache
    pub fn publish(&self, id: PolicyId, version: u32, artifact: Arc<dyn ValidatedConfig>) {
        let key = (id, version);
        let mut cache = self.artifacts.lock();
        if cache.entries.contains_key(&key) {
            return;
        }
        // Release evicted artifacts after unlocking: their destructors may be expensive.
        let evicted = if cache.entries.len() == self.capacity.get() {
            cache
                .insertion_order
                .pop_front()
                .and_then(|old| cache.entries.remove(&old))
        } else {
            None
        };
        cache.insertion_order.push_back(key.clone());
        cache.entries.insert(key, artifact);
        drop(cache);
        drop(evicted);
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-cache
}

/// Invoke an already-prepared artifact and enforce the shared decision boundary.
/// Suitable for the synchronous transaction callback: no preparation or I/O occurs.
///
/// # Errors
/// Missing artifacts return `PreparationRequired`; engine/invariant failures must
/// abort the owning transaction without effects.
pub fn evaluate_prepared(
    registry: &EngineRegistry,
    cache: &PolicyArtifactCache,
    context: &quota_enforcement_sdk::EvaluationContext<'_>,
    metrics: &dyn super::ports::metrics::QeMetrics,
) -> Result<
    quota_enforcement_sdk::engine::EvaluationOutcome,
    quota_enforcement_sdk::engine::EvaluationFailure,
> {
    use super::ports::metrics::EngineLabel;
    use quota_enforcement_sdk::engine::{EngineError, EvaluationFailure, EvaluationOutcome};
    let engine = registry
        .get(&context.policy.engine_id)
        .ok_or_else(|| EngineError::Internal("active policy engine is not registered".into()))?;
    let artifact = cache
        .get(&context.policy.policy_id, context.policy.version)
        .ok_or_else(|| EvaluationFailure::PreparationRequired {
            policy_id: context.policy.policy_id.clone(),
            version: context.policy.version,
        })?;
    // Registration refuses an engine without a label, so this cannot fail for
    // a registered engine; the error is defence in depth, not a hot-path branch.
    let label = EngineLabel::from_id(engine.id()).ok_or_else(|| {
        EngineError::Internal("registered engine lacks deployment telemetry label".into())
    })?;
    let started = std::time::Instant::now();
    let decision = engine.evaluate(context, artifact.as_ref());
    metrics.record_engine_evaluation(label, started.elapsed());
    let outcome = EvaluationOutcome::validate(decision?, context).map_err(|invariant| {
        metrics.record_plan_violation(label, invariant);
        // The canonical `Internal` envelope carries no detail on the wire, so
        // the sub-token lives here, on the trace and in the metric label.
        tracing::error!(
            target: "qe.engine",
            engine_id = label.as_label(),
            policy_id = %context.policy.policy_id,
            policy_version = context.policy.version,
            invariant = %invariant,
            "INVARIANT_VIOLATION: engine decision refused before any mutation"
        );
        EvaluationFailure::Invariant(invariant)
    })?;
    Ok(outcome)
}

/// Drives a storage mutation that evaluates inside its own transaction.
///
/// The callback handed to storage only pins artifacts the cache already holds:
/// compiling one is unbounded work and must never happen while the transaction
/// holds row locks. When the transaction reports that the version it selected
/// has no resident artifact it has already rolled back, so this driver compiles
/// that version outside any transaction, publishes it, and calls again. The
/// number of preparations one operation may trigger is bounded by
/// configuration, so a version that cannot be compiled cannot spin.
pub struct PreparedEvaluation<'a> {
    engines: Arc<EngineRegistry>,
    artifacts: Arc<PolicyArtifactCache>,
    metrics: Arc<dyn super::ports::metrics::QeMetrics>,
    storage: &'a dyn quota_enforcement_sdk::QuotaEnforcementStoragePluginV1,
    attempts: std::num::NonZeroU32,
}

impl<'a> PreparedEvaluation<'a> {
    /// Bind the deployment's engines, artifact cache and preparation budget.
    ///
    /// The engines, the cache and the metrics sink arrive as handles rather
    /// than borrows because the callback [`Self::evaluator`] builds outlives
    /// this value: a storage plugin moves it into the future that runs inside
    /// its transaction, and such a future may not name a caller's lifetime.
    #[must_use]
    pub fn new(
        engines: Arc<EngineRegistry>,
        artifacts: Arc<PolicyArtifactCache>,
        metrics: Arc<dyn super::ports::metrics::QeMetrics>,
        storage: &'a dyn quota_enforcement_sdk::QuotaEnforcementStoragePluginV1,
        attempts: std::num::NonZeroU32,
    ) -> Self {
        Self {
            engines,
            artifacts,
            metrics,
            storage,
            attempts,
        }
    }

    /// The synchronous callback storage invokes inside its transaction.
    #[must_use]
    pub fn evaluator(&self) -> Arc<quota_enforcement_sdk::engine::TransactionEvaluator> {
        let engines = Arc::clone(&self.engines);
        let artifacts = Arc::clone(&self.artifacts);
        let metrics = Arc::clone(&self.metrics);
        Arc::new(
            move |context: &quota_enforcement_sdk::EvaluationContext<'_>| {
                evaluate_prepared(&engines, &artifacts, context, metrics.as_ref())
            },
        )
    }

    /// Call storage, preparing what a rolled-back attempt asked for and trying
    /// again, at most `preparation_max_attempts` times.
    ///
    /// # Errors
    /// The mutation's own storage failure, or an internal error when the
    /// preparation budget is spent without the artifact becoming available.
    pub async fn run<T, Fut>(&self, mut call: impl FnMut() -> Fut) -> Result<T, DomainError>
    where
        Fut: std::future::Future<Output = Result<T, quota_enforcement_sdk::StorageError>>,
    {
        let mut prepared = 0;
        loop {
            let failure = match call().await {
                Err(quota_enforcement_sdk::StorageError::PreparationRequired {
                    policy_id,
                    version,
                }) => (policy_id, version),
                other => return other.map_err(DomainError::from),
            };
            if prepared == self.attempts.get() {
                return Err(DomainError::Internal(format!(
                    "policy {} version {} was not prepared within {} attempts",
                    failure.0, failure.1, self.attempts
                )));
            }
            prepared += 1;
            self.prepare(&failure.0, failure.1).await?;
        }
    }

    /// Compile one persisted version outside any transaction and publish it.
    ///
    /// Concurrent misses of the same version are common: a hot policy is
    /// selected by every in-flight request, and they are told to prepare it at
    /// the same moment. They queue on that version's gate, and all but the
    /// first find the artifact already published and compile nothing.
    ///
    /// The permit and the gate belong to the compilation itself, not to the
    /// caller awaiting it. A blocking job cannot be cancelled, so a caller that
    /// goes away would otherwise hand both to the next request while its own
    /// compilation kept running, and the artifact it paid for would be thrown
    /// away. Here the job publishes first and releases afterwards, so an
    /// abandoned caller still leaves the artifact behind it.
    async fn prepare(&self, policy_id: &PolicyId, version: u32) -> Result<(), DomainError> {
        let key = (policy_id.clone(), version);
        let gate = GateGuard::enter(&self.artifacts, key).await;
        if self.artifacts.get(policy_id, version).is_some() {
            // Someone else compiled it while this task waited.
            return Ok(());
        }
        let persisted = self
            .storage
            .read_policy_version(policy_id, version)
            .await?
            .ok_or_else(|| {
                DomainError::from(quota_enforcement_sdk::StorageError::UnknownPolicyVersion {
                    policy_id: policy_id.clone(),
                    version,
                })
            })?;
        let engine = Arc::clone(self.engines.get(&persisted.engine_id).ok_or_else(|| {
            DomainError::Internal(format!(
                "active policy names unregistered engine `{}`",
                persisted.engine_id
            ))
        })?);
        let permit = Arc::clone(&self.artifacts.permits)
            .acquire_owned()
            .await
            .map_err(|_| DomainError::Internal("preparation permits closed".to_owned()))?;
        let engine_id = persisted.engine_id.clone();
        let cache = Arc::clone(&self.artifacts);
        // Compilation is unbounded CPU work: parsing alone runs on a thread of
        // its own inside the engine. Keep it off the runtime's workers.
        let compiled = tokio::task::spawn_blocking(move || {
            let outcome = engine
                .validate_config(quota_enforcement_sdk::EngineValidationInput {
                    raw: &persisted.engine_config,
                    schemas: &persisted.schema_snapshot,
                })
                .map(|artifact| cache.publish(persisted.policy_id, persisted.version, artifact));
            // Published, so a waiter that takes the gate now finds the artifact.
            drop(permit);
            drop(gate);
            outcome
        })
        .await
        .map_err(|e| DomainError::Internal(format!("artifact compilation did not run: {e}")))?;
        compiled.map_err(|e| DomainError::EngineFailure {
            engine_id,
            detail: e.to_string(),
        })
    }
}

/// Construct the statically linked deployment registry before seeding policies.
/// # Errors
/// Duplicate engine identifiers fail bootstrap.
pub fn builtin_registry() -> Result<EngineRegistry, DuplicateEngine> {
    let engines: Vec<Arc<dyn QuotaResolutionEngineV1>> = vec![
        Arc::new(quota_enforcement_engine_most_restrictive::MostRestrictiveWins),
        Arc::new(quota_enforcement_engine_cel::CelEngine),
    ];
    EngineRegistry::new(engines)
}

/// Lift an evaluation failure into the domain error DESIGN section 3.3 maps:
/// `Timeout` to `DeadlineExceeded`, `CostExceeded` to `ResourceExhausted`,
/// an invariant violation and every other engine failure to `Internal`.
/// `InvalidConfig` is caught at create or update and cannot legitimately reach
/// the hot path, so here it is an internal failure too.
///
/// `PreparationRequired` is not an error a caller should ever see: the owning
/// transaction aborts, prepares the artifact outside it and retries. Lifting
/// it means the retry budget is spent, which is an internal condition.
#[must_use]
pub fn lift_evaluation(engine_id: &str, failure: EvaluationFailure) -> DomainError {
    let engine_id = engine_id.to_owned();
    match failure {
        EvaluationFailure::Engine(EngineError::Timeout) => DomainError::EngineTimeout { engine_id },
        EvaluationFailure::Engine(EngineError::CostExceeded) => {
            DomainError::EngineCostExceeded { engine_id }
        }
        EvaluationFailure::Engine(
            error @ (EngineError::TypeError(_)
            | EngineError::InvalidConfig(_)
            | EngineError::Internal(_)),
        ) => DomainError::EngineFailure {
            engine_id,
            detail: error.to_string(),
        },
        EvaluationFailure::Invariant(invariant) => DomainError::InvariantViolation {
            engine_id,
            invariant,
        },
        EvaluationFailure::PreparationRequired { policy_id, version } => {
            DomainError::EngineFailure {
                engine_id,
                detail: format!(
                    "artifact for policy {policy_id} version {version} could not be prepared"
                ),
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "engines_tests.rs"]
mod engines_tests;
