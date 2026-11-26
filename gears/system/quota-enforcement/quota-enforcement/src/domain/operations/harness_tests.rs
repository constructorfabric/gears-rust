#![allow(clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::{InMemoryStorage, bundle_with_global_policy};
use quota_enforcement_sdk::{
    DebitRequest, EnforcementMode, EvaluationAttribution, MetricRef, QuotaDraft,
    QuotaEnforcementStoragePluginV1, QuotaId, QuotaSource, QuotaType, SubjectClaim, SubjectRef,
};
use serde_json::json;
use toolkit_security::{AccessScope, PlatformIdentity};
use types_registry_sdk::testing_platform::MockTypesRegistry;

use super::{IdempotencyCache, Operations};
use crate::domain::admission::{Admission, Gate};
use crate::domain::catalog::{MetricCatalog, MetricMode};
use crate::domain::engines::{PolicyArtifactCache, linked_registry};
use crate::test_support::{
    METRIC_TOKENS, PermitTenantsPdp, RecordingMetrics, USER_SUBJECT_TYPE, actor, caller,
    catalog_over, metric_type_document, platform_registry, tenant,
};

pub(super) fn type_id(raw: &str) -> GtsTypeId {
    GtsTypeId::try_new(raw).expect("type id")
}

pub(super) fn evaluation_limits() -> quota_enforcement_sdk::engine::EvaluationLimits {
    quota_enforcement_sdk::engine::EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(60_000).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(10_000).expect("nonzero"),
    }
}

pub(super) struct Harness {
    pub(super) catalog: Arc<MetricCatalog>,
    pub(super) registry: Arc<MockTypesRegistry>,
    pub(super) admission: Admission,
    pub(super) storage: Arc<InMemoryStorage>,
    pub(super) cache: IdempotencyCache,
    pub(super) engines: Arc<crate::domain::engines::EngineRegistry>,
    pub(super) artifacts: Arc<PolicyArtifactCache>,
    pub(super) metrics: Arc<RecordingMetrics>,
}

impl Harness {
    pub(super) async fn new() -> Self {
        Self::with_pdp(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await
    }

    // A second replica over the same storage: its own empty replay cache, so
    // a replay has to come from the record rather than from memory.
    pub(super) async fn new_over(other: &Self) -> Self {
        let mut replica =
            Self::with_pdp(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
        replica.storage = Arc::clone(&other.storage);
        replica
    }

    pub(super) async fn with_lease_cap(cap: u32) -> Self {
        let mut harness = Self::new().await;
        let storage = Arc::new(InMemoryStorage::new());
        let mut bundle = bundle_with_global_policy();
        if let Some(policy) = bundle.global_policy.as_mut() {
            policy.engine_id = "most-restrictive-wins".to_owned();
            policy.timeout_ms = Some(60_000);
        }
        bundle.config_defaults.max_active_leases = cap;
        storage.bootstrap(&bundle).await.expect("bootstrap");
        harness.storage = storage;
        harness
    }

    pub(super) async fn with_pdp(pdp: Arc<dyn AuthZResolverApi>) -> Self {
        Self::over(pdp, MetricMode::QuotaGated).await
    }

    pub(super) async fn over(pdp: Arc<dyn AuthZResolverApi>, mode: MetricMode) -> Self {
        let metrics = Arc::new(RecordingMetrics::default());
        let registry = platform_registry();
        if mode == MetricMode::Direct {
            registry.seed(
                METRIC_TOKENS,
                metric_type_document(
                    METRIC_TOKENS,
                    json!({
                        "kind": "counter",
                        "admitted_subjects": [USER_SUBJECT_TYPE],
                        "enforcement": "direct",
                    }),
                    json!({ "region": { "type": "string" }, "model": { "type": "string" } }),
                ),
            );
        }
        let catalog = catalog_over(&registry, metrics.clone());
        let storage = Arc::new(InMemoryStorage::new());
        // The seeded policy names the engine this deployment actually links,
        // so the transaction's own evaluation runs the real MRW engine.
        let mut bundle = bundle_with_global_policy();
        if let Some(policy) = bundle.global_policy.as_mut() {
            policy.engine_id = "most-restrictive-wins".to_owned();
            // Generous, so a slow runner hits the cost limit and never the 5 ms default.
            policy.timeout_ms = Some(60_000);
        }
        storage.bootstrap(&bundle).await.expect("bootstrap");
        Self {
            catalog,
            registry,
            admission: Admission::new(PolicyEnforcer::new(pdp), metrics.clone()),
            storage,
            cache: IdempotencyCache::new(16, std::time::Duration::from_secs(5)),
            engines: Arc::new(linked_registry().expect("engines")),
            artifacts: Arc::new(PolicyArtifactCache::new(
                std::num::NonZeroUsize::new(8).expect("capacity"),
                std::num::NonZeroUsize::new(2).expect("permits"),
            )),
            metrics,
        }
    }

    pub(super) fn operations(&self) -> Operations<'_> {
        self.built(self.admission.pdp(caller()))
    }

    pub(super) fn trusted(&self, caller: Option<PlatformIdentity>) -> Operations<'_> {
        self.built(self.admission.trusted(caller))
    }

