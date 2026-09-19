#![allow(clippy::expect_used)]

use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use quota_enforcement_sdk::engine::{DebitPlanInvariant, EngineError, EvaluationFailure};
use quota_enforcement_sdk::{
    Decision, DecisionResult, EngineConfigError, EngineValidationInput, EvaluationBudget,
    EvaluationContext, EvaluationQuota, MetricId, PolicyId, PolicySchemaSnapshot, PolicyScope,
    PolicyVersion, PolicyVersionState, QuotaDebitPlan, QuotaId, QuotaResolutionEngineV1,
    QuotaScopeTier, QuotaSnapshot, ValidatedConfig,
};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{
    DuplicateEngine, EngineRegistry, PolicyArtifactCache, builtin_registry, evaluate_prepared,
    lift_evaluation,
};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::EngineLabel;
use crate::test_support::{LLM_USER_PROJECTION, METRIC_OTHER, METRIC_TOKENS, RecordingMetrics};

fn metric() -> MetricId {
    MetricId::parse(METRIC_TOKENS).expect("metric")
}

fn policy(engine_id: &str) -> PolicyVersion {
    PolicyVersion {
        schema_snapshot: PolicySchemaSnapshot::default(),
        policy_id: PolicyId::global(),
        version: 1,
        scope: PolicyScope::Global,
        engine_id: engine_id.to_owned(),
        engine_config: json!({}),
        timeout_ms: None,
        description: None,
        state: PolicyVersionState::Active,
        created_at: OffsetDateTime::UNIX_EPOCH,
        created_by: "operator".to_owned(),
        comment: None,
    }
}

fn snapshot(cap: u64) -> QuotaSnapshot {
    QuotaSnapshot {
        quota_id: QuotaId::generate(),
        subject: serde_json::from_value(json!({
            "projection_type": LLM_USER_PROJECTION,
            "subject_id": "alice"
        }))
        .expect("subject"),
        metric: metric(),
        quota_type: quota_enforcement_sdk::QuotaType::Consumption,
        enforcement_mode: quota_enforcement_sdk::EnforcementMode::Hard,
        cap: Some(cap),
        consumed: 0,
        remaining: Some(cap),
        period: None,
        metadata: serde_json::Map::new(),
        validity_window: None,
        currently_within_window: true,
    }
}

fn budget() -> EvaluationBudget {
    let thousand = NonZeroU64::new(1000).expect("nonzero");
    EvaluationBudget::new(Some(1000), thousand, thousand).expect("budget")
}

/// An engine that reports a registered id but debits a Quota nobody asked
/// about: what the invariant boundary exists to catch.
struct RogueEngine;

struct NoConfig;

impl ValidatedConfig for NoConfig {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl QuotaResolutionEngineV1 for RogueEngine {
    fn id(&self) -> &'static str {
        "cel"
    }

    fn validate_config(
        &self,
        _input: EngineValidationInput<'_>,
    ) -> Result<Arc<dyn ValidatedConfig>, EngineConfigError> {
        Ok(Arc::new(NoConfig))
    }

    fn evaluate(
        &self,
        context: &EvaluationContext<'_>,
        _config: &dyn ValidatedConfig,
    ) -> Result<Decision, EngineError> {
        Ok(Decision {
            result: DecisionResult::Allowed,
            debit_plan: std::iter::once((
                QuotaId::generate(),
                QuotaDebitPlan {
                    amount: context.amount,
                },
            ))
            .collect(),
            diagnostics: std::collections::BTreeMap::new(),
        })
    }
}

#[test]
fn the_builtin_registry_links_both_engines_and_refuses_a_duplicate_id() {
    let registry = builtin_registry().expect("built-ins");
    assert_eq!(
        registry.ids().collect::<Vec<_>>(),
        vec!["cel", "most-restrictive-wins"],
        "deterministic order for UNKNOWN_ENGINE details"
    );
    assert!(registry.get("most-restrictive-wins").is_some());
    assert!(registry.get("starlark").is_none());

    let engines: Vec<Arc<dyn QuotaResolutionEngineV1>> =
        vec![Arc::new(RogueEngine), Arc::new(RogueEngine)];
    assert!(
        matches!(
            EngineRegistry::new(engines),
            Err(DuplicateEngine { engine_id: "cel" })
        ),
        "a duplicate registration fails bootstrap instead of overwriting"
    );
}

