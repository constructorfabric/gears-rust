#![allow(clippy::expect_used)]

//! Criterion benchmarks for the consumption hot path.
//!
//! What they measure: the canonical payload digest, the in-process replay
//! cache, pinning a compiled artifact, the engine's selection, and the counter
//! mutation itself against the in-memory storage double.
//!
//! What they deliberately exclude: the PDP round trip, catalogue mapping, and
//! HTTP. Those are network and policy costs that would swamp the arithmetic
//! this feature owns, and they are measured where they belong.
//!
//! These are a harness, not a gate. The NFR thresholds of
//! `cpt-cf-quota-enforcement-dod-nfr-verification` are not asserted here.
//!
//! Run with: `cargo bench -p cf-gears-quota-enforcement`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use quota_enforcement::domain::engines::{
    EngineRegistry, PolicyArtifactCache, PreparedEvaluation, builtin_registry,
};
use quota_enforcement::domain::operations::IdempotencyCache;
use quota_enforcement::domain::operations::idempotency::ReplayRecord;
use quota_enforcement::domain::ports::metrics::NoopMetrics;
use quota_enforcement_sdk::engine::EvaluationLimits;
use quota_enforcement_sdk::testing::{
    InMemoryStorage, bundle_with_global_policy, quota_draft, test_subject,
};
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, Decision, DecisionResult, EvaluatedMutation,
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, MetricId, OperationType,
    PayloadHash, QuotaEnforcementStoragePluginV1, QuotaId, SubjectRef, TenantId,
};
use serde_json::json;
use time::OffsetDateTime;
use tokio::runtime::Runtime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

const METRIC: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.tokens.v1";

fn tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(1))
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("security context")
}

fn metric() -> MetricId {
    MetricId::parse(METRIC).expect("metric")
}

fn subjects(count: usize) -> Vec<SubjectRef> {
    (0..count).map(|i| test_subject(&format!("u{i}"))).collect()
}

fn applicable(subjects: Vec<SubjectRef>) -> ApplicableQuotas {
    ApplicableQuotas {
        tenant_id: tenant(),
        subjects,
        metric: metric(),
    }
}

fn write(key: &str) -> IdempotencyWrite {
    IdempotencyWrite {
        scope: IdempotencyScope {
            tenant_id: tenant(),
            subject_key: IdempotencySubjectKey::from_bytes([1; 32]),
            operation_type: OperationType::Debit,
            key: key.to_owned(),
        },
        payload_hash: PayloadHash::from_bytes([2; 32]),
    }
}

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(100).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(100_000).expect("nonzero"),
    }
}

/// Storage seeded with the real most-restrictive-wins policy and `quotas`
/// unbounded Quotas, one per subject, so a debit never denies.
fn seeded(runtime: &Runtime, quotas: usize) -> (Arc<InMemoryStorage>, Vec<QuotaId>) {
    runtime.block_on(async {
        let storage = Arc::new(InMemoryStorage::new());
        let mut bundle = bundle_with_global_policy();
        if let Some(policy) = bundle.global_policy.as_mut() {
            // The engine this deployment actually links, so the bench measures
            // a real selection rather than a scripted answer.
            policy.engine_id.clear();
            policy.engine_id.push_str("most-restrictive-wins");
        }
        storage.bootstrap(&bundle).await.expect("bootstrap");
        let mut ids = Vec::with_capacity(quotas);
        for subject in subjects(quotas) {
            // The fixture draft carries its own tenant and metric. Both have to
            // match the request, or the applicable set is empty and this would
            // time a `NO_APPLICABLE_QUOTA` denial instead of a debit.
            let mut draft = quota_draft(subject, None);
            draft.tenant_id = tenant();
            draft.metric = metric();
            ids.push(
                storage
                    .create_quota(&ctx(), &AccessScope::allow_all(), draft, &[])
                    .await
                    .expect("quota"),
            );
        }
        (storage, ids)
    })
}