    fn built<'a>(&'a self, admission: Gate<'a>) -> Operations<'a> {
        Operations {
            admission,
            catalog: &self.catalog,
            storage: self.storage.as_ref(),
            engines: Arc::clone(&self.engines),
            artifacts: Arc::clone(&self.artifacts),
            idempotency: &self.cache,
            metrics: Arc::clone(&self.metrics) as _,
            evaluation: evaluation_limits(),
            preparation_max_attempts: std::num::NonZeroU32::new(3).expect("attempts"),
            leases: crate::domain::operations::LeaseLimits::default(),
            batch: crate::domain::operations::BatchLimits::default(),
            snapshot: crate::domain::operations::SnapshotLimits::default(),
        }
    }

    pub(super) async fn quota(&self, cap: Option<u64>) -> QuotaId {
        self.quota_with(cap, serde_json::Map::new()).await
    }

    /// A Quota with the conditions `metadata`.
    pub(super) async fn quota_with(
        &self,
        cap: Option<u64>,
        metadata: serde_json::Map<String, serde_json::Value>,
    ) -> QuotaId {
        self.storage
            .create_quota(
                &actor(),
                &AccessScope::allow_all(),
                QuotaDraft {
                    tenant_id: tenant(),
                    subject: SubjectRef {
                        subject_type: type_id(USER_SUBJECT_TYPE),
                        subject_id: "u-1".to_owned(),
                    },
                    metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
                    quota_type: QuotaType::Allocation,
                    period: None,
                    enforcement_mode: EnforcementMode::Hard,
                    cap,
                    notification_thresholds: Vec::new(),
                    validity_window: None,
                    fail_open_hint: false,
                    metadata,
                    source: QuotaSource::Operator,
                },
                &[],
            )
            .await
            .expect("quota")
    }

    pub(super) fn consumed(&self, id: QuotaId) -> u64 {
        self.storage.consumed(id)
    }
}

pub(super) fn attribution() -> EvaluationAttribution {
    EvaluationAttribution {
        tenant_id: tenant(),
        subjects: vec![SubjectClaim {
            subject_type: USER_SUBJECT_TYPE.to_owned(),
            id: "u-1".to_owned(),
        }],
        metric: MetricRef {
            metric_type: METRIC_TOKENS.to_owned(),
            id: None,
            metadata: Some(
                json!({ "region": "eu-west-1" })
                    .as_object()
                    .cloned()
                    .expect("object"),
            ),
        },
    }
}

pub(super) fn debit(amount: i64, key: &str) -> DebitRequest {
    DebitRequest {
        attribution: attribution(),
        amount,
        idempotency_key: key.to_owned(),
    }
}