#[test]
fn the_artifact_cache_is_bounded_fifo_and_keeps_the_first_publication() {
    let cache = PolicyArtifactCache::new(
        NonZeroUsize::new(2).expect("capacity"),
        NonZeroUsize::new(2).expect("permits"),
    );
    let a = PolicyId::new("a".to_owned());
    let b = PolicyId::new("b".to_owned());
    cache.publish(a.clone(), 1, Arc::new(NoConfig));
    cache.publish(b.clone(), 1, Arc::new(NoConfig));
    assert!(cache.get(&a, 1).is_some() && cache.get(&b, 1).is_some());

    // A third entry evicts the oldest; the same key published twice keeps the
    // first artifact rather than swapping it under a running evaluation.
    cache.publish(a.clone(), 2, Arc::new(NoConfig));
    assert!(cache.get(&a, 1).is_none(), "oldest evicted");
    assert!(cache.get(&a, 2).is_some() && cache.get(&b, 1).is_some());
    let first = cache.get(&b, 1).expect("present");
    cache.publish(b.clone(), 1, Arc::new(NoConfig));
    assert!(Arc::ptr_eq(&first, &cache.get(&b, 1).expect("present")));
}

#[test]
fn a_prepared_evaluation_records_latency_and_a_miss_asks_for_preparation() {
    let registry = builtin_registry().expect("built-ins");
    let cache = PolicyArtifactCache::new(
        NonZeroUsize::new(8).expect("capacity"),
        NonZeroUsize::new(2).expect("permits"),
    );
    let metrics = RecordingMetrics::default();
    let policy = policy("most-restrictive-wins");
    let quota = snapshot(100);
    let quotas = [EvaluationQuota {
        snapshot: &quota,
        tier: QuotaScopeTier::User,
        arbitration: &Value::Null,
    }];
    let context = EvaluationContext {
        policy: &policy,
        metric: &metric(),
        amount: 7,
        time: OffsetDateTime::UNIX_EPOCH,
        quotas: &quotas,
        request: &Value::Null,
        resource: &Value::Null,
        budget: budget(),
    };

    // Nothing published yet: the caller must prepare outside the transaction.
    let miss = evaluate_prepared(&registry, &cache, &context, &metrics).expect_err("miss");
    assert!(
        matches!(
            miss,
            EvaluationFailure::PreparationRequired { version: 1, .. }
        ),
        "{miss:?}"
    );
    assert!(metrics.engine_evaluations.lock().is_empty());

    let artifact = registry
        .get("most-restrictive-wins")
        .expect("registered")
        .validate_config(EngineValidationInput {
            raw: &json!({}),
            schemas: &PolicySchemaSnapshot::default(),
        })
        .expect("empty config");
    cache.publish(PolicyId::global(), 1, artifact);
    let outcome = evaluate_prepared(&registry, &cache, &context, &metrics).expect("prepared");
    assert_eq!(
        outcome.decision().debit_plan.get(&quota.quota_id),
        Some(&QuotaDebitPlan { amount: 7 })
    );
    assert_eq!(
        outcome.decision().diagnostics["engine_id"],
        json!("most-restrictive-wins")
    );
    let evaluations = metrics.engine_evaluations.lock();
    assert_eq!(evaluations.len(), 1);
    assert_eq!(evaluations[0].0, EngineLabel::MostRestrictiveWins);
}

