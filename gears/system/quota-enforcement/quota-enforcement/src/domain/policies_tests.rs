#![allow(clippy::expect_used)]
//! The operator policy lifecycle through the shared service: admission first,
//! engine validation before persistence, artifacts published after the write,
//! and transition telemetry that counts committed changes only.

use std::num::NonZeroUsize;
use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    MetricId, PageRequest, PolicyId, PolicyPatch, PolicyScope, PolicySpec, PolicyVersionState,
    QuotaEnforcementStoragePluginV1,
};
use serde_json::json;

use crate::domain::catalog::{
    CatalogBuilder, CatalogConfig, MetricClassifications, ProjectionContractCatalog,
};
use crate::domain::engines::{PolicyArtifactCache, builtin_registry};
use crate::domain::error::DomainError;
use crate::domain::policies::PolicySchemas;
use crate::domain::policies::schemas::CatalogPolicySchemas;
use crate::domain::ports::metrics::PolicyTransition;
use crate::domain::{Admission, Bound, Readiness, Service};
use crate::test_support::{
    DenyAllPdp, FakeContractRegistry, FakeMetricRegistry, LLM_TENANT_PROJECTION,
    LLM_USER_PROJECTION, METRIC_OTHER, METRIC_TOKENS, NoopCoordinator, PermitUnconstrainedPdp,
    RecordingMetrics, ctx, policy_limits, test_limits,
};

struct Harness {
    service: Arc<Service>,
    metrics: Arc<RecordingMetrics>,
    storage: Arc<InMemoryStorage>,
    artifacts: Arc<PolicyArtifactCache>,
    catalog: Arc<ProjectionContractCatalog>,
}

async fn harness(pdp: Arc<dyn AuthZResolverApi>) -> Harness {
    let metrics = Arc::new(RecordingMetrics::default());
    let service = Arc::new(Service::new(
        Admission::new(PolicyEnforcer::new(pdp), metrics.clone()),
        Arc::new(Readiness::new()),
        test_limits(),
        policy_limits(),
        crate::domain::service::OperationsRuntime {
            cache_entries: 16,
            cache_ttl: std::time::Duration::from_secs(5),
            preparation_max_attempts: std::num::NonZeroU32::new(3).expect("attempts"),
            leases: crate::domain::operations::LeaseLimits::default(),
        },
    ));
    let registry = Arc::new(FakeContractRegistry::llm_gateway());
    let catalog = CatalogBuilder::new(registry.as_ref(), metrics.as_ref())
        .build(&CatalogConfig {
            subject_projections: vec![
                GtsTypeId::new(LLM_USER_PROJECTION),
                GtsTypeId::new(LLM_TENANT_PROJECTION),
            ],
            resource_projections: Vec::new(),
        })
        .await
        .expect("catalogue");
    let catalog = Arc::new(catalog);
    let storage = Arc::new(InMemoryStorage::new());
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(8).expect("capacity"),
        NonZeroUsize::new(2).expect("permits"),
    ));
    service
        .bind(Bound {
            engines: Arc::new(builtin_registry().expect("engines")),
            artifacts: Arc::clone(&artifacts),
            storage: storage.clone(),
            coordinator: Arc::new(NoopCoordinator),
            catalog: Arc::clone(&catalog),
            registry,
            metric_registry: Arc::new(FakeMetricRegistry::classified()),
            classifications: Arc::new(MetricClassifications::default()),
        })
        .expect("bind");
    Harness {
        service,
        metrics,
        storage,
        artifacts,
        catalog,
    }
}

fn permitted() -> Arc<dyn AuthZResolverApi> {
    Arc::new(PermitUnconstrainedPdp)
}

fn tokens() -> MetricId {
    MetricId::parse(METRIC_TOKENS).expect("metric")
}

fn mrw(scope: PolicyScope) -> PolicySpec {
    PolicySpec {
        scope,
        engine_id: "most-restrictive-wins".to_owned(),
        engine_config: json!({}),
        timeout_ms: None,
        description: None,
        comment: Some("v1".to_owned()),
    }
}

fn patch(if_match_version: u32) -> PolicyPatch {
    PolicyPatch {
        if_match_version,
        engine_id: None,
        engine_config: None,
        timeout_ms: Some(3),
        comment: Some("bump".to_owned()),
    }
}

