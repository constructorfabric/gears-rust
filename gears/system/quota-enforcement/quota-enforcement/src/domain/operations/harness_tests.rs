#![allow(clippy::expect_used)]
//! The consumption operations over real engines and the in-memory storage
//! double, shared by the service and lease test modules.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::{InMemoryStorage, bundle_with_global_policy};
use quota_enforcement_sdk::{
    DebitRequest, EnforcementMode, EvaluationAttribution, MetricKind, QuotaDraft,
    QuotaEnforcementStoragePluginV1, QuotaId, QuotaSource, QuotaType, SubjectClaim, SubjectRef,
};
use serde_json::json;
use toolkit_security::AccessScope;

use super::{IdempotencyCache, Operations};
use crate::domain::admission::Admission;
use crate::domain::attribution::Attribution;
use crate::domain::catalog::{
    CatalogBuilder, CatalogConfig, MetricClassifications, ProjectionContractCatalog,
};
use crate::domain::engines::{PolicyArtifactCache, builtin_registry};
use crate::domain::ports::metric_registry::{MetricDescriptor, MetricMode};
use crate::test_support::{
    FakeContractRegistry, LLM_TENANT_PROJECTION, LLM_USER_PROJECTION, METRIC_TOKENS,
    PermitTenantsPdp, RecordingMetrics, ctx, tenant,
};

pub(super) fn type_id(raw: &str) -> GtsTypeId {
    GtsTypeId::try_new(raw).expect("type id")
}

pub(super) fn evaluation_limits() -> quota_enforcement_sdk::engine::EvaluationLimits {
    quota_enforcement_sdk::engine::EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(50).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(10_000).expect("nonzero"),
    }
}

pub(super) struct Harness {
    pub(super) catalog: ProjectionContractCatalog,
    pub(super) admission: Admission,
    pub(super) storage: Arc<InMemoryStorage>,
    pub(super) classifications: MetricClassifications,
    pub(super) cache: IdempotencyCache,
    pub(super) engines: Arc<crate::domain::engines::EngineRegistry>,
    pub(super) artifacts: Arc<PolicyArtifactCache>,
    pub(super) metrics: Arc<RecordingMetrics>,
}

impl Harness {
    pub(super) async fn new() -> Self {
        Self::with_pdp(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await
    }

    /// A second replica over the same storage: its own empty replay cache, so
    /// a replay has to come from the record rather than from memory.
    pub(super) async fn new_over(other: &Self) -> Self {
        let mut replica =
            Self::with_pdp(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
        replica.storage = Arc::clone(&other.storage);
        replica
    }

    /// A harness whose storage admits at most `cap` live leases per
    /// `(tenant, metric)`.
    pub(super) async fn with_lease_cap(cap: u32) -> Self {
        let mut harness = Self::new().await;
        let storage = Arc::new(InMemoryStorage::new());
        let mut bundle = bundle_with_global_policy();
        if let Some(policy) = bundle.global_policy.as_mut() {
            policy.engine_id = "most-restrictive-wins".to_owned();
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
        let registry = FakeContractRegistry::llm_gateway();
        let catalog = CatalogBuilder::new(&registry, metrics.as_ref())
            .build(&CatalogConfig {
                subject_projections: vec![
                    type_id(LLM_USER_PROJECTION),
                    type_id(LLM_TENANT_PROJECTION),
                ],
                resource_projections: Vec::new(),
            })
            .await
            .expect("catalogue");
        let storage = Arc::new(InMemoryStorage::new());
        // The seeded policy names the engine this deployment actually links,
        // so the transaction's own evaluation runs the real MRW engine.
        let mut bundle = bundle_with_global_policy();
        if let Some(policy) = bundle.global_policy.as_mut() {
            policy.engine_id = "most-restrictive-wins".to_owned();
        }
        storage.bootstrap(&bundle).await.expect("bootstrap");
        let classifications = MetricClassifications::from_pairs([(
            quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
            MetricDescriptor {
                kind: MetricKind::Counter,
                mode,
            },
        )]);
        Self {
            catalog,
            admission: Admission::new(PolicyEnforcer::new(pdp), metrics.clone()),
            storage,
            classifications,
            cache: IdempotencyCache::new(16, std::time::Duration::from_secs(5)),
            engines: Arc::new(builtin_registry().expect("engines")),
            artifacts: Arc::new(PolicyArtifactCache::new(
                std::num::NonZeroUsize::new(8).expect("capacity"),
                std::num::NonZeroUsize::new(2).expect("permits"),
            )),
            metrics,
        }
    }

    pub(super) fn operations(&self) -> Operations<'_> {
        Operations {
            admission: &self.admission,
            attribution: Attribution::new(&self.admission, &self.catalog, self.metrics.as_ref()),
            catalog: &self.catalog,
            classifications: &self.classifications,
            storage: self.storage.as_ref(),
            engines: Arc::clone(&self.engines),
            artifacts: Arc::clone(&self.artifacts),
            idempotency: &self.cache,
            metrics: Arc::clone(&self.metrics) as _,
            evaluation: evaluation_limits(),
            preparation_max_attempts: std::num::NonZeroU32::new(3).expect("attempts"),
            leases: crate::domain::operations::LeaseLimits::default(),
            batch: crate::domain::operations::BatchLimits::default(),
        }
    }

    pub(super) async fn quota(&self, cap: Option<u64>) -> QuotaId {
        self.storage
            .create_quota(
                &ctx(),
                &AccessScope::allow_all(),
                QuotaDraft {
                    tenant_id: tenant(),
                    subject: SubjectRef {
                        projection_type: type_id(LLM_USER_PROJECTION),
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
                    metadata: serde_json::Map::new(),
                    source: QuotaSource::Operator,
                    constraint_contract: quota_enforcement_sdk::ContractRef {
                        type_id: type_id(crate::test_support::LLM_TOKEN_CONSTRAINT),
                        version: 1,
                    },
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
        metric: METRIC_TOKENS.to_owned(),
        subjects: vec![SubjectClaim {
            kind: quota_enforcement_sdk::SCOPE_USER.to_owned(),
            id: "u-1".to_owned(),
        }],
        metadata: Some(
            json!({ "region": "eu-west-1" })
                .as_object()
                .cloned()
                .expect("object"),
        ),
        resource: None,
    }
}

pub(super) fn debit(amount: i64, key: &str) -> DebitRequest {
    DebitRequest {
        attribution: attribution(),
        amount,
        idempotency_key: key.to_owned(),
    }
}