#[test]
fn a_decision_outside_the_applicable_set_is_refused_and_counted() {
    let engines: Vec<Arc<dyn QuotaResolutionEngineV1>> = vec![Arc::new(RogueEngine)];
    let registry = EngineRegistry::new(engines).expect("rogue registry");
    let cache = PolicyArtifactCache::new(
        NonZeroUsize::new(8).expect("capacity"),
        NonZeroUsize::new(2).expect("permits"),
    );
    cache.publish(PolicyId::global(), 1, Arc::new(NoConfig));
    let metrics = RecordingMetrics::default();
    let policy = policy("cel");
    let quota = snapshot(100);
    let quotas = [EvaluationQuota {
        snapshot: &quota,
        tier: QuotaScopeTier::User,
        arbitration: &Value::Null,
    }];
    let context = EvaluationContext {
        policy: &policy,
        metric: &metric(),
        amount: 7,
        time: OffsetDateTime::UNIX_EPOCH,
        quotas: &quotas,
        request: &Value::Null,
        resource: &Value::Null,
        budget: budget(),
    };
    let failure = evaluate_prepared(&registry, &cache, &context, &metrics).expect_err("rogue");
    assert!(matches!(
        failure,
        EvaluationFailure::Invariant(DebitPlanInvariant::QuotaIdOutsideApplicableSet)
    ));
    assert_eq!(
        metrics.plan_violations.lock().as_slice(),
        &[(
            EngineLabel::Cel,
            DebitPlanInvariant::QuotaIdOutsideApplicableSet
        )]
    );
    assert_eq!(
        metrics.engine_evaluations.lock().len(),
        1,
        "latency is observed on failure too"
    );

    // And the lift lands it on the documented canonical class.
    assert_eq!(
        lift_evaluation("cel", failure),
        DomainError::InvariantViolation {
            engine_id: "cel".to_owned(),
            invariant: DebitPlanInvariant::QuotaIdOutsideApplicableSet,
        }
    );
}

#[test]
fn every_engine_failure_lifts_to_its_documented_domain_error() {
    let lift = |failure| lift_evaluation("cel", failure);
    assert_eq!(
        lift(EngineError::Timeout.into()),
        DomainError::EngineTimeout {
            engine_id: "cel".to_owned()
        }
    );
    assert_eq!(
        lift(EngineError::CostExceeded.into()),
        DomainError::EngineCostExceeded {
            engine_id: "cel".to_owned()
        }
    );
    for error in [
        EngineError::TypeError("x".to_owned()),
        EngineError::InvalidConfig("x".to_owned()),
        EngineError::Internal("x".to_owned()),
    ] {
        assert!(
            matches!(
                lift(error.into()),
                DomainError::EngineFailure { ref engine_id, .. } if engine_id == "cel"
            ),
            "type, config and internal failures are all opaque internals"
        );
    }
    assert!(matches!(
        lift(EvaluationFailure::PreparationRequired {
            policy_id: PolicyId::global(),
            version: 3,
        }),
        DomainError::EngineFailure { ref detail, .. } if detail.contains("version 3")
    ));
}

/// A persisted global policy naming an engine this deployment registers, so
/// the driver has something it can actually compile.
async fn seed_global(storage: &quota_enforcement_sdk::testing::InMemoryStorage) -> PolicyVersion {
    seed_policy(storage, PolicyScope::Global, "most-restrictive-wins").await
}

/// A persisted policy at one scope, naming whichever engine the test registers.
async fn seed_policy(
    storage: &quota_enforcement_sdk::testing::InMemoryStorage,
    scope: PolicyScope,
    engine_id: &str,
) -> PolicyVersion {
    use quota_enforcement_sdk::QuotaEnforcementStoragePluginV1;
    storage
        .create_policy(
            &crate::test_support::ctx(),
            quota_enforcement_sdk::PolicyDraft {
                schema_snapshot: PolicySchemaSnapshot::default(),
                scope,
                engine_id: engine_id.to_owned(),
                engine_config: json!({}),
                timeout_ms: None,
                description: None,
                comment: None,
                created_by: "operator".to_owned(),
            },
            &[],
        )
        .await
        .expect("seed the policy")
}

#[tokio::test]
async fn a_missing_artifact_is_prepared_outside_the_transaction_and_the_mutation_retried() {
    use quota_enforcement_sdk::testing::InMemoryStorage;

    let storage = InMemoryStorage::new();
    let seeded = seed_global(&storage).await;
    let engines = builtin_registry().expect("engines");
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(2).expect("permits"),
    ));
    let metrics = RecordingMetrics::default();
    let driver = super::PreparedEvaluation::new(
        Arc::new(engines),
        Arc::clone(&artifacts),
        Arc::new(metrics),
        &storage,
        std::num::NonZeroU32::new(2).expect("attempts"),
    );

    // A storage call that asks for a preparation once, then succeeds.
    let attempts = std::cell::Cell::new(0_u32);
    let outcome: Result<&str, DomainError> = driver
        .run(|| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            async move {
                if attempt == 0 {
                    return Err(quota_enforcement_sdk::StorageError::PreparationRequired {
                        policy_id: PolicyId::global(),
                        version: 1,
                    });
                }
                Ok("committed")
            }
        })
        .await;
    assert_eq!(outcome.expect("the retry commits"), "committed");
    assert_eq!(attempts.get(), 2, "one preparation, one retry");
    assert!(
        artifacts.get(&seeded.policy_id, 1).is_some(),
        "the version storage named was compiled and published"
    );
}