/// The deployment's engines and an artifact cache, shared across iterations so
/// the first debit compiles the policy and the rest hit a warm cache. That is
/// the steady state a hot path runs in.
fn deployment() -> (Arc<EngineRegistry>, Arc<PolicyArtifactCache>) {
    (
        Arc::new(builtin_registry().expect("engines")),
        Arc::new(PolicyArtifactCache::new(
            std::num::NonZeroUsize::new(64).expect("capacity"),
            std::num::NonZeroUsize::new(4).expect("permits"),
        )),
    )
}

/// One debit through the storage double: hash comparison, policy selection,
/// engine invocation, invariant check, and the counter mutation.
fn debit(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");
    let (engines, artifacts) = deployment();
    let metrics: Arc<dyn quota_enforcement::domain::ports::metrics::QeMetrics> =
        Arc::new(NoopMetrics);
    let null = serde_json::Value::Null;
    let security = ctx();
    let access = AccessScope::allow_all();

    let mut group = c.benchmark_group("debit");
    for (name, quota_count) in [("single_quota", 1_usize), ("ten_quota_cascade", 10)] {
        let applicable = applicable(subjects(quota_count));
        group.bench_function(name, |b| {
            // Each iteration debits into a freshly seeded store. The in-memory
            // double stages every mutation by cloning its whole state, so a
            // store that accumulated a record per iteration would make this
            // measure the growing history rather than the debit. Setup is not
            // timed.
            b.iter_batched(
                || seeded(&runtime, quota_count).0,
                |storage| {
                    let prepared = PreparedEvaluation::new(
                        Arc::clone(&engines),
                        Arc::clone(&artifacts),
                        Arc::clone(&metrics),
                        storage.as_ref(),
                        std::num::NonZeroU32::new(3).expect("attempts"),
                    );
                    let idempotency = write("k1");
                    let mutation = EvaluatedMutation {
                        applicable: &applicable,
                        amount: 1,
                        request: &null,
                        resource: &null,
                        user_projection: None,
                        limits: limits(),
                        idempotency: &idempotency,
                        authorized: AttributionDigest::from_bytes([7; 32]),
                        evaluate: prepared.evaluator(),
                    };
                    // The same driver the gear uses: it compiles and publishes
                    // the artifact on the first miss, then every later call is
                    // warm, because the cache outlives the store.
                    let outcome = runtime.block_on(
                        prepared
                            .run(|| storage.apply_debit_plan(&security, &access, &mutation, &[])),
                    );
                    let applied = outcome.expect("debit");
                    let decision = &applied.get().decision;
                    // A denial is a different, much cheaper path. Asserting the
                    // shape every iteration stops these numbers from quietly
                    // becoming a measurement of refusing work.
                    assert_eq!(decision.result, DecisionResult::Allowed);
                    assert_eq!(decision.debit_plan.len(), 1, "MRW binds exactly one Quota");
                    black_box(applied);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// The digest a debit computes before it touches storage at all.
fn payload_digest(c: &mut Criterion) {
    let payload = json!({
        "attribution": {
            "tenant_id": tenant().as_uuid(),
            "metric": METRIC,
            "subjects": [{ "kind": "user", "id": "u0" }],
            "metadata": { "region": "eu-west-1" }
        },
        "amount": 1
    });
    c.bench_function("debit/payload_digest", |b| {
        b.iter(|| black_box(PayloadHash::of_canonical(&payload).expect("serializable")));
    });
}

/// The replay a retry usually takes: a hit in the in-process cache, with no
/// storage round trip at all.
fn replay_cache_hit(c: &mut Criterion) {
    let cache = IdempotencyCache::new(4096, Duration::from_secs(5));
    let scope = write("k1").scope;
    let now = OffsetDateTime::now_utc();
    cache.insert(
        scope.clone(),
        ReplayRecord {
            payload_hash: PayloadHash::from_bytes([2; 32]),
            decision: Decision::allowed_with_plan(std::collections::BTreeMap::new()),
            expires_at: now + time::Duration::hours(1),
        },
        now,
    );
    c.bench_function("debit/replay_cache_hit", |b| {
        b.iter(|| black_box(cache.get(&scope, now)));
    });
}

criterion_group!(benches, debit, payload_digest, replay_cache_hit);
criterion_main!(benches);