fn transitions(h: &Harness) -> Vec<PolicyTransition> {
    h.metrics.policy_transitions.lock().clone()
}

#[tokio::test]
async fn create_validates_with_the_engine_persists_the_actor_and_publishes_after_the_write() {
    let h = harness(permitted()).await;
    let policies = h.service.policies().expect("bound");

    let created = policies
        .create(&ctx(), mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect("create");
    assert_eq!(created.version, 1);
    assert_eq!(created.state, PolicyVersionState::Active);
    assert_eq!(
        created.created_by,
        ctx().subject_id().to_string(),
        "actor identity comes from the security context, never the caller"
    );
    assert!(
        created.schema_snapshot.environments.is_empty(),
        "most-restrictive-wins reads no metadata and persists an empty snapshot"
    );
    assert!(
        h.artifacts.get(&created.policy_id, 1).is_some(),
        "the compiled artifact is published once the write committed"
    );
    assert_eq!(transitions(&h), vec![PolicyTransition::Create]);

    // A second live policy at the same exact scope is refused by storage.
    let occupied = policies
        .create(&ctx(), mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect_err("occupied");
    assert!(matches!(occupied, DomainError::PolicyScopeOccupied { .. }));
    assert_eq!(transitions(&h).len(), 1, "a refused create counts nothing");
}

#[tokio::test]
async fn an_unknown_engine_or_a_bad_config_is_refused_before_anything_is_stored() {
    let h = harness(permitted()).await;
    let policies = h.service.policies().expect("bound");

    let unknown = policies
        .create(
            &ctx(),
            PolicySpec {
                engine_id: "starlark".to_owned(),
                ..mrw(PolicyScope::Global)
            },
        )
        .await
        .expect_err("unknown engine");
    match unknown {
        DomainError::InvalidPolicy { reason, detail, .. } => {
            assert_eq!(reason, "UNKNOWN_ENGINE");
            assert!(
                detail.contains("most-restrictive-wins") && detail.contains("cel"),
                "the rejection names the registered engines: {detail}"
            );
        }
        other => panic!("expected UNKNOWN_ENGINE, got {other:?}"),
    }

    let bad_config = policies
        .create(
            &ctx(),
            PolicySpec {
                engine_config: json!({ "weights": [1, 2] }),
                ..mrw(PolicyScope::Global)
            },
        )
        .await
        .expect_err("non-empty MRW config");
    assert!(
        matches!(
            bad_config,
            DomainError::InvalidPolicy {
                reason: "INVALID_ENGINE_CONFIG",
                ..
            }
        ),
        "{bad_config:?}"
    );

    let outside = policies
        .create(
            &ctx(),
            mrw(PolicyScope::Metric {
                metric: MetricId::parse(METRIC_OTHER).expect("metric"),
            }),
        )
        .await
        .expect_err("metric outside the catalogue");
    assert!(
        matches!(
            outside,
            DomainError::InvalidPolicy {
                reason: DomainError::PROJECTION_NOT_RESOLVABLE,
                ..
            }
        ),
        "{outside:?}"
    );

    assert!(
        h.storage
            .read_policy(&PolicyScope::Global)
            .await
            .expect("read")
            .is_none(),
        "nothing reached storage"
    );
    assert!(transitions(&h).is_empty());
}

#[tokio::test]
async fn a_cel_policy_is_type_checked_against_the_catalogue_and_keeps_its_schema_closure() {
    let h = harness(permitted()).await;
    let policies = h.service.policies().expect("bound");
    let expr = r#"{ "debit_plan": quotas
        .filter(q, size(q.arbitration.regions) > 0)
        .map(q, { "id": q.id, "amount": amount }) }"#;
    let created = policies
        .create(
            &ctx(),
            PolicySpec {
                engine_id: "cel".to_owned(),
                engine_config: json!({ "expr": expr }),
                ..mrw(PolicyScope::Metric { metric: tokens() })
            },
        )
        .await
        .expect("cel policy");
    assert_eq!(created.schema_snapshot.environments.len(), 1);
    assert_eq!(created.schema_snapshot.environments[0].metric, tokens());
    assert!(
        !created.schema_snapshot.schemas.is_empty(),
        "the closure the engine needs to rebuild offline travels with the version"
    );
    // The expression reads only `q.arbitration`, so only the constraint
    // contract is persisted; a change to the request or resource contracts
    // cannot strand this policy.
    assert_eq!(
        created.schema_snapshot.inputs,
        quota_enforcement_sdk::EnvironmentInputs {
            request: false,
            resource: false,
            arbitration: true,
        }
    );
    assert!(
        created
            .schema_snapshot
            .schemas
            .contains_key(crate::test_support::LLM_TOKEN_CONSTRAINT)
    );
    assert!(
        !created
            .schema_snapshot
            .schemas
            .contains_key(crate::test_support::LLM_TOKEN_REQUEST)
    );
    assert!(h.artifacts.get(&created.policy_id, 1).is_some());

    // A reference the constraint contract does not declare is caught at save
    // time, with a position, before persistence.
    let err = policies
        .create(
            &ctx(),
            PolicySpec {
                engine_id: "cel".to_owned(),
                engine_config: json!({ "expr": r#"{ "deny": { "reason": request.zone } }"# }),
                ..mrw(PolicyScope::Global)
            },
        )
        .await
        .expect_err("unknown property");
    match err {
        DomainError::InvalidPolicy { reason, detail, .. } => {
            assert_eq!(reason, "INVALID_ENGINE_CONFIG");
            assert!(detail.contains("absent"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn update_is_conditional_and_a_stale_version_counts_a_conflict_not_a_transition() {
    let h = harness(permitted()).await;
    let policies = h.service.policies().expect("bound");
    let created = policies
        .create(&ctx(), mrw(PolicyScope::Global))
        .await
        .expect("create");
    let id = created.policy_id.clone();

    let stale = policies
        .update(&ctx(), id.clone(), patch(7))
        .await
        .expect_err("stale");
    assert!(matches!(
        stale,
        DomainError::VersionConflict {
            expected: 7,
            actual: 1
        }
    ));
    assert_eq!(*h.metrics.policy_conflicts.lock(), 1);
    assert_eq!(transitions(&h), vec![PolicyTransition::Create]);

    let updated = policies
        .update(&ctx(), id.clone(), patch(1))
        .await
        .expect("update");
    assert_eq!(updated.version, 2);
    assert_eq!(
        updated.timeout_ms,
        Some(3),
        "the requested timeout is stored, not a clamped one"
    );
    assert!(h.artifacts.get(&id, 2).is_some());
    assert_eq!(
        transitions(&h),
        vec![PolicyTransition::Create, PolicyTransition::Update]
    );
    assert_eq!(
        policies
            .read(&ctx(), &id, None)
            .await
            .expect("active")
            .version,
        2
    );
}

#[tokio::test]
async fn rollback_and_delete_count_only_the_transition_that_actually_moved_state() {
    let h = harness(permitted()).await;
    let policies = h.service.policies().expect("bound");
    let created = policies
        .create(&ctx(), mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect("create");
    let id = created.policy_id.clone();
    policies
        .update(&ctx(), id.clone(), patch(1))
        .await
        .expect("v2");

    let back = policies
        .rollback(&ctx(), id.clone(), 1, Some("undo".to_owned()))
        .await
        .expect("rollback");
    assert_eq!((back.version, back.state), (1, PolicyVersionState::Active));
    let replay = policies
        .rollback(&ctx(), id.clone(), 1, Some("retry".to_owned()))
        .await
        .expect("replay is a no-op");
    assert_eq!(replay.version, 1);
    assert_eq!(
        transitions(&h),
        vec![
            PolicyTransition::Create,
            PolicyTransition::Update,
            PolicyTransition::Rollback
        ],
        "the replay counted nothing"
    );
    assert!(matches!(
        policies.rollback(&ctx(), id.clone(), 2, None).await,
        Err(DomainError::VersionRolledBack { version: 2, .. })
    ));
    assert!(matches!(
        policies.rollback(&ctx(), id.clone(), 9, None).await,
        Err(DomainError::UnknownPolicyVersion { version: 9, .. })
    ));

    policies
        .delete(&ctx(), id.clone(), None)
        .await
        .expect("delete");
    policies
        .delete(&ctx(), id.clone(), None)
        .await
        .expect("repeated delete is a no-op");
    assert_eq!(transitions(&h).last(), Some(&PolicyTransition::Delete));
    assert_eq!(transitions(&h).len(), 4);
    assert!(matches!(
        policies.read(&ctx(), &id, None).await,
        Err(DomainError::PolicyDeleted { .. })
    ));
    assert_eq!(
        policies
            .read(&ctx(), &id, Some(2))
            .await
            .expect("retained")
            .state,
        PolicyVersionState::RolledBack
    );
    assert!(matches!(
        policies
            .read(&ctx(), &PolicyId::new("never-created".to_owned()), None)
            .await,
        Err(DomainError::PolicyNotFound { .. })
    ));
}

#[tokio::test]
async fn the_global_policy_cannot_be_deleted_and_history_pages_are_bounded() {
    let h = harness(permitted()).await;
    let policies = h.service.policies().expect("bound");
    policies
        .create(&ctx(), mrw(PolicyScope::Global))
        .await
        .expect("global");
    assert!(matches!(
        policies.delete(&ctx(), PolicyId::global(), None).await,
        Err(DomainError::CannotDeleteSeededGlobalPolicy)
    ));

    let page = policies
        .list(&ctx(), &PolicyId::global(), PageRequest::first(10))
        .await
        .expect("history");
    assert_eq!(page.items.len(), 1);
    for limit in [0, policy_limits().authoring.list_limit + 1] {
        assert!(
            matches!(
                policies
                    .list(&ctx(), &PolicyId::global(), PageRequest::first(limit))
                    .await,
                Err(DomainError::InvalidPolicy {
                    reason: "INVALID_PAGE",
                    ..
                })
            ),
            "limit {limit} is outside the configured bound"
        );
    }
}

#[tokio::test]
async fn a_denied_operator_never_reaches_storage() {
    let h = harness(Arc::new(DenyAllPdp)).await;
    let policies = h.service.policies().expect("bound");
    let denied = policies
        .create(&ctx(), mrw(PolicyScope::Global))
        .await
        .expect_err("denied");
    assert!(
        matches!(denied, DomainError::PdpDenied { .. }),
        "{denied:?}"
    );
    assert!(
        h.storage
            .read_policy(&PolicyScope::Global)
            .await
            .expect("read")
            .is_none()
    );
    assert!(matches!(
        policies.read(&ctx(), &PolicyId::global(), None).await,
        Err(DomainError::PdpDenied { .. })
    ));
    assert!(transitions(&h).is_empty());
}

#[tokio::test]
async fn a_contract_read_through_brackets_is_persisted_and_the_version_rebuilds_after_a_restart() {
    let h = harness(permitted()).await;
    let policies = h.service.policies().expect("bound");
    // `q['arbitration']` selects the constraint contract exactly as
    // `q.arbitration` does; the persisted closure must carry it either way.
    let expr = r#"{ "debit_plan": quotas
        .filter(q, size(q['arbitration'].regions) > 0)
        .map(q, { "id": q.id, "amount": amount }) }"#;
    let created = policies
        .create(
            &ctx(),
            PolicySpec {
                engine_id: "cel".to_owned(),
                engine_config: json!({ "expr": expr }),
                ..mrw(PolicyScope::Metric { metric: tokens() })
            },
        )
        .await
        .expect("cel policy");
    assert!(
        created.schema_snapshot.inputs.arbitration,
        "a bracket read is a read"
    );
    assert!(
        created
            .schema_snapshot
            .schemas
            .contains_key(crate::test_support::LLM_TOKEN_CONSTRAINT)
    );

    // Restart: bootstrap rebuilds every active version from its own persisted
    // closure, with no access to the environment the operator saved against.
    let stored = h
        .storage
        .read_active_policies()
        .await
        .expect("read")
        .into_iter()
        .find(|version| version.policy_id == created.policy_id)
        .expect("the created policy is active");
    let schemas = CatalogPolicySchemas::new(Arc::clone(&h.catalog), policy_limits().snapshot);
    schemas
        .check_activation(&stored)
        .expect("the persisted closure still matches the catalogue");
    builtin_registry()
        .expect("engines")
        .get(&stored.engine_id)
        .expect("cel")
        .validate_config(quota_enforcement_sdk::EngineValidationInput {
            raw: &stored.engine_config,
            schemas: &stored.schema_snapshot,
        })
        .expect("the version rebuilds from what storage holds");
}