#[tokio::test]
async fn a_version_that_never_becomes_available_exhausts_the_bounded_retry_budget() {
    use quota_enforcement_sdk::testing::InMemoryStorage;

    let storage = InMemoryStorage::new();
    seed_global(&storage).await;
    let engines = builtin_registry().expect("engines");
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(2).expect("permits"),
    ));
    let metrics = RecordingMetrics::default();
    let driver = super::PreparedEvaluation::new(
        Arc::new(engines),
        Arc::clone(&artifacts),
        Arc::new(metrics),
        &storage,
        std::num::NonZeroU32::new(2).expect("attempts"),
    );
    let attempts = std::cell::Cell::new(0_u32);
    let err = driver
        .run(|| {
            attempts.set(attempts.get() + 1);
            async {
                Err::<(), _>(quota_enforcement_sdk::StorageError::PreparationRequired {
                    policy_id: PolicyId::global(),
                    version: 1,
                })
            }
        })
        .await
        .expect_err("a transaction that always asks cannot spin");
    assert!(
        matches!(err, DomainError::Internal(ref detail) if detail.contains("within 2 attempts")),
        "{err:?}"
    );
    assert_eq!(attempts.get(), 3, "two preparations, then refusal");
}

/// Counts compilations and records the highest number that ever ran at once.
struct CountingEngine {
    compiles: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    dwell: Duration,
}

impl QuotaResolutionEngineV1 for CountingEngine {
    fn id(&self) -> &'static str {
        "cel"
    }

    fn validate_config(
        &self,
        _input: EngineValidationInput<'_>,
    ) -> Result<Arc<dyn ValidatedConfig>, EngineConfigError> {
        let running = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(running, Ordering::SeqCst);
        std::thread::sleep(self.dwell);
        self.compiles.fetch_add(1, Ordering::SeqCst);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(Arc::new(NoConfig))
    }

    fn evaluate(
        &self,
        _context: &EvaluationContext<'_>,
        _config: &dyn ValidatedConfig,
    ) -> Result<Decision, EngineError> {
        Err(EngineError::Internal(
            "preparation tests never evaluate".into(),
        ))
    }
}

fn counting(
    compiles: &Arc<AtomicUsize>,
    peak: &Arc<AtomicUsize>,
) -> Result<EngineRegistry, DuplicateEngine> {
    EngineRegistry::new(vec![Arc::new(CountingEngine {
        compiles: Arc::clone(compiles),
        in_flight: Arc::new(AtomicUsize::new(0)),
        peak: Arc::clone(peak),
        dwell: Duration::from_millis(20),
    }) as Arc<dyn QuotaResolutionEngineV1>])
}

#[tokio::test]
async fn concurrent_misses_of_one_version_compile_it_once() {
    use quota_enforcement_sdk::testing::InMemoryStorage;

    let storage = InMemoryStorage::new();
    let seeded = seed_policy(&storage, PolicyScope::Global, "cel").await;
    let compiles = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let engines = counting(&compiles, &peak).expect("registry");
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(4).expect("permits"),
    ));
    let metrics = RecordingMetrics::default();
    let driver = super::PreparedEvaluation::new(
        Arc::new(engines),
        Arc::clone(&artifacts),
        Arc::new(metrics),
        &storage,
        NonZeroU32::new(2).expect("attempts"),
    );

    // Four callers are told, at the same moment, that the version they selected
    // has no resident artifact. Only the first through the gate compiles it.
    let driver = &driver;
    let miss = || async move {
        let pending = std::cell::Cell::new(true);
        driver
            .run(|| {
                let first = pending.replace(false);
                async move {
                    if first {
                        return Err(quota_enforcement_sdk::StorageError::PreparationRequired {
                            policy_id: PolicyId::global(),
                            version: 1,
                        });
                    }
                    Ok(())
                }
            })
            .await
    };
    let served = tokio::join!(miss(), miss(), miss(), miss());
    for outcome in [served.0, served.1, served.2, served.3] {
        outcome.expect("every caller is served");
    }
    assert_eq!(
        compiles.load(Ordering::SeqCst),
        1,
        "four misses of one version compile it once"
    );
    assert!(
        artifacts.get(&seeded.policy_id, 1).is_some(),
        "and the artifact they all waited for is published"
    );
}

#[tokio::test]
async fn compiling_distinct_versions_stays_within_the_configured_bound() {
    use quota_enforcement_sdk::testing::InMemoryStorage;

    let storage = InMemoryStorage::new();
    let one = seed_policy(&storage, PolicyScope::Global, "cel").await;
    let two = seed_policy(&storage, PolicyScope::Metric { metric: metric() }, "cel").await;
    let three = seed_policy(
        &storage,
        PolicyScope::Metric {
            metric: MetricId::parse(METRIC_OTHER).expect("metric"),
        },
        "cel",
    )
    .await;
    let compiles = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let engines = counting(&compiles, &peak).expect("registry");
    // One permit: three different versions cannot compile side by side.
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(1).expect("permits"),
    ));
    let metrics = RecordingMetrics::default();
    let driver = super::PreparedEvaluation::new(
        Arc::new(engines),
        Arc::clone(&artifacts),
        Arc::new(metrics),
        &storage,
        NonZeroU32::new(2).expect("attempts"),
    );

    let driver = &driver;
    let ask = |policy_id: PolicyId| async move {
        let pending = std::cell::Cell::new(true);
        driver
            .run(|| {
                let first = pending.replace(false);
                let policy_id = policy_id.clone();
                async move {
                    if first {
                        return Err(quota_enforcement_sdk::StorageError::PreparationRequired {
                            policy_id,
                            version: 1,
                        });
                    }
                    Ok(())
                }
            })
            .await
    };
    let served = tokio::join!(ask(one.policy_id), ask(two.policy_id), ask(three.policy_id));
    for outcome in [served.0, served.1, served.2] {
        outcome.expect("every caller is served");
    }
    assert_eq!(
        compiles.load(Ordering::SeqCst),
        3,
        "three distinct versions"
    );
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "the permit count bounds how many compile at once"
    );
}

/// An engine that reports when a compilation starts, so a test can abandon its
/// caller while the work is genuinely running.
struct DwellingEngine {
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    dwell: Duration,
}

impl QuotaResolutionEngineV1 for DwellingEngine {
    fn id(&self) -> &'static str {
        "cel"
    }

    fn validate_config(
        &self,
        _input: EngineValidationInput<'_>,
    ) -> Result<Arc<dyn ValidatedConfig>, EngineConfigError> {
        let running = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(running, Ordering::SeqCst);
        self.entered.notify_one();
        std::thread::sleep(self.dwell);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(Arc::new(NoConfig))
    }

    fn evaluate(
        &self,
        _context: &EvaluationContext<'_>,
        _config: &dyn ValidatedConfig,
    ) -> Result<Decision, EngineError> {
        Err(EngineError::Internal(
            "preparation tests never evaluate".into(),
        ))
    }
}

#[tokio::test]
async fn a_finished_preparation_leaves_no_gate_behind() {
    use quota_enforcement_sdk::testing::InMemoryStorage;

    let storage = InMemoryStorage::new();
    let seeded = seed_policy(&storage, PolicyScope::Global, "cel").await;
    let compiles = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let engines = counting(&compiles, &peak).expect("registry");
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(4).expect("permits"),
    ));
    let metrics = RecordingMetrics::default();
    let driver = super::PreparedEvaluation::new(
        Arc::new(engines),
        Arc::clone(&artifacts),
        Arc::new(metrics),
        &storage,
        NonZeroU32::new(2).expect("attempts"),
    );
    let pending = std::cell::Cell::new(true);
    driver
        .run(|| {
            let first = pending.replace(false);
            async move {
                if first {
                    return Err(quota_enforcement_sdk::StorageError::PreparationRequired {
                        policy_id: PolicyId::global(),
                        version: 1,
                    });
                }
                Ok(())
            }
        })
        .await
        .expect("prepared");
    assert!(artifacts.get(&seeded.policy_id, 1).is_some(), "published");
    assert_eq!(
        artifacts.compiling.lock().len(),
        0,
        "a version's gate is dropped once its preparation finishes, so the map \
         does not grow with every version this process compiles"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abandoned_caller_keeps_its_permit_until_the_compilation_publishes() {
    use quota_enforcement_sdk::testing::InMemoryStorage;

    let storage = InMemoryStorage::new();
    let one = seed_policy(&storage, PolicyScope::Global, "cel").await;
    let two = seed_policy(&storage, PolicyScope::Metric { metric: metric() }, "cel").await;
    let peak = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let engines = EngineRegistry::new(vec![Arc::new(DwellingEngine {
        in_flight: Arc::new(AtomicUsize::new(0)),
        peak: Arc::clone(&peak),
        entered: Arc::clone(&entered),
        dwell: Duration::from_millis(400),
    }) as Arc<dyn QuotaResolutionEngineV1>])
    .expect("registry");
    // One permit: nothing may compile beside a compilation already running.
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(1).expect("permits"),
    ));
    let metrics = RecordingMetrics::default();
    let driver = super::PreparedEvaluation::new(
        Arc::new(engines),
        Arc::clone(&artifacts),
        Arc::new(metrics),
        &storage,
        NonZeroU32::new(2).expect("attempts"),
    );

    let driver = &driver;
    let ask = |policy_id: PolicyId| {
        let pending = std::cell::Cell::new(true);
        async move {
            driver
                .run(|| {
                    let first = pending.replace(false);
                    let policy_id = policy_id.clone();
                    async move {
                        if first {
                            return Err(quota_enforcement_sdk::StorageError::PreparationRequired {
                                policy_id,
                                version: 1,
                            });
                        }
                        Ok(())
                    }
                })
                .await
        }
    };

    // Abandon the first caller the way a dropped request future would, once its
    // compilation is genuinely running. A blocking job cannot be cancelled, so
    // the coordination must stay with the job rather than with the caller.
    let mut first = Box::pin(ask(one.policy_id.clone()));
    tokio::select! {
        _ = &mut first => panic!("the compilation dwells; it cannot have finished"),
        () = entered.notified() => {}
    }
    drop(first);

    // A different version asks to be prepared while that compilation runs on.
    ask(two.policy_id).await.expect("second preparation");
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "an abandoned caller must not hand its permit to the next one"
    );
    assert!(
        artifacts.get(&one.policy_id, 1).is_some(),
        "and the work it paid for is published rather than thrown away"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_waiter_cancelled_before_it_acquires_the_gate_still_releases_it() {
    use quota_enforcement_sdk::testing::InMemoryStorage;

    let storage = InMemoryStorage::new();
    let seeded = seed_policy(&storage, PolicyScope::Global, "cel").await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let engines = EngineRegistry::new(vec![Arc::new(DwellingEngine {
        in_flight: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        entered: Arc::clone(&entered),
        dwell: Duration::from_millis(400),
    }) as Arc<dyn QuotaResolutionEngineV1>])
    .expect("registry");
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(4).expect("capacity"),
        NonZeroUsize::new(4).expect("permits"),
    ));
    let metrics = RecordingMetrics::default();
    let driver = super::PreparedEvaluation::new(
        Arc::new(engines),
        Arc::clone(&artifacts),
        Arc::new(metrics),
        &storage,
        NonZeroU32::new(2).expect("attempts"),
    );

    let driver = &driver;
    let ask = || {
        let pending = std::cell::Cell::new(true);
        async move {
            driver
                .run(|| {
                    let first = pending.replace(false);
                    async move {
                        if first {
                            return Err(quota_enforcement_sdk::StorageError::PreparationRequired {
                                policy_id: PolicyId::global(),
                                version: 1,
                            });
                        }
                        Ok(())
                    }
                })
                .await
        }
    };

    // One caller holds the gate and compiles.
    let mut compiling = Box::pin(ask());
    tokio::select! {
        _ = &mut compiling => panic!("the compilation dwells; it cannot have finished"),
        () = entered.notified() => {}
    }

    // A second caller queues behind it and is abandoned before it ever gets
    // the gate, so it never owns the lock it would otherwise clean up with.
    {
        let mut queued = Box::pin(ask());
        tokio::select! {
            _ = &mut queued => panic!("the gate is held; it cannot have finished"),
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    compiling.await.expect("the first caller prepares");
    assert!(artifacts.get(&seeded.policy_id, 1).is_some(), "published");
    assert_eq!(
        artifacts.compiling.lock().len(),
        0,
        "the last claim removes the gate, even when the claim before it was \
         cancelled while still queued"
    );
}
